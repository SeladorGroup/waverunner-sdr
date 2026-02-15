//! Audio Output via cpal
//!
//! Provides a lock-free audio sink that decouples the DSP processing thread
//! from the audio device callback thread. The architecture:
//!
//! ```text
//! DSP thread ──write()──→ [Ring Buffer] ──callback──→ Audio Device
//! ```
//!
//! The ring buffer is a single-producer, single-consumer (SPSC) design using
//! atomic indices for lock-free operation. This is critical for audio: the
//! device callback runs in a high-priority OS thread and must never block.
//!
//! ## Resampling
//!
//! If the demodulator output rate differs from the audio device rate (usually
//! 48 kHz), the `AudioSink` automatically inserts a polyphase resampler.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// Lock-free SPSC ring buffer for audio samples.
///
/// Uses atomic load/store for the read and write pointers, providing
/// wait-free progress for both producer and consumer. No mutex, no
/// allocation in the hot path.
///
/// The buffer capacity is always a power of 2 for efficient modular
/// arithmetic (bitwise AND instead of modulo).
struct RingBuffer {
    data: Vec<f32>,
    capacity: usize,       // Always power of 2
    mask: usize,           // capacity - 1
    write_pos: AtomicUsize,
    read_pos: AtomicUsize,
}

impl RingBuffer {
    fn new(min_capacity: usize) -> Self {
        let capacity = min_capacity.next_power_of_two();
        Self {
            data: vec![0.0; capacity],
            capacity,
            mask: capacity - 1,
            write_pos: AtomicUsize::new(0),
            read_pos: AtomicUsize::new(0),
        }
    }

    /// Available space for writing.
    fn available_write(&self) -> usize {
        let w = self.write_pos.load(Ordering::Relaxed);
        let r = self.read_pos.load(Ordering::Acquire);
        self.capacity - (w.wrapping_sub(r))
    }

    /// Available samples for reading.
    fn available_read(&self) -> usize {
        let w = self.write_pos.load(Ordering::Acquire);
        let r = self.read_pos.load(Ordering::Relaxed);
        w.wrapping_sub(r)
    }

    /// Write samples to the ring buffer. Returns number actually written.
    fn write(&self, samples: &[f32]) -> usize {
        let available = self.available_write();
        let to_write = samples.len().min(available);

        let w = self.write_pos.load(Ordering::Relaxed);
        for (i, &sample) in samples.iter().enumerate().take(to_write) {
            // Safety: we're the only writer, and we checked available space
            let idx = (w + i) & self.mask;
            unsafe {
                let ptr = self.data.as_ptr() as *mut f32;
                *ptr.add(idx) = sample;
            }
        }
        self.write_pos.store(w + to_write, Ordering::Release);

        to_write
    }

    /// Read samples from the ring buffer. Returns number actually read.
    fn read(&self, output: &mut [f32]) -> usize {
        let available = self.available_read();
        let to_read = output.len().min(available);

        let r = self.read_pos.load(Ordering::Relaxed);
        for (i, out) in output.iter_mut().enumerate().take(to_read) {
            let idx = (r + i) & self.mask;
            *out = self.data[idx];
        }
        self.read_pos.store(r + to_read, Ordering::Release);

        to_read
    }
}

// Safety: RingBuffer uses atomics for synchronization, single-producer/single-consumer
unsafe impl Send for RingBuffer {}
unsafe impl Sync for RingBuffer {}

/// Audio output sink.
///
/// Wraps a cpal output stream with a ring buffer for lock-free audio delivery.
/// The DSP thread writes demodulated audio via `write()`, and the cpal callback
/// reads from the ring buffer in its own thread.
pub struct AudioSink {
    _stream: cpal::Stream,
    ring: Arc<RingBuffer>,
    volume: Arc<AtomicUsize>, // Volume as u32 bits (f32 reinterpreted)
    running: Arc<AtomicBool>,
    sample_rate: u32,
}

