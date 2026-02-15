//! Plugin system for Waverunner SDR platform.
//!
//! Phase 0 stub — defines core plugin traits. Plugin loading, sandboxing,
//! and registry will be implemented in Phase 2.

use wavecore::types::SampleBlock;

/// Trait that all signal processing plugins must implement.
pub trait SignalPlugin: Send + Sync {
    /// Unique name identifying this plugin.
    fn name(&self) -> &str;

    /// Version string (semver).
    fn version(&self) -> &str;

    /// Process a block of IQ samples.
    ///
    /// Returns `Some(block)` with the processed output, or `None` if
    /// the plugin consumed the block without producing output.
    fn process(&mut self, block: &SampleBlock) -> Option<SampleBlock>;
}
