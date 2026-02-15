use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use tracing::{debug, info};

use crate::error::HardwareError;
use crate::types::{self, DeviceInfo, Frequency, SampleRate};

use super::traits::{DeviceEnumerator, GainMode, SdrDevice};

/// Number of async read buffers.
const NUM_BUFFERS: u32 = 16;
/// Size of each async read buffer in bytes (256 KiB).
/// At 2.048 MS/s with 2 bytes/sample, each buffer holds ~64ms of data.
const BUFFER_SIZE: u32 = 262_144;

/// RTL-SDR device implementation wrapping `rtlsdr_mt`.
pub struct RtlSdrDevice {
    name: String,
    index: u32,
    controller: Mutex<rtlsdr_mt::Controller>,
    reader: Mutex<Option<rtlsdr_mt::Reader>>,
    streaming: AtomicBool,
    current_freq: Mutex<Frequency>,
    current_rate: Mutex<SampleRate>,
    current_gain: Mutex<GainMode>,
}

impl RtlSdrDevice {
    fn available_gains(controller: &rtlsdr_mt::Controller) -> Vec<f64> {
        let mut gains_buf = [0i32; 32];
        controller
            .tuner_gains(&mut gains_buf)
            .iter()
            .copied()
            .filter(|&g| g != 0)
            .map(|g| g as f64 / 10.0)
            .collect()
    }
}

impl DeviceEnumerator for RtlSdrDevice {
    fn enumerate() -> Result<Vec<DeviceInfo>, HardwareError> {
        let devices: Vec<_> = rtlsdr_mt::devices().collect();

        if devices.is_empty() {
            return Ok(Vec::new());
        }

        let mut infos = Vec::new();
        for (idx, name_cstr) in devices.iter().enumerate() {
            let name = name_cstr.to_string_lossy().into_owned();
            infos.push(DeviceInfo {
                name,
                driver: "rtlsdr".to_string(),
                serial: None,
                index: idx as u32,
                // RTL-SDR typical ranges
                frequency_range: (24_000_000.0, 1_766_000_000.0),
                sample_rate_range: (225_001.0, 3_200_000.0),
                gain_range: (0.0, 49.6),
                available_gains: Vec::new(), // Populated on open
            });
        }

        Ok(infos)
    }

    fn open(index: u32) -> Result<Box<dyn SdrDevice>, HardwareError> {
        info!(index, "Opening RTL-SDR device");

        let (mut controller, reader) =
            rtlsdr_mt::open(index).map_err(|()| HardwareError::DeviceNotFound(index))?;

        // Get device name
        let devices: Vec<_> = rtlsdr_mt::devices().collect();
        let name = devices
            .get(index as usize)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("RTL-SDR #{index}"));

        // Set sane defaults
        let default_rate: u32 = 2_048_000;
        let default_freq: u32 = 100_000_000; // 100 MHz

        controller
            .set_sample_rate(default_rate)
            .map_err(|()| HardwareError::SampleRateError {
                rate: default_rate as f64,
                reason: "failed to set default sample rate".into(),
            })?;

        controller
            .set_center_freq(default_freq)
            .map_err(|()| HardwareError::FrequencyError {
                freq: default_freq as f64,
                reason: "failed to set default frequency".into(),
            })?;

        controller
            .enable_agc()
            .map_err(|()| HardwareError::GainError {
                gain: 0.0,
                reason: "failed to enable AGC".into(),
            })?;

        info!(name, index, "RTL-SDR device opened");

        Ok(Box::new(RtlSdrDevice {
            name,
            index,
            controller: Mutex::new(controller),
            reader: Mutex::new(Some(reader)),
            streaming: AtomicBool::new(false),
            current_freq: Mutex::new(default_freq as f64),
            current_rate: Mutex::new(default_rate as f64),
            current_gain: Mutex::new(GainMode::Auto),
        }))
    }
}

impl SdrDevice for RtlSdrDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn info(&self) -> Result<DeviceInfo, HardwareError> {
        let controller = self.controller.lock();
        let gains = Self::available_gains(&controller);
        let gain_max = gains.last().copied().unwrap_or(49.6);