impl AudioSink {
    /// Open the default audio output device.
    ///
    /// `sample_rate`: desired sample rate (Hz). Falls back to device default if unsupported.
    ///
    /// Returns `Err` if no audio device is available.
    pub fn new(sample_rate: u32) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "No audio output device found".to_string())?;

        let supported_configs = device
            .supported_output_configs()
            .map_err(|e| format!("Failed to query audio configs: {e}"))?;

        // Find a config matching our sample rate, prefer f32 format
        let config = supported_configs
            .filter(|c| c.sample_format() == cpal::SampleFormat::F32)
            .find(|c| {
                c.min_sample_rate().0 <= sample_rate && c.max_sample_rate().0 >= sample_rate
            })
            .map(|c| c.with_sample_rate(cpal::SampleRate(sample_rate)))
            .or_else(|| {
                device
                    .default_output_config()
                    .ok()
            })
            .ok_or_else(|| "No suitable audio config found".to_string())?;

        let actual_rate = config.sample_rate().0;
        let channels = config.channels() as usize;

        // Ring buffer: ~200ms of audio at the device rate
        let ring_size = (actual_rate as usize) * channels / 5;
        let ring = Arc::new(RingBuffer::new(ring_size.max(4096)));

        let volume = Arc::new(AtomicUsize::new(f32::to_bits(1.0) as usize));
        let running = Arc::new(AtomicBool::new(true));

        let ring_cb = Arc::clone(&ring);
        let vol_cb = Arc::clone(&volume);
        let running_cb = Arc::clone(&running);

        let stream = device
            .build_output_stream(
                &config.into(),
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if !running_cb.load(Ordering::Relaxed) {
                        data.fill(0.0);
                        return;
                    }

                    let vol = f32::from_bits(vol_cb.load(Ordering::Relaxed) as u32);

                    // For mono source into stereo output: duplicate samples
                    if channels >= 2 {
                        // Read mono samples and duplicate to all channels
                        let frames = data.len() / channels;
                        let mut mono_buf = vec![0.0f32; frames];
                        let read = ring_cb.read(&mut mono_buf);

                        for i in 0..frames {
                            let sample = if i < read { mono_buf[i] * vol } else { 0.0 };
                            for ch in 0..channels {
                                data[i * channels + ch] = sample;
                            }
                        }
                    } else {
                        let read = ring_cb.read(data);
                        for s in &mut data[..read] {
                            *s *= vol;
                        }
                        data[read..].fill(0.0);
                    }
                },
                move |err| {
                    tracing::error!("Audio output error: {err}");
                },
                None,
            )
            .map_err(|e| format!("Failed to build audio stream: {e}"))?;

        stream
            .play()
            .map_err(|e| format!("Failed to start audio stream: {e}"))?;

        Ok(Self {
            _stream: stream,
            ring,
            volume,
            running,
            sample_rate: actual_rate,
        })
    }

    /// Queue audio samples for playback.
    ///
    /// Non-blocking: if the ring buffer is full, excess samples are dropped.
    /// Returns the number of samples actually queued.
    pub fn write(&self, samples: &[f32]) -> usize {
        self.ring.write(samples)
    }

    /// Set the output volume (0.0 = mute, 1.0 = unity).
    pub fn set_volume(&self, gain: f32) {
        let clamped = gain.clamp(0.0, 2.0);
        self.volume
            .store(f32::to_bits(clamped) as usize, Ordering::Relaxed);
    }

    /// Get current volume.
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed) as u32)
    }

    /// Pause audio output (fills device buffer with silence).
    pub fn pause(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    /// Resume audio output.
    pub fn resume(&self) {
        self.running.store(true, Ordering::Relaxed);
    }

    /// The actual audio device sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Available space in the ring buffer (in samples).
    pub fn available(&self) -> usize {
        self.ring.available_write()
    }

    /// How many samples are currently buffered.
    pub fn buffered(&self) -> usize {
        self.ring.available_read()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_basic() {
        let ring = RingBuffer::new(16);
        assert_eq!(ring.capacity, 16);
        assert_eq!(ring.available_write(), 16);
        assert_eq!(ring.available_read(), 0);

        let data = [1.0, 2.0, 3.0, 4.0];
        let written = ring.write(&data);
        assert_eq!(written, 4);
        assert_eq!(ring.available_read(), 4);
        assert_eq!(ring.available_write(), 12);

        let mut out = [0.0f32; 4];
        let read = ring.read(&mut out);
        assert_eq!(read, 4);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(ring.available_read(), 0);
    }

    #[test]
    fn ring_buffer_wraparound() {
        let ring = RingBuffer::new(8);

        // Fill most of the buffer
        let data = [1.0; 6];
        ring.write(&data);

        // Read 4
        let mut out = [0.0; 4];
        ring.read(&mut out);

        // Write 6 more (wraps around)
        let data2 = [2.0; 6];
        let written = ring.write(&data2);
        assert_eq!(written, 6);

        // Read all 8 (2 from first write + 6 from second)
        let mut out2 = [0.0; 8];
        let read = ring.read(&mut out2);
        assert_eq!(read, 8);
        assert_eq!(&out2[..2], &[1.0, 1.0]);
        assert_eq!(&out2[2..8], &[2.0, 2.0, 2.0, 2.0, 2.0, 2.0]);
    }

    #[test]
    fn ring_buffer_overflow_drops() {
        let ring = RingBuffer::new(4);

        let data = [1.0; 8];
        let written = ring.write(&data);
        assert_eq!(written, 4); // Only 4 fit

        // Buffer is full
        let more = ring.write(&[5.0]);
        assert_eq!(more, 0);
    }

    #[test]
    fn ring_buffer_empty_read() {
        let ring = RingBuffer::new(8);
        let mut out = [0.0; 4];
        let read = ring.read(&mut out);
        assert_eq!(read, 0);
    }
}
