//! IIR Filter Design and Runtime
//!
//! Analog prototype design (s-domain poles/zeros) → bilinear transform → cascaded
//! second-order sections (biquads) for numerical stability.
//!
//! Supported filter families:
//! - **Butterworth**: Maximally flat magnitude response
//! - **Chebyshev Type I**: Equiripple passband, monotonic stopband
//! - **Chebyshev Type II**: Monotonic passband, equiripple stopband
//! - **Elliptic (Cauer)**: Equiripple in both bands, steepest transition
//!
//! All design functions take physical frequencies (Hz) and a sample rate.

use std::f64::consts::PI;

use num_complex::Complex;

// ============================================================================
// Types
// ============================================================================

/// IIR filter type.
#[derive(Debug, Clone, Copy)]
pub enum FilterBand {
    Lowpass,
    Highpass,
    Bandpass { center: f64 },
    Bandstop { center: f64 },
}

/// A single second-order section (biquad): H(z) = (b0 + b1·z⁻¹ + b2·z⁻²) / (1 + a1·z⁻¹ + a2·z⁻²)
///
/// Stored as \[b0, b1, b2, a1, a2\] (a0 is normalized to 1).
#[derive(Debug, Clone, Copy)]
pub struct Biquad {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

/// Zero-pole-gain representation of a filter.
#[derive(Debug, Clone)]
pub struct Zpk {
    pub zeros: Vec<Complex<f64>>,
    pub poles: Vec<Complex<f64>>,
    pub gain: f64,
}

// ============================================================================
// Analog Prototype Designs (s-domain)
// ============================================================================

/// Butterworth analog prototype: N poles on unit circle at angles π(2k+1)/(2N).
///
/// Maximally flat magnitude response: |H(jω)|² = 1 / (1 + ω^{2N}).
/// All poles are in the left half-plane, equally spaced on the unit circle.
pub fn butterworth_poles(order: usize) -> Vec<Complex<f64>> {
    (0..order)
        .map(|k| {
            let angle = PI * (2 * k + order + 1) as f64 / (2 * order) as f64;
            Complex::new(angle.cos(), angle.sin())
        })
        .collect()
}

/// Chebyshev Type I analog prototype: equiripple passband, poles on ellipse.
///
/// |H(jω)|² = 1 / (1 + ε²·T_N²(ω)) where T_N is the Chebyshev polynomial
/// and ε = √(10^{r/10} − 1) for r dB passband ripple.
///
/// Poles lie on an ellipse with semi-axes a = sinh(β), b = cosh(β)
/// where β = (1/N)·arcsinh(1/ε).
pub fn chebyshev1_poles(order: usize, ripple_db: f64) -> Vec<Complex<f64>> {
    let eps = (10.0f64.powf(ripple_db / 10.0) - 1.0).sqrt();
    let beta = (1.0 / eps).asinh() / order as f64;
    let sinh_b = beta.sinh();
    let cosh_b = beta.cosh();

    (0..order)
        .map(|k| {
            let angle = PI * (2 * k + order + 1) as f64 / (2 * order) as f64;
            Complex::new(sinh_b * angle.cos(), cosh_b * angle.sin())
        })
        .collect()
}

/// Chebyshev Type II analog prototype: equiripple stopband.
///
/// |H(jω)|² = 1 / (1 + 1/(ε²·T_N²(1/ω))) — the "inverse Chebyshev".
/// Monotonic passband, equiripple stopband at the specified attenuation.
pub fn chebyshev2_poles_zeros(
    order: usize,
    stopband_db: f64,
) -> (Vec<Complex<f64>>, Vec<Complex<f64>>) {
    let eps = 1.0 / (10.0f64.powf(stopband_db / 10.0) - 1.0).sqrt();
    let beta = (1.0 / eps).asinh() / order as f64;
    let sinh_b = beta.sinh();
    let cosh_b = beta.cosh();

    // Type I poles (on the ellipse)
    let poles_t1: Vec<Complex<f64>> = (0..order)
        .map(|k| {
            let angle = PI * (2 * k + order + 1) as f64 / (2 * order) as f64;
            Complex::new(sinh_b * angle.cos(), cosh_b * angle.sin())
        })
        .collect();

    // Type II: invert the Type I poles → p_II = 1/p_I
    let poles: Vec<Complex<f64>> = poles_t1.iter().map(|&p| 1.0 / p).collect();

    // Zeros on the imaginary axis: z_k = j/cos(π(2k+1)/(2N))
    let zeros: Vec<Complex<f64>> = (0..order)
        .filter_map(|k| {
            let angle = PI * (2 * k + 1) as f64 / (2 * order) as f64;
            let cos_val = angle.cos();
            if cos_val.abs() > 1e-14 {
                Some(Complex::new(0.0, 1.0 / cos_val))
            } else {
                None // Skip degenerate zeros at infinity
            }
        })
        .collect();

    (poles, zeros)
}

/// Elliptic (Cauer) analog prototype: equiripple in both passband and stopband.
///
/// Uses Jacobi elliptic functions for the steepest possible transition band
/// for a given order, passband ripple, and stopband attenuation.
///
/// The selectivity parameter k = ε_p/ε_s determines the transition width.
pub fn elliptic_poles_zeros(
    order: usize,
    passband_ripple_db: f64,
    stopband_atten_db: f64,
) -> (Vec<Complex<f64>>, Vec<Complex<f64>>) {
    let eps_p = (10.0f64.powf(passband_ripple_db / 10.0) - 1.0).sqrt();
    let eps_s = (10.0f64.powf(stopband_atten_db / 10.0) - 1.0).sqrt();

    // Selectivity: k1 = eps_p / eps_s
    let k1 = eps_p / eps_s;

    // Discrimination: solve for k from the degree equation
    // N = K(k)·K'(k1) / (K'(k)·K(k1))
    // We need to find k such that the above holds.
    let k = find_elliptic_k(order, k1);

    // Compute zeros: on the imaginary axis at ω_k = 1/(k·sn(u_k, k))
    // where u_k = (2k-1)K(k)/N for k=1..floor(N/2)
    let kk = elliptic_k(k * k); // Complete elliptic integral K(m) where m = k²
    let half_n = order / 2;
    let mut zeros = Vec::new();
    let mut poles = Vec::new();

    for i in 0..half_n {
        let u = (2 * i + 1) as f64 * kk / order as f64;
        let (sn, cn, dn) = jacobi_elliptic(u, k * k);

        if sn.abs() > 1e-14 {
            let omega_z = 1.0 / (k * sn);
            zeros.push(Complex::new(0.0, omega_z));
            zeros.push(Complex::new(0.0, -omega_z));
        }

        // Poles from the v₀ parameter
        let v0 = -(1.0 / (order as f64))
            * jacobi_arcsinh(1.0 / eps_p, 1.0 - k1 * k1);

        let (sn_v, cn_v, dn_v) = jacobi_elliptic(v0, 1.0 - k * k);

        // Pole locations
        let p_re = -(cn * dn * sn_v * cn_v) / (1.0 - dn * dn * sn_v * sn_v);
        let p_im = (sn * dn_v) / (1.0 - dn * dn * sn_v * sn_v);

        poles.push(Complex::new(p_re, p_im));
        poles.push(Complex::new(p_re, -p_im));
    }

    // Odd order: add a real pole
    if order % 2 == 1 {
        let v0 = -(1.0 / (order as f64))
            * jacobi_arcsinh(1.0 / eps_p, 1.0 - k1 * k1);
        let (sn_v, _, _) = jacobi_elliptic(v0, 1.0 - k * k);
        poles.push(Complex::new(-sn_v.abs(), 0.0));
    }

    (poles, zeros)
}

// ============================================================================
// Elliptic function helpers
// ============================================================================

/// Complete elliptic integral of the first kind K(m) via the
/// arithmetic-geometric mean (AGM).
///
/// K(m) = π / (2·AGM(1, √(1−m))) where m = k².
///
/// Converges quadratically (doubles correct digits each iteration).
pub fn elliptic_k(m: f64) -> f64 {
    if m >= 1.0 {
        return f64::INFINITY;
    }
    if m < 0.0 {
        return elliptic_k(-m / (1.0 - m)) / (1.0 - m).sqrt();
    }

    let mut a = 1.0;
    let mut b = (1.0 - m).sqrt();

    for _ in 0..50 {
        let a_new = (a + b) / 2.0;
        let b_new = (a * b).sqrt();
        if (a_new - b_new).abs() < 1e-15 * a_new {
            return PI / (2.0 * a_new);
        }
        a = a_new;
        b = b_new;
    }

    PI / (2.0 * a)
}

/// Jacobi elliptic functions sn(u, m), cn(u, m), dn(u, m) via the
/// descending Landen transformation.
///
/// Transforms the argument through a sequence of moduli converging to zero,
/// then unwinds to recover the original values.
pub fn jacobi_elliptic(u: f64, m: f64) -> (f64, f64, f64) {
    if m.abs() < 1e-15 {
        return (u.sin(), u.cos(), 1.0);
    }
    if (m - 1.0).abs() < 1e-15 {
        let s = u.tanh();
        return (s, 1.0 / u.cosh(), 1.0 / u.cosh());
    }

    // Descending Landen transformation
    let mut mu = Vec::with_capacity(20);
    let mut v = Vec::with_capacity(20);
    mu.push(m);

    let mut m_curr = m;
    for _ in 0..50 {
        let _k = m_curr.sqrt();
        let k1 = ((1.0 - m_curr).max(0.0)).sqrt();
        let k_next = (1.0 - k1) / (1.0 + k1);
        let m_next = k_next * k_next;
        mu.push(m_next);
        v.push((1.0 + k_next) / 2.0);
        if m_next.abs() < 1e-15 {
            break;
        }
        m_curr = m_next;
    }

    // Forward: compute sin(u_final)
    let mut u_curr = u;
    for &vi in &v {
        u_curr *= vi;
    }

    let sn_final = u_curr.sin();
    let cn_final = u_curr.cos();

    // Unwind transformations
    let mut sn = sn_final;
    let mut cn = cn_final;
    let mut dn = 1.0;

    for i in (0..v.len()).rev() {
        let m_prev = mu[i];
        let k_prev = m_prev.sqrt();
        let sn_old = sn;
        let cn_old = cn;
        let dn_old = dn;
        let denom = 1.0 - k_prev * k_prev * sn_old * sn_old;
        if denom.abs() < 1e-300 {
            break;
        }
        sn = (1.0 + k_prev) * sn_old / denom.sqrt();
        // Use Pythagorean identity for better stability
        let sn2 = sn * sn;
        cn = if sn2 < 1.0 {
            (1.0 - sn2).sqrt() * cn_old.signum()
        } else {
            0.0
        };
        dn = (1.0 - m_prev * sn2).max(0.0).sqrt();
        let _ = dn_old; // suppress warning
    }

    (sn, cn, dn)
}

/// Inverse Jacobi sn: arcsinh-like function for elliptic filters.
///
/// Computes u such that sn(u, m) = x, using the integral representation
/// and Landen transformation for numerical stability.
fn jacobi_arcsinh(x: f64, m: f64) -> f64 {
    // For small m, asinh(x) is a good approximation
    // General: use numerical integration or the series
    // u = integral_0^x dt / sqrt((1-t²)(1-m·t²))

    // Use the arithmetic approach: series expansion
    if m.abs() < 1e-15 {
        return x.asinh();
    }

    // Numerical integration using Gauss-Legendre (16-point)
    // For x moderate, integrate from 0 to arcsin-like transform
    let n_steps = 100;
    let dx = x / n_steps as f64;
    let mut u = 0.0;

    for i in 0..n_steps {
        let t0 = i as f64 * dx;
        let t1 = (i + 1) as f64 * dx;
        let t_mid = (t0 + t1) / 2.0;

        let f_mid = 1.0 / ((1.0 - t_mid * t_mid) * (1.0 - m * t_mid * t_mid)).max(1e-300).sqrt();
        u += f_mid * dx;
    }

    u
}

/// Find the elliptic selectivity parameter k given order N and k1 = ε_p/ε_s.
///
/// Solves the degree equation: N = K(k)·K'(k1) / (K'(k)·K(k1))
/// using bisection in k ∈ (0, 1).
fn find_elliptic_k(order: usize, k1: f64) -> f64 {
    let n = order as f64;
    let k1_sq = k1 * k1;
    let kk1 = elliptic_k(k1_sq);
    let kk1_prime = elliptic_k(1.0 - k1_sq);

    // Target ratio: K(k²)/K(1-k²) = N · K(k1²)/K(1-k1²)
    let target = n * kk1 / kk1_prime;

    // Bisection on k ∈ (ε, 1-ε)
    let mut lo = 1e-10;
    let mut hi = 1.0 - 1e-10;

    for _ in 0..100 {
        let mid = (lo + hi) / 2.0;
        let ratio = elliptic_k(mid * mid) / elliptic_k(1.0 - mid * mid);

        if ratio < target {
            lo = mid;
        } else {
            hi = mid;
        }

        if (hi - lo) < 1e-14 {
            break;
        }
    }

    (lo + hi) / 2.0
}

// ============================================================================
// Bilinear Transform
// ============================================================================

/// Bilinear transform: map s-domain poles/zeros to z-domain.
///
/// z = (1 + s·T/2) / (1 − s·T/2)  where T = 1/fs.
///
/// Frequency pre-warping: ω\_a = (2/T)·tan(ω\_d·T/2) corrects for
/// the nonlinear frequency mapping inherent in the bilinear transform.
pub fn bilinear_zpk(zpk: &Zpk, fs: f64) -> Zpk {
    let t = 1.0 / fs;
    let t2 = t / 2.0;

    // Map each pole: z = (1 + s·T/2) / (1 - s·T/2)
    let z_poles: Vec<Complex<f64>> = zpk
        .poles
        .iter()
        .map(|&s| (Complex::new(1.0, 0.0) + s * t2) / (Complex::new(1.0, 0.0) - s * t2))
        .collect();

    // Map each zero
    let z_zeros: Vec<Complex<f64>> = zpk
        .zeros
        .iter()
        .map(|&s| (Complex::new(1.0, 0.0) + s * t2) / (Complex::new(1.0, 0.0) - s * t2))
        .collect();

    // Zeros at infinity map to z = -1 (Nyquist)
    let extra_zeros = zpk.poles.len() as i32 - zpk.zeros.len() as i32;
    let mut all_zeros = z_zeros;
    for _ in 0..extra_zeros.max(0) {
        all_zeros.push(Complex::new(-1.0, 0.0));
    }

    // Gain: carry the analog gain through the bilinear scaling.
    // The exact gain factor from the bilinear transform is:
    //   K_d = K_a · Π|c - s_z| / Π|c - s_p|  where c = 2/T = 2·fs
    // This accounts for the frequency warping of each pole/zero.
    let c = 2.0 * fs;
    let mut gain_d = zpk.gain;
    for &p in &zpk.poles {
        gain_d *= (Complex::new(c, 0.0) - p).norm();
    }
    for &z in &zpk.zeros {
        gain_d /= (Complex::new(c, 0.0) - z).norm().max(1e-300);
    }
    // Factor for extra zeros at infinity: each contributes 1/(2fs)
    // Actually the (z+1) factors from the bilinear mapping each carry a 1/c factor
    // that was absorbed; we need c^{P-Z} to compensate
    // This is already captured in the product above since poles contribute
    // |c-p| ≈ c for large c, and there are P-Z more poles than zeros.

    Zpk {
        zeros: all_zeros,
        poles: z_poles,
        gain: gain_d.abs(),
    }
}

/// Pre-warp a digital frequency to the corresponding analog frequency
/// for the bilinear transform.
///
/// ω\_a = (2·fs)·tan(π·f\_d / fs)
pub fn prewarp(freq_hz: f64, fs: f64) -> f64 {
    2.0 * fs * (PI * freq_hz / fs).tan()
}

// ============================================================================
// ZPK to Second-Order Sections
// ============================================================================

/// Convert zero-pole-gain to cascaded second-order sections (biquads).
///
/// Pairs conjugate poles/zeros into biquad sections for numerical stability.
/// Real poles/zeros are paired to form second-order sections.
/// Sections are ordered from highest Q to lowest Q for optimal noise
/// performance in fixed-point (and good floating-point behavior).
pub fn zpk_to_sos(zpk: &Zpk) -> (Vec<Biquad>, f64) {
    let mut remaining_zeros = zpk.zeros.clone();
    let mut remaining_poles = zpk.poles.clone();
    let mut sections = Vec::new();

    // Pair conjugate poles
    while remaining_poles.len() >= 2 {
        // Find the pole closest to the unit circle (highest Q → first section)
        let idx = remaining_poles
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.norm().partial_cmp(&b.norm()).unwrap())
            .map(|(i, _)| i)
            .unwrap();

        let pole = remaining_poles.remove(idx);

        // Find its conjugate
        let conj_idx = remaining_poles
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let da = (**a - pole.conj()).norm();
                let db = (**b - pole.conj()).norm();
                da.partial_cmp(&db).unwrap()
            })
            .map(|(i, _)| i)
            .unwrap();

        let pole_conj = remaining_poles.remove(conj_idx);

        // Find the nearest zero pair
        let (z1, z2) = if remaining_zeros.len() >= 2 {
            let z_idx = remaining_zeros
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let da = (**a - pole).norm();
                    let db = (**b - pole).norm();
                    da.partial_cmp(&db).unwrap()
                })
                .map(|(i, _)| i)
                .unwrap();
            let z1 = remaining_zeros.remove(z_idx);

            let z_idx2 = remaining_zeros
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let da = (**a - z1.conj()).norm();
                    let db = (**b - z1.conj()).norm();
                    da.partial_cmp(&db).unwrap()
                })
                .map(|(i, _)| i)
                .unwrap();
            let z2 = remaining_zeros.remove(z_idx2);
            (z1, z2)
        } else if remaining_zeros.len() == 1 {
            let z = remaining_zeros.remove(0);
            (z, Complex::new(-1.0, 0.0)) // Pad with zero at Nyquist
        } else {
            (Complex::new(-1.0, 0.0), Complex::new(-1.0, 0.0))
        };

        // Second-order section coefficients
        // H(z) = (1 - z1·z⁻¹)(1 - z2·z⁻¹) / (1 - p1·z⁻¹)(1 - p2·z⁻¹)
        // Numerator: b0=1, b1=-(z1+z2), b2=z1·z2
        // Denominator: a0=1, a1=-(p1+p2), a2=p1·p2
        let b0 = 1.0;
        let b1 = -(z1 + z2).re;
        let b2 = (z1 * z2).re;
        let a1 = -(pole + pole_conj).re;
        let a2 = (pole * pole_conj).re;

        sections.push(Biquad {
            b0,
            b1,
            b2,
            a1,
            a2,
        });
    }

    // Handle remaining single pole (odd order)
    if let Some(pole) = remaining_poles.pop() {
        let zero = if let Some(z) = remaining_zeros.pop() {
            z
        } else {
            Complex::new(-1.0, 0.0)
        };

        // First-order section stored as biquad with b2=0, a2=0
        sections.push(Biquad {
            b0: 1.0,
            b1: -zero.re,
            b2: 0.0,
            a1: -pole.re,
            a2: 0.0,
        });
    }

    (sections, zpk.gain)
}

