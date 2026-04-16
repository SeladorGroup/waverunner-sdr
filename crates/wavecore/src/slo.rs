//! Service Level Objective definitions and runtime checker.
//!
//! Provides machine-readable SLO thresholds that are enforced in automated
//! tests and CI gates. The canonical SLO values live in `slo.toml` at the
//! workspace root and are compiled into the binary via `include_str!`.

use serde::Deserialize;

use crate::session::SessionStats;

/// Parsed SLO configuration from slo.toml.
#[derive(Debug, Clone, Deserialize)]
pub struct Slo {
    pub drop_budget: DropBudget,
    pub latency: LatencyBudget,
    pub throughput: ThroughputBudget,
    pub startup: StartupBudget,
    pub export: ExportBudget,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DropBudget {
    pub max_block_drop_rate: f64,
    pub max_event_drop_rate: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LatencyBudget {
    pub max_block_latency_us: u64,
    pub max_fft_latency_us: u64,
    pub max_cfar_latency_us: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThroughputBudget {
    pub sustained_throughput_msps: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StartupBudget {
    pub max_startup_blocks: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExportBudget {
    pub max_export_time_ms: u64,
}

/// A single SLO violation.
#[derive(Debug, Clone)]
pub struct SloViolation {
    /// Which SLO was violated.
    pub name: &'static str,
    /// Measured value.
    pub measured: f64,
    /// Threshold that was exceeded.
    pub threshold: f64,
}

impl std::fmt::Display for SloViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: measured={:.4}, threshold={:.4}",
            self.name, self.measured, self.threshold
        )
    }
}

impl Slo {
    /// Load SLO from the embedded slo.toml.
    pub fn load() -> Self {
        let toml_str = include_str!("../../../slo.toml");
        toml::from_str(toml_str).expect("slo.toml must be valid")
    }

    /// Check a SessionStats snapshot against all applicable SLOs.
    ///
    /// Returns an empty vec if all SLOs are met.
    pub fn check_stats(&self, stats: &SessionStats) -> Vec<SloViolation> {
        let mut violations = Vec::new();

        // Block drop rate
        if stats.blocks_processed > 0 {
            let drop_rate = stats.blocks_dropped as f64 / stats.blocks_processed as f64;
            if drop_rate > self.drop_budget.max_block_drop_rate {
                violations.push(SloViolation {
                    name: "max_block_drop_rate",
                    measured: drop_rate,
                    threshold: self.drop_budget.max_block_drop_rate,
                });
            }
        }

        // Event drop rate (approximate: use events_dropped / blocks_processed as proxy)
        if stats.blocks_processed > 0 {
            let event_drop_rate = stats.events_dropped as f64 / stats.blocks_processed as f64;
            if event_drop_rate > self.drop_budget.max_event_drop_rate {
                violations.push(SloViolation {
                    name: "max_event_drop_rate",
                    measured: event_drop_rate,
                    threshold: self.drop_budget.max_event_drop_rate,
                });
            }
        }

        // Block latency
        if stats.latency.total_us > self.latency.max_block_latency_us {
            violations.push(SloViolation {
                name: "max_block_latency_us",
                measured: stats.latency.total_us as f64,
                threshold: self.latency.max_block_latency_us as f64,
            });
        }

        // FFT latency
        if stats.latency.fft_us > self.latency.max_fft_latency_us {
            violations.push(SloViolation {
                name: "max_fft_latency_us",
                measured: stats.latency.fft_us as f64,
                threshold: self.latency.max_fft_latency_us as f64,
            });
        }

        // CFAR latency
        if stats.latency.cfar_us > self.latency.max_cfar_latency_us {
            violations.push(SloViolation {
                name: "max_cfar_latency_us",
                measured: stats.latency.cfar_us as f64,
                threshold: self.latency.max_cfar_latency_us as f64,
            });
        }

        // Throughput (only check after enough blocks to stabilize)
        if stats.blocks_processed > 20
            && stats.throughput_msps < self.throughput.sustained_throughput_msps
        {
            violations.push(SloViolation {
                name: "sustained_throughput_msps",
                measured: stats.throughput_msps,
                threshold: self.throughput.sustained_throughput_msps,
            });
        }

        violations
    }

    /// Check only drop-budget SLOs (block drops, event drops).
    ///
    /// Use this in debug-mode tests where latency/throughput targets
    /// don't apply (debug builds are ~100x slower than release).
    pub fn check_drop_budget(&self, stats: &SessionStats) -> Vec<SloViolation> {
        let mut violations = Vec::new();

        if stats.blocks_processed > 0 {
            let drop_rate = stats.blocks_dropped as f64 / stats.blocks_processed as f64;
            if drop_rate > self.drop_budget.max_block_drop_rate {
                violations.push(SloViolation {
                    name: "max_block_drop_rate",
                    measured: drop_rate,
                    threshold: self.drop_budget.max_block_drop_rate,
                });
            }
        }

        if stats.blocks_processed > 0 {
            let event_drop_rate = stats.events_dropped as f64 / stats.blocks_processed as f64;
            if event_drop_rate > self.drop_budget.max_event_drop_rate {
                violations.push(SloViolation {
                    name: "max_event_drop_rate",
                    measured: event_drop_rate,
                    threshold: self.drop_budget.max_event_drop_rate,
                });
            }
        }

        violations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{HealthStatus, LatencyBreakdown};

    fn make_stats(
        blocks_processed: u64,
        blocks_dropped: u64,
        events_dropped: u64,
        total_us: u64,
        fft_us: u64,
        cfar_us: u64,
        throughput_msps: f64,
    ) -> SessionStats {
        SessionStats {
            blocks_processed,
            blocks_dropped,
            processing_time_us: total_us,
            throughput_msps,
            cpu_load_percent: 0.0,
            buffer_occupancy: 0,
            events_dropped,
            health: HealthStatus::Normal,
            latency: LatencyBreakdown {
                dc_removal_us: 0,
                fft_us,
                cfar_us,
                statistics_us: 0,
                decoder_feed_us: 0,
                demod_us: 0,
                total_us,
            },
        }
    }

    #[test]
    fn slo_load_from_toml() {
        let slo = Slo::load();
        assert_eq!(slo.drop_budget.max_block_drop_rate, 0.0);
        assert!(slo.latency.max_block_latency_us > 0);
        assert!(slo.throughput.sustained_throughput_msps > 0.0);
    }

    #[test]
    fn slo_check_passing_stats() {
        let slo = Slo::load();
        let stats = make_stats(1000, 0, 0, 100, 50, 20, 2.1);
        let violations = slo.check_stats(&stats);
        assert!(
            violations.is_empty(),
            "Unexpected violations: {violations:?}"
        );
    }

    #[test]
    fn slo_check_block_drop_violation() {
        let slo = Slo::load();
        let stats = make_stats(1000, 5, 0, 100, 50, 20, 2.1);
        let violations = slo.check_stats(&stats);
        assert!(
            violations.iter().any(|v| v.name == "max_block_drop_rate"),
            "Should flag block drops"
        );
    }

    #[test]
    fn slo_check_event_drop_violation() {
        let slo = Slo::load();
        let stats = make_stats(1000, 0, 100, 100, 50, 20, 2.1);
        let violations = slo.check_stats(&stats);
        assert!(
            violations.iter().any(|v| v.name == "max_event_drop_rate"),
            "Should flag event drops"
        );
    }

    #[test]
    fn slo_check_latency_violation() {
        let slo = Slo::load();
        let stats = make_stats(1000, 0, 0, 1000, 50, 20, 2.1);
        let violations = slo.check_stats(&stats);
        assert!(
            violations.iter().any(|v| v.name == "max_block_latency_us"),
            "Should flag block latency"
        );
    }

    #[test]
    fn slo_check_fft_latency_violation() {
        let slo = Slo::load();
        let stats = make_stats(1000, 0, 0, 100, 300, 20, 2.1);
        let violations = slo.check_stats(&stats);
        assert!(
            violations.iter().any(|v| v.name == "max_fft_latency_us"),
            "Should flag FFT latency"
        );
    }

    #[test]
    fn slo_check_throughput_violation() {
        let slo = Slo::load();
        let stats = make_stats(100, 0, 0, 100, 50, 20, 0.5);
        let violations = slo.check_stats(&stats);
        assert!(
            violations
                .iter()
                .any(|v| v.name == "sustained_throughput_msps"),
            "Should flag low throughput"
        );
    }

    #[test]
    fn slo_zero_blocks_no_violations() {
        let slo = Slo::load();
        let stats = make_stats(0, 0, 0, 0, 0, 0, 0.0);
        let violations = slo.check_stats(&stats);
        // With zero blocks, rate checks are skipped, latency is 0 (under threshold)
        assert!(violations.is_empty());
    }

    #[test]
    fn slo_violation_display() {
        let v = SloViolation {
            name: "test_slo",
            measured: 1.5,
            threshold: 1.0,
        };
        let s = format!("{v}");
        assert!(s.contains("test_slo"));
        assert!(s.contains("1.5"));
    }
}