        Ok(DeviceInfo {
            name: self.name.clone(),
            driver: "rtlsdr".to_string(),
            serial: None,
            index: self.index,
            frequency_range: (24_000_000.0, 1_766_000_000.0),
            sample_rate_range: (225_001.0, 3_200_000.0),
            gain_range: (0.0, gain_max),
            available_gains: gains,
        })
    }

    fn frequency(&self) -> Result<Frequency, HardwareError> {
        Ok(*self.current_freq.lock())
    }

    fn set_frequency(&self, freq: Frequency) -> Result<(), HardwareError> {
        debug!(freq, "Setting center frequency");
        {
            let mut controller = self.controller.lock();
            controller.set_center_freq(freq as u32).map_err(|()| {
                HardwareError::FrequencyError {
                    freq,
                    reason: "rtlsdr set_center_freq failed".into(),
                }
            })?;
        }
        *self.current_freq.lock() = freq;
        Ok(())
    }

    fn sample_rate(&self) -> Result<SampleRate, HardwareError> {
        Ok(*self.current_rate.lock())
    }

    fn set_sample_rate(&self, rate: SampleRate) -> Result<(), HardwareError> {
        debug!(rate, "Setting sample rate");
        {
            let mut controller = self.controller.lock();
            controller.set_sample_rate(rate as u32).map_err(|()| {
                HardwareError::SampleRateError {
                    rate,
                    reason: "rtlsdr set_sample_rate failed".into(),
                }
            })?;
        }
        *self.current_rate.lock() = rate;
        Ok(())
    }

    fn gain(&self) -> Result<GainMode, HardwareError> {
        Ok(*self.current_gain.lock())
    }

    fn set_gain(&self, mode: GainMode) -> Result<(), HardwareError> {
        debug!(?mode, "Setting gain");
        let mut controller = self.controller.lock();
        match mode {
            GainMode::Auto => {
                controller
                    .enable_agc()
                    .map_err(|()| HardwareError::GainError {
                        gain: 0.0,
                        reason: "failed to enable AGC".into(),
                    })?;
            }
            GainMode::Manual(db) => {
                controller
                    .disable_agc()
                    .map_err(|()| HardwareError::GainError {
                        gain: db,
                        reason: "failed to disable AGC".into(),
                    })?;
                // rtlsdr_mt uses tenths of dB
                let gain_tenths = (db * 10.0) as i32;
                controller
                    .set_tuner_gain(gain_tenths)
                    .map_err(|()| HardwareError::GainError {
                        gain: db,
                        reason: "failed to set tuner gain".into(),
                    })?;
            }
        }
        *self.current_gain.lock() = mode;
        Ok(())
    }

    fn set_ppm(&self, ppm: i32) -> Result<(), HardwareError> {
        debug!(ppm, "Setting frequency correction");
        let mut controller = self.controller.lock();
        controller.set_ppm(ppm).map_err(|()| {
            HardwareError::DriverError(format!("failed to set PPM correction to {ppm}"))
        })
    }

    fn start_rx(&self, mut callback: super::RxCallback) -> Result<(), HardwareError> {
        let mut reader_guard = self.reader.lock();
        let mut reader = reader_guard.take().ok_or(HardwareError::StreamError(
            "reader already consumed; device must be re-opened to stream again".into(),
        ))?;

        self.streaming.store(true, Ordering::SeqCst);
        info!("Starting RTL-SDR async read");

        // Drop the lock before blocking
        drop(reader_guard);

        let streaming = &self.streaming;
        let result = reader.read_async(NUM_BUFFERS, BUFFER_SIZE, |raw_bytes| {
            let samples = types::u8_iq_to_samples(raw_bytes);
            callback(&samples);
        });

        streaming.store(false, Ordering::SeqCst);

        result.map_err(|()| HardwareError::StreamError("async read failed".into()))
    }

    fn stop_rx(&self) -> Result<(), HardwareError> {
        if self.streaming.load(Ordering::SeqCst) {
            debug!("Cancelling RTL-SDR async read");
            let mut controller = self.controller.lock();
            controller.cancel_async_read();
        }
        Ok(())
    }

    fn is_streaming(&self) -> bool {
        self.streaming.load(Ordering::SeqCst)
    }
}