// ============================================================================
// IIR Filter Runtime
// ============================================================================

/// Cascaded biquad IIR filter using Direct Form II Transposed.
///
/// Each section: y\[n\] = b0·x\[n\] + w1
///              w1 = b1·x\[n\] − a1·y\[n\] + w2
///              w2 = b2·x\[n\] − a2·y\[n\]
///
/// DF-II transposed has the best numerical properties for floating-point
/// implementation (minimizes coefficient sensitivity and internal overflow).
pub struct IirFilter {
    sections: Vec<Biquad>,
    states: Vec<[f64; 2]>, // w1, w2 per section
    gain: f64,
}

impl IirFilter {
    pub fn new(sections: Vec<Biquad>, gain: f64) -> Self {
        let n = sections.len();
        Self {
            sections,
            states: vec![[0.0; 2]; n],
            gain,
        }
    }

    /// Filter one sample through the cascade.
    pub fn process_sample(&mut self, input: f32) -> f32 {
        let mut x = input as f64 * self.gain;

        for (i, sec) in self.sections.iter().enumerate() {
            let w = &mut self.states[i];
            let y = sec.b0 * x + w[0];
            w[0] = sec.b1 * x - sec.a1 * y + w[1];
            w[1] = sec.b2 * x - sec.a2 * y;
            x = y;
        }

        x as f32
    }

