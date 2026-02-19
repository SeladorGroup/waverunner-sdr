//! Signal analysis tools for WaveRunner.
//!
//! Provides on-demand analysis capabilities: RF measurements, burst detection,
//! modulation estimation, bitstream inspection, time-series tracking,
//! spectrum comparison, and data export.
//!
//! Analysis operations are triggered by user action (not per-block).
//! Only `SignalTracker::push()` runs in the real-time processing loop.

pub mod measurement;
pub mod tracking;
pub mod comparison;
pub mod burst;
pub mod modulation;
pub mod bitstream;
pub mod export;
pub mod report;

/// Unique ID for correlating analysis request/response pairs.
pub type AnalysisId = u64;

/// Result of an analysis computation, returned via `Event::AnalysisResult`.
#[derive(Debug, Clone, serde::Serialize)]
pub enum AnalysisResult {
    /// RF measurement report (bandwidth, channel power, ACPR).
    Measurement(measurement::MeasurementReport),
    /// Burst/pulse analysis report.
    Burst(burst::BurstReport),
    /// Modulation estimation report.
    Modulation(modulation::ModulationReport),
    /// Spectrum comparison report.
    Comparison(comparison::ComparisonReport),
    /// Bitstream inspection report.
    Bitstream(bitstream::BitstreamReport),
    /// Time-series tracking snapshot.
    Tracking(tracking::TrackingSnapshot),
    /// Export completed successfully.
    ExportComplete {
        /// Path to the exported file.
        path: String,
        /// Export format used.
        format: String,
    },
}

/// Configuration for what analysis to perform.
#[derive(Debug, Clone)]
pub enum AnalysisRequest {
    /// Measure bandwidth, channel power, ACPR on current signal.
    MeasureSignal(measurement::MeasureConfig),
    /// Analyze burst/pulse characteristics from captured IQ.
    AnalyzeBurst(burst::BurstConfig),
    /// Estimate modulation parameters.
    EstimateModulation(modulation::ModulationConfig),
    /// Compare current spectrum against captured reference.
    CompareSpectra,
    /// Inspect bitstream from decoder raw_bits.
    InspectBitstream(bitstream::BitstreamConfig),
    /// Take a snapshot of accumulated tracking data.
    TrackingSnapshot,
    /// Export analysis data to file.
    Export(export::ExportConfig),
}
