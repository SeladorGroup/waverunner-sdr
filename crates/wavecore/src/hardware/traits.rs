use crate::error::HardwareError;
use crate::types::{DeviceInfo, Frequency, Sample, SampleRate};

/// Callback type for streaming received samples.
pub type RxCallback = Box<dyn FnMut(&[Sample]) + Send>;

/// Gain control mode.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum GainMode {
    /// Automatic gain control.
    Auto,
    /// Manual gain in dB.
    Manual(f64),
}

/// Core trait for any SDR device.
///
/// All methods take `&self` with interior mutability, matching SoapySDR's
/// model and enabling `Arc<dyn SdrDevice>` sharing across threads.
pub trait SdrDevice: Send + Sync {
    /// Human-readable device name.
    fn name(&self) -> &str;

    /// Full device information including capabilities.
    fn info(&self) -> Result<DeviceInfo, HardwareError>;

    /// Get current center frequency in Hz.
    fn frequency(&self) -> Result<Frequency, HardwareError>;

    /// Set center frequency in Hz.
    fn set_frequency(&self, freq: Frequency) -> Result<(), HardwareError>;

    /// Get current sample rate in samples/second.
    fn sample_rate(&self) -> Result<SampleRate, HardwareError>;

    /// Set sample rate in samples/second.
    fn set_sample_rate(&self, rate: SampleRate) -> Result<(), HardwareError>;

    /// Get current gain mode and value.
    fn gain(&self) -> Result<GainMode, HardwareError>;

    /// Set gain mode (Auto or Manual with dB value).
    fn set_gain(&self, mode: GainMode) -> Result<(), HardwareError>;

    /// Set frequency correction in PPM.
    fn set_ppm(&self, ppm: i32) -> Result<(), HardwareError>;

    /// Start receiving samples. The callback is called with each buffer
    /// of IQ samples (already converted to Complex<f32>).
    ///
    /// This method **blocks** the calling thread until `stop_rx()` is called
    /// from another thread or an error occurs. Typically called from a
    /// dedicated thread.
    fn start_rx(&self, callback: RxCallback) -> Result<(), HardwareError>;

    /// Signal the device to stop receiving. Safe to call from another thread
    /// while `start_rx` is blocking.
    fn stop_rx(&self) -> Result<(), HardwareError>;

    /// Check if device is currently streaming.
    fn is_streaming(&self) -> bool;
}

/// Enumerate and open SDR devices of a specific type.
pub trait DeviceEnumerator {
    /// List all devices of this type currently connected.
    fn enumerate() -> Result<Vec<DeviceInfo>, HardwareError>
    where
        Self: Sized;

    /// Open device by index.
    fn open(index: u32) -> Result<Box<dyn SdrDevice>, HardwareError>
    where
        Self: Sized;
}