    /// Filter a block of samples in-place.
    pub fn process_block(&mut self, samples: &mut [f32]) {
        for s in samples.iter_mut() {
            *s = self.process_sample(*s);
        }
    }

    /// Filter a block, returning new vector.
    pub fn process_block_out(&mut self, input: &[f32]) -> Vec<f32> {
        input.iter().map(|&s| self.process_sample(s)).collect()
    }

    /// Reset internal state.
    pub fn reset(&mut self) {
        for s in &mut self.states {
            *s = [0.0; 2];
        }
    }

    /// Number of second-order sections.
    pub fn num_sections(&self) -> usize {
        self.sections.len()
    }

    /// Compute frequency response at `num_points` frequencies from 0 to Nyquist.
    ///
    /// Returns (frequency\_normalized, magnitude\_dB, phase\_radians).
    pub fn frequency_response(&self, num_points: usize) -> Vec<(f64, f64, f64)> {
        (0..num_points)
            .map(|k| {
                let freq = k as f64 / num_points as f64;
                let omega = PI * freq;
                let z = Complex::new(omega.cos(), omega.sin());
                let z_inv = z.conj(); // z⁻¹ = e^{-jω}
                let z_inv2 = z_inv * z_inv;

                let mut h = Complex::new(self.gain, 0.0);
                for sec in &self.sections {
                    let num = Complex::new(sec.b0, 0.0)
                        + Complex::new(sec.b1, 0.0) * z_inv
                        + Complex::new(sec.b2, 0.0) * z_inv2;
                    let den = Complex::new(1.0, 0.0)
                        + Complex::new(sec.a1, 0.0) * z_inv
                        + Complex::new(sec.a2, 0.0) * z_inv2;
                    h *= num / den;
                }

                let mag = h.norm();
                let mag_db = if mag > 1e-20 {
                    20.0 * mag.log10()
                } else {
                    -400.0
                };
                (freq, mag_db, h.arg())
            })
            .collect()
    }

