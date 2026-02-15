use thiserror::Error;

#[derive(Error, Debug)]
pub enum WaveError {
    #[error("Hardware error: {0}")]
    Hardware(#[from] HardwareError),

    #[error("DSP error: {0}")]
    Dsp(#[from] DspError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Channel closed")]
    ChannelClosed,
}

#[derive(Error, Debug)]
pub enum HardwareError {
    #[error("No SDR device found")]
    NoDevice,

    #[error("Device not found at index {0}")]
    DeviceNotFound(u32),

    #[error("Device busy")]
    DeviceBusy,

    #[error("Failed to set frequency to {freq} Hz: {reason}")]
    FrequencyError { freq: f64, reason: String },

    #[error("Failed to set sample rate to {rate} S/s: {reason}")]
    SampleRateError { rate: f64, reason: String },

    #[error("Failed to set gain to {gain}: {reason}")]
    GainError { gain: f64, reason: String },

    #[error("Stream error: {0}")]
    StreamError(String),

    #[error("Driver error: {0}")]
    DriverError(String),
}

#[derive(Error, Debug)]
pub enum DspError {
    #[error("FFT error: {0}")]
    FftError(String),

    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),

    #[error("Buffer overflow")]
    BufferOverflow,
}

pub type Result<T> = std::result::Result<T, WaveError>;
