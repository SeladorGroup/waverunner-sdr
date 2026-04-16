//! WaveCore — core DSP, session management, and analysis engine for WaveRunner SDR.
//!
//! This crate provides the signal processing pipeline, hardware abstraction,
//! decoder framework, and session management that powers the WaveRunner
//! software-defined radio platform.

pub mod analysis;
pub mod bookmarks;
pub mod buffer;
pub mod dsp;
pub mod error;
pub mod frequency_db;
pub mod hardware;
pub mod logging;
pub mod migration;
pub mod mode;
pub mod recording;
pub mod session;
pub mod sigmf;
pub mod signal_identify;
pub mod slo;
pub mod types;
pub mod util;

#[cfg(feature = "audio")]
pub mod audio;