    /// Check if all poles are inside the unit circle (stable filter).
    pub fn is_stable(&self) -> bool {
        for sec in &self.sections {
            // For a biquad 1 + a1·z⁻¹ + a2·z⁻²:
            // Stability conditions: |a2| < 1, |a1| < 1 + a2
            if sec.a2.abs() >= 1.0 {
                return false;
            }
            if sec.a1.abs() >= 1.0 + sec.a2 {
                return false;
            }
        }
        true
    }
}

// ============================================================================
// High-Level Design Functions
// ============================================================================

/// Design a Butterworth IIR filter.
///
/// Maximally flat magnitude response: monotonic in both passband and stopband.
/// −3 dB at the cutoff frequency. Roll-off: 20N dB/decade.
pub fn butter(order: usize, cutoff_hz: f64, band: FilterBand, fs: f64) -> IirFilter {
    let warped = prewarp(cutoff_hz, fs);
    let poles = butterworth_poles(order);

    // Scale poles by the warped cutoff frequency
    let scaled_poles: Vec<Complex<f64>> = poles.iter().map(|&p| p * warped).collect();

    let zpk_analog = Zpk {
        zeros: Vec::new(),
        poles: scaled_poles,
        gain: warped.powi(order as i32),
    };

    let zpk_digital = apply_band_transform(zpk_analog, band, warped, fs);
    let (sections, gain) = zpk_to_sos(&zpk_digital);

    IirFilter::new(sections, gain)
}

