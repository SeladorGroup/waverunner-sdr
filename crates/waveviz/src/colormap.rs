//! Colormap module for mapping normalized values to RGBA colors.
//!
//! Provides precomputed 256-entry lookup tables for efficient GPU-side
//! and CPU-side color mapping. Colormaps are uploaded as 1D textures
//! for shader-side lookup.

/// Available colormap presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colormap {
    /// Google Turbo — high perceptual resolution, good for SDR.
    Turbo,
    /// Viridis — perceptually uniform, colorblind-friendly.
    Viridis,
    /// Magma — dark-to-bright, good contrast.
    Magma,
    /// Inferno — similar to Magma with more yellow.
    Inferno,
    /// Grayscale — simple linear luminance ramp.
    Grayscale,
}

/// Precomputed 256-entry RGBA lookup table.
pub struct ColormapLut {
    /// 256 RGBA entries, one per quantization level.
    pub table: [[u8; 4]; 256],
}

impl ColormapLut {
    /// Generate a LUT for the given colormap.
    pub fn new(colormap: Colormap) -> Self {
        let mut table = [[0u8; 4]; 256];
        for (i, entry) in table.iter_mut().enumerate() {
            let t = i as f32 / 255.0;
            *entry = map_color(colormap, t);
        }
        Self { table }
    }

    /// Look up a color by normalized value [0, 1].
    pub fn lookup(&self, normalized: f32) -> [u8; 4] {
        let idx = (normalized.clamp(0.0, 1.0) * 255.0) as usize;
        self.table[idx.min(255)]
    }

    /// Get the table as a flat byte slice (for GPU upload).
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.table)
    }
}

/// Map a normalized value [0, 1] to an RGBA color for the given colormap.
pub fn map_color(colormap: Colormap, t: f32) -> [u8; 4] {
    let t = t.clamp(0.0, 1.0);
    match colormap {
        Colormap::Turbo => turbo(t),
        Colormap::Viridis => viridis(t),
        Colormap::Magma => magma(t),
        Colormap::Inferno => inferno(t),
        Colormap::Grayscale => grayscale(t),
    }
}

/// Piecewise linear interpolation over control points.
fn lerp_colormap(t: f32, points: &[(f32, [u8; 3])]) -> [u8; 4] {
    if points.is_empty() {
        return [0, 0, 0, 255];
    }
    if t <= points[0].0 {
        let c = points[0].1;
        return [c[0], c[1], c[2], 255];
    }
    if t >= points[points.len() - 1].0 {
        let c = points[points.len() - 1].1;
        return [c[0], c[1], c[2], 255];
    }

    for i in 0..points.len() - 1 {
        let (t0, c0) = points[i];
        let (t1, c1) = points[i + 1];
        if t >= t0 && t <= t1 {
            let frac = (t - t0) / (t1 - t0);
            let r = (c0[0] as f32 + frac * (c1[0] as f32 - c0[0] as f32)) as u8;
            let g = (c0[1] as f32 + frac * (c1[1] as f32 - c0[1] as f32)) as u8;
            let b = (c0[2] as f32 + frac * (c1[2] as f32 - c0[2] as f32)) as u8;
            return [r, g, b, 255];
        }
    }

    [0, 0, 0, 255]
}

fn turbo(t: f32) -> [u8; 4] {
    let points: &[(f32, [u8; 3])] = &[
        (0.00, [48, 18, 59]),
        (0.10, [68, 62, 168]),
        (0.20, [61, 119, 235]),
        (0.30, [31, 175, 214]),
        (0.40, [23, 218, 150]),
        (0.50, [88, 237, 82]),
        (0.60, [174, 236, 33]),
        (0.70, [233, 204, 28]),
        (0.80, [252, 148, 14]),
        (0.90, [235, 81, 6]),
        (1.00, [122, 4, 3]),
    ];
    lerp_colormap(t, points)
}

fn viridis(t: f32) -> [u8; 4] {
    let points: &[(f32, [u8; 3])] = &[
        (0.00, [68, 1, 84]),
        (0.13, [72, 35, 116]),
        (0.25, [64, 67, 135]),
        (0.38, [52, 94, 141]),
        (0.50, [33, 144, 140]),
        (0.63, [53, 183, 121]),
        (0.75, [109, 205, 89]),
        (0.88, [180, 222, 44]),
        (1.00, [253, 231, 37]),
    ];
    lerp_colormap(t, points)
}

fn magma(t: f32) -> [u8; 4] {
    let points: &[(f32, [u8; 3])] = &[
        (0.00, [0, 0, 4]),
        (0.14, [28, 16, 68]),
        (0.29, [79, 18, 123]),
        (0.43, [137, 28, 111]),
        (0.57, [187, 55, 84]),
        (0.71, [228, 107, 62]),
        (0.86, [249, 182, 72]),
        (1.00, [252, 253, 191]),
    ];
    lerp_colormap(t, points)
}

fn inferno(t: f32) -> [u8; 4] {
    let points: &[(f32, [u8; 3])] = &[
        (0.00, [0, 0, 4]),
        (0.14, [31, 12, 72]),
        (0.29, [85, 15, 109]),
        (0.43, [136, 34, 106]),
        (0.57, [186, 54, 85]),
        (0.71, [227, 89, 51]),
        (0.86, [249, 149, 21]),
        (1.00, [252, 255, 164]),
    ];
    lerp_colormap(t, points)
}

fn grayscale(t: f32) -> [u8; 4] {
    let v = (t * 255.0) as u8;
    [v, v, v, 255]
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lut_boundary_values() {
        for cm in [
            Colormap::Turbo,
            Colormap::Viridis,
            Colormap::Magma,
            Colormap::Inferno,
            Colormap::Grayscale,
        ] {
            let lut = ColormapLut::new(cm);
            // First and last entries should have full alpha
            assert_eq!(lut.table[0][3], 255);
            assert_eq!(lut.table[255][3], 255);
        }
    }

    #[test]
    fn lut_endpoints() {
        let lut = ColormapLut::new(Colormap::Turbo);
        // Turbo starts dark blue
        assert!(
            lut.table[0][2] > lut.table[0][0],
            "Turbo should start bluish"
        );
        // Turbo ends dark red
        let last = lut.table[255];
        assert!(last[0] > last[2], "Turbo should end reddish");
    }

    #[test]
    fn grayscale_monotonic() {
        let lut = ColormapLut::new(Colormap::Grayscale);
        for i in 0..255 {
            assert!(
                lut.table[i + 1][0] >= lut.table[i][0],
                "Grayscale should be monotonic"
            );
            // R = G = B for grayscale
            assert_eq!(lut.table[i][0], lut.table[i][1]);
            assert_eq!(lut.table[i][1], lut.table[i][2]);
        }
    }

    #[test]
    fn lookup_clamps() {
        let lut = ColormapLut::new(Colormap::Grayscale);
        assert_eq!(lut.lookup(-1.0), lut.table[0]);
        assert_eq!(lut.lookup(2.0), lut.table[255]);
    }

    #[test]
    fn lut_as_bytes_length() {
        let lut = ColormapLut::new(Colormap::Turbo);
        assert_eq!(lut.as_bytes().len(), 256 * 4);
    }
}