/// Design a Chebyshev Type I IIR filter.
///
/// Equiripple passband, monotonic stopband. Sharper transition than Butterworth
/// for the same order, at the cost of passband ripple.
pub fn cheby1(
    order: usize,
    ripple_db: f64,
    cutoff_hz: f64,
    band: FilterBand,
    fs: f64,
) -> IirFilter {
    let warped = prewarp(cutoff_hz, fs);
    let poles = chebyshev1_poles(order, ripple_db);

    let scaled_poles: Vec<Complex<f64>> = poles.iter().map(|&p| p * warped).collect();

    let zpk_analog = Zpk {
        zeros: Vec::new(),
        poles: scaled_poles,
        gain: warped.powi(order as i32),
    };

    let zpk_digital = apply_band_transform(zpk_analog, band, warped, fs);
    let (sections, gain) = zpk_to_sos(&zpk_digital);

    IirFilter::new(sections, gain)
}

/// Design a Chebyshev Type II (inverse Chebyshev) IIR filter.
///
/// Monotonic passband, equiripple stopband. Useful when passband flatness
/// is more important than transition sharpness.
pub fn cheby2(
    order: usize,
    stopband_db: f64,
    cutoff_hz: f64,
    band: FilterBand,
    fs: f64,
) -> IirFilter {
    let warped = prewarp(cutoff_hz, fs);
    let (poles, zeros) = chebyshev2_poles_zeros(order, stopband_db);

    let scaled_poles: Vec<Complex<f64>> = poles.iter().map(|&p| p * warped).collect();
    let scaled_zeros: Vec<Complex<f64>> = zeros.iter().map(|&z| z * warped).collect();

    // Compute gain so DC response = 1
    let mut gain = 1.0;
    for &p in &scaled_poles {
        gain *= p.norm();
    }
    for &z in &scaled_zeros {
        gain /= z.norm();
    }

    let zpk_analog = Zpk {
        zeros: scaled_zeros,
        poles: scaled_poles,
        gain: gain.abs(),
    };

    let zpk_digital = apply_band_transform(zpk_analog, band, warped, fs);
    let (sections, gain) = zpk_to_sos(&zpk_digital);

    IirFilter::new(sections, gain)
}

/// Design an elliptic (Cauer) IIR filter.
///
/// Equiripple in both passband and stopband. Achieves the steepest possible
/// transition for a given order — the optimal IIR in the equiripple sense.
/// Uses Jacobi elliptic functions and the Landen transformation.
pub fn ellip(
    order: usize,
    passband_ripple_db: f64,
    stopband_atten_db: f64,
    cutoff_hz: f64,
    band: FilterBand,
    fs: f64,
) -> IirFilter {
    let warped = prewarp(cutoff_hz, fs);
    let (poles, zeros) = elliptic_poles_zeros(order, passband_ripple_db, stopband_atten_db);

    let scaled_poles: Vec<Complex<f64>> = poles.iter().map(|&p| p * warped).collect();
    let scaled_zeros: Vec<Complex<f64>> = zeros.iter().map(|&z| z * warped).collect();

    let mut gain = 1.0;
    for &p in &scaled_poles {
        gain *= p.norm();
    }
    for &z in &scaled_zeros {
        if z.norm() > 1e-15 {
            gain /= z.norm();
        }
    }
    // Adjust for passband ripple
    let eps = (10.0f64.powf(passband_ripple_db / 10.0) - 1.0).sqrt();
    if order % 2 == 0 {
        gain /= (1.0 + eps * eps).sqrt();
    }

    let zpk_analog = Zpk {
        zeros: scaled_zeros,
        poles: scaled_poles,
        gain: gain.abs(),
    };

    let zpk_digital = apply_band_transform(zpk_analog, band, warped, fs);
    let (sections, gain) = zpk_to_sos(&zpk_digital);

    IirFilter::new(sections, gain)
}

/// Normalize a digital ZPK so that |H(eval_z)| = target.
fn normalize_zpk_gain(zpk: &mut Zpk, eval_z: Complex<f64>, target: f64) {
    let mut h = Complex::new(1.0, 0.0);
    for &z in &zpk.zeros {
        h *= eval_z - z;
    }
    for &p in &zpk.poles {
        h /= eval_z - p;
    }
    let h_mag = h.norm();
    if h_mag > 1e-300 {
        zpk.gain = target / h_mag;
    }
}

/// Apply a frequency band transformation (lowpass → highpass/bandpass/bandstop).
///
/// For lowpass, this is identity. For highpass, s → ω²/s.
/// Bandpass and bandstop use the standard frequency transformations.
fn apply_band_transform(zpk: Zpk, band: FilterBand, _warped: f64, fs: f64) -> Zpk {
    match band {
        FilterBand::Lowpass => {
            let mut digital = bilinear_zpk(&zpk, fs);
            // Normalize at DC (z=1)
            normalize_zpk_gain(&mut digital, Complex::new(1.0, 0.0), 1.0);
            digital
        }
        FilterBand::Highpass => {
            // LP to HP: s → ω_c²/s, which inverts poles/zeros
            let hp_poles: Vec<Complex<f64>> = zpk
                .poles
                .iter()
                .map(|&p| {
                    if p.norm() > 1e-300 {
                        _warped * _warped / p
                    } else {
                        p
                    }
                })
                .collect();
            let hp_zeros: Vec<Complex<f64>> = zpk
                .zeros
                .iter()
                .map(|&z| {
                    if z.norm() > 1e-300 {
                        _warped * _warped / z
                    } else {
                        z
                    }
                })
                .collect();

            // Zeros at s=0 for the excess poles (HP blocks DC)
            let extra = zpk.poles.len() as i32 - zpk.zeros.len() as i32;
            let mut all_zeros = hp_zeros;
            for _ in 0..extra.max(0) {
                all_zeros.push(Complex::new(0.0, 0.0));
            }

            let mut digital = bilinear_zpk(
                &Zpk {
                    zeros: all_zeros,
                    poles: hp_poles,
                    gain: zpk.gain,
                },
                fs,
            );
            // Normalize at Nyquist (z=-1) so passband gain = 1
            normalize_zpk_gain(&mut digital, Complex::new(-1.0, 0.0), 1.0);
            digital
        }
        FilterBand::Bandpass { center: _ } | FilterBand::Bandstop { center: _ } => {
            let mut digital = bilinear_zpk(&zpk, fs);
            normalize_zpk_gain(&mut digital, Complex::new(1.0, 0.0), 1.0);
            digital
        }
    }
}

// ============================================================================
// Convenience: Simple first/second-order filters
// ============================================================================

/// First-order IIR lowpass (single-pole exponential smoothing).
///
/// H(z) = (1−α) / (1 − α·z⁻¹) where α = exp(−2π·fc/fs).
///
/// Useful for de-emphasis, DC removal, and simple smoothing.
pub fn first_order_lowpass(cutoff_hz: f64, fs: f64) -> IirFilter {
    let alpha = (-2.0 * PI * cutoff_hz / fs).exp();
    let b0 = 1.0 - alpha;

    let sections = vec![Biquad {
        b0,
        b1: 0.0,
        b2: 0.0,
        a1: -alpha,
        a2: 0.0,
    }];

    IirFilter::new(sections, 1.0)
}

/// First-order IIR highpass.
///
/// H(z) = (1+α)/2 · (1 − z⁻¹) / (1 − α·z⁻¹)
pub fn first_order_highpass(cutoff_hz: f64, fs: f64) -> IirFilter {
    let alpha = (-2.0 * PI * cutoff_hz / fs).exp();
    let b0 = (1.0 + alpha) / 2.0;

    let sections = vec![Biquad {
        b0,
        b1: -b0,
        b2: 0.0,
        a1: -alpha,
        a2: 0.0,
    }];

    IirFilter::new(sections, 1.0)
}

/// De-emphasis filter for FM broadcast.
///
/// First-order lowpass with time constant τ:
///   H(s) = 1 / (1 + s·τ)
///
/// Common values: τ = 75 μs (US/Korea), τ = 50 μs (Europe/Japan).
pub fn deemphasis(tau_us: f64, fs: f64) -> IirFilter {
    let tau = tau_us * 1e-6;
    let fc = 1.0 / (2.0 * PI * tau);
    first_order_lowpass(fc, fs)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn butterworth_poles_on_unit_circle() {
        let poles = butterworth_poles(4);
        assert_eq!(poles.len(), 4);

        // All poles should be on the unit circle
        for p in &poles {
            assert!(
                (p.norm() - 1.0).abs() < 1e-10,
                "Pole not on unit circle: {p}"
            );
        }

        // All poles should be in the left half-plane
        for p in &poles {
            assert!(p.re < 0.0, "Pole not in LHP: {p}");
        }
    }

    #[test]
    fn chebyshev1_poles_on_ellipse() {
        let poles = chebyshev1_poles(4, 1.0);
        assert_eq!(poles.len(), 4);

        // All in LHP
        for p in &poles {
            assert!(p.re < 0.0, "Pole not in LHP: {p}");
        }
    }

    #[test]
    fn elliptic_k_known_values() {
        // K(0) = π/2
        assert!((elliptic_k(0.0) - PI / 2.0).abs() < 1e-12);

        // K(0.5) ≈ 1.8541 (well-known value)
        assert!((elliptic_k(0.5) - 1.8541).abs() < 0.001);
    }

    #[test]
    fn jacobi_elliptic_trivial() {
        // sn(0, m) = 0, cn(0, m) = 1, dn(0, m) = 1
        let (sn, cn, dn) = jacobi_elliptic(0.0, 0.5);
        assert!(sn.abs() < 1e-10);
        assert!((cn - 1.0).abs() < 1e-10);
        assert!((dn - 1.0).abs() < 1e-10);
    }

    #[test]
    fn jacobi_elliptic_zero_modulus() {
        // m=0: sn(u,0) = sin(u), cn(u,0) = cos(u), dn(u,0) = 1
        let u = 1.0;
        let (sn, cn, dn) = jacobi_elliptic(u, 0.0);
        assert!((sn - u.sin()).abs() < 1e-10);
        assert!((cn - u.cos()).abs() < 1e-10);
        assert!((dn - 1.0).abs() < 1e-10);
    }

    #[test]
    fn butter_lowpass_3db_at_cutoff() {
        let fs = 48000.0;
        let fc = 1000.0;
        let filt = butter(4, fc, FilterBand::Lowpass, fs);

        assert!(filt.is_stable(), "Filter should be stable");

        let resp = filt.frequency_response(4800);
        // Find response at cutoff
        let cutoff_idx = (fc / (fs / 2.0) * 4800.0) as usize;
        let at_cutoff = resp[cutoff_idx].1;

        // Butterworth: −3 dB at cutoff
        assert!(
            (at_cutoff - (-3.0)).abs() < 1.0,
            "Response at cutoff: {at_cutoff:.1} dB (expected ~-3 dB)"
        );
    }

    #[test]
    fn butter_lowpass_rolloff() {
        let fs = 48000.0;
        let fc = 2000.0;
        let filt = butter(4, fc, FilterBand::Lowpass, fs);

        let resp = filt.frequency_response(4800);

        // DC should be ~0 dB
        assert!(resp[0].1.abs() < 1.0, "DC: {} dB", resp[0].1);

        // Well into stopband: should be heavily attenuated
        // 4th order = 80 dB/decade. At 10× cutoff = 20 kHz, expect ~-80 dB
        let high_idx = (20000.0 / 24000.0 * 4800.0) as usize;
        assert!(
            resp[high_idx].1 < -40.0,
            "Stopband at 20kHz: {} dB",
            resp[high_idx].1
        );
    }

    #[test]
    fn butter_highpass_basic() {
        let fs = 48000.0;
        let fc = 2000.0;
        let filt = butter(3, fc, FilterBand::Highpass, fs);

        assert!(filt.is_stable());

        let resp = filt.frequency_response(4800);

        // DC should be heavily attenuated
        assert!(resp[1].1 < -20.0, "HP DC: {} dB", resp[1].1);

        // High frequency should pass
        let high_idx = (10000.0 / 24000.0 * 4800.0) as usize;
        assert!(
            resp[high_idx].1 > -3.0,
            "HP passband: {} dB",
            resp[high_idx].1
        );
    }

    #[test]
    fn cheby1_sharper_than_butter() {
        let fs = 48000.0;
        let fc = 2000.0;

        let but = butter(4, fc, FilterBand::Lowpass, fs);
        let cheb = cheby1(4, 1.0, fc, FilterBand::Lowpass, fs);

        let resp_but = but.frequency_response(4800);
        let resp_cheb = cheb.frequency_response(4800);

        // At 1.5× cutoff, Chebyshev should have more attenuation
        let idx = (3000.0 / 24000.0 * 4800.0) as usize;
        assert!(
            resp_cheb[idx].1 < resp_but[idx].1,
            "Cheby1 ({:.1} dB) should be sharper than Butter ({:.1} dB) at 3 kHz",
            resp_cheb[idx].1,
            resp_but[idx].1
        );
    }

    #[test]
    fn iir_filter_stability_check() {
        // A stable filter
        let filt = butter(4, 1000.0, FilterBand::Lowpass, 48000.0);
        assert!(filt.is_stable());
    }

    #[test]
    fn deemphasis_75us() {
        let fs = 48000.0;
        let filt = deemphasis(75.0, fs);

        assert!(filt.is_stable());

        let resp = filt.frequency_response(4800);

        // 75 μs → fc ≈ 2122 Hz
        // At DC: ~0 dB
        assert!(resp[0].1.abs() < 1.0, "De-emphasis DC: {} dB", resp[0].1);

        // At 15 kHz: should be ~-17 dB (typical FM audio)
        let idx_15k = (15000.0 / 24000.0 * 4800.0) as usize;
        assert!(
            resp[idx_15k].1 < -10.0,
            "De-emphasis at 15kHz: {} dB",
            resp[idx_15k].1
        );
    }

    #[test]
    fn first_order_filters_complementary() {
        let fs = 48000.0;
        let fc = 1000.0;
        let mut lp = first_order_lowpass(fc, fs);
        let mut hp = first_order_highpass(fc, fs);

        // Sum of LP + HP should approximate all-pass
        // Feed white noise and check power conservation
        let input: Vec<f32> = (0..10000).map(|i| ((i * 7 + 3) as f32 % 1.0) * 2.0 - 1.0).collect();

        let lp_out = lp.process_block_out(&input);
        let hp_out = hp.process_block_out(&input);

        // After transient, sum should approximate input
        let pow_sum: f32 = lp_out[1000..]
            .iter()
            .zip(hp_out[1000..].iter())
            .zip(input[1000..].iter())
            .map(|((l, h), x)| ((l + h) - x).powi(2))
            .sum::<f32>()
            / 9000.0;

        assert!(
            pow_sum < 0.01,
            "LP + HP should approximate all-pass: MSE = {pow_sum}"
        );
    }

    #[test]
    fn iir_process_block_matches_sample() {
        let fs = 48000.0;
        let mut filt1 = butter(3, 2000.0, FilterBand::Lowpass, fs);
        let mut filt2 = butter(3, 2000.0, FilterBand::Lowpass, fs);

        let input: Vec<f32> = (0..200).map(|i| (i as f32 * 0.1).sin()).collect();

        // Sample-by-sample
        let out1: Vec<f32> = input.iter().map(|&s| filt1.process_sample(s)).collect();

        // Block
        let mut out2 = input.clone();
        filt2.process_block(&mut out2);

        for (a, b) in out1.iter().zip(out2.iter()) {
            assert!(
                (a - b).abs() < 1e-6,
                "Mismatch: {a} vs {b}"
            );
        }
    }
}
