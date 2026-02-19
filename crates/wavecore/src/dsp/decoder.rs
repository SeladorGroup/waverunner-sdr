//! Decoder plugin system for protocol demodulation.
//!
//! Decoders process IQ samples and emit structured messages. They run
//! in their own threads, fed via bounded channels that drop on overflow
//! to never block the realtime DSP path.
//!
//! ```text
//! Processing Thread ──bounded channel──→ Decoder Thread ──→ DecodedMessage
//!       │ (never blocks)                    │ (can be slow)
//!       ↓                                   ↓
//!  Spectrum/Audio                     Event channel to frontend
//! ```
//!
//! ## Adding a new decoder
//!
//! 1. Implement `DecoderPlugin` for your struct
//! 2. Register a factory in `DecoderRegistry`
//! 3. The SessionManager handles thread spawning and channel management

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, TrySendError};

use crate::session::DecodedMessage;
use crate::types::Sample;

/// Requirements a decoder declares for its input signal.
///
/// The SessionManager uses these to auto-configure the DDC chain:
/// tune to `center_frequency`, decimate to `sample_rate`, filter to `bandwidth`.
#[derive(Debug, Clone)]
pub struct DecoderRequirements {
    /// Required center frequency in Hz (e.g., 929.6125 MHz for POCSAG).
    pub center_frequency: f64,
    /// Minimum sample rate needed by the decoder.
    pub sample_rate: f64,
    /// Channel bandwidth in Hz.
    pub bandwidth: f64,
    /// Whether the decoder wants complex IQ samples (true) or real audio (false).
    pub wants_iq: bool,
}

/// Trait for protocol decoder plugins.
///
/// Each decoder processes blocks of IQ samples and returns zero or more
/// decoded messages. Decoders maintain internal state (sync, bit buffers,
/// error correction state) across calls to `process()`.
pub trait DecoderPlugin: Send {
    /// Human-readable name (e.g., "POCSAG", "ADS-B", "RDS").
    fn name(&self) -> &str;

    /// Input signal requirements for this decoder.
    fn requirements(&self) -> DecoderRequirements;

    /// Process a block of IQ samples, returning decoded messages.
    ///
    /// Called repeatedly with consecutive sample blocks. The decoder must
    /// handle block boundaries (partial frames spanning blocks).
    fn process(&mut self, samples: &[Sample]) -> Vec<DecodedMessage>;

    /// Reset decoder state (e.g., when frequency changes).
    fn reset(&mut self);
}

/// Factory type for creating decoder instances.
pub type DecoderFactory = Box<dyn Fn() -> Box<dyn DecoderPlugin> + Send + Sync>;

/// Registry of available decoder plugins.
///
/// Stores factory functions that create decoder instances on demand.
/// The SessionManager queries the registry when `EnableDecoder` is received.
pub struct DecoderRegistry {
    factories: HashMap<String, DecoderFactory>,
}

impl DecoderRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    /// Register a decoder factory under a name.
    pub fn register<F>(&mut self, name: &str, factory: F)
    where
        F: Fn() -> Box<dyn DecoderPlugin> + Send + Sync + 'static,
    {
        self.factories.insert(name.to_string(), Box::new(factory));
    }

    /// Create a new decoder instance by name.
    pub fn create(&self, name: &str) -> Option<Box<dyn DecoderPlugin>> {
        self.factories.get(name).map(|f| f())
    }

    /// List all registered decoder names.
    pub fn list(&self) -> Vec<&str> {
        self.factories.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for DecoderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle for a running decoder thread.
///
/// The SessionManager holds one of these per active decoder. Samples
/// are sent via a bounded channel; if the decoder can't keep up,
/// old samples are dropped (never blocks the realtime path).
pub struct DecoderHandle {
    /// Thread handle for the decoder.
    thread: Option<JoinHandle<()>>,
    /// Channel to send sample blocks to the decoder thread.
    sample_tx: Option<Sender<Vec<Sample>>>,
    /// Flag to signal the decoder thread to stop.
    running: Arc<AtomicBool>,
    /// Name of the decoder.
    name: String,
}

impl DecoderHandle {
    /// Spawn a decoder in its own thread.
    ///
    /// `event_tx` is used by the decoder thread to send decoded messages
    /// back to the SessionManager's event channel.
    ///
    /// The bounded channel has capacity `buffer_depth`. If full, new
    /// samples are dropped (try_send) to prevent blocking.
    pub fn spawn(
        mut decoder: Box<dyn DecoderPlugin>,
        event_tx: Sender<DecodedMessage>,
        buffer_depth: usize,
    ) -> Self {
        let (sample_tx, sample_rx): (Sender<Vec<Sample>>, Receiver<Vec<Sample>>) =
            crossbeam_channel::bounded(buffer_depth);
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);
        let name = decoder.name().to_string();

        let thread = std::thread::Builder::new()
            .name(format!("decoder-{}", &name))
            .spawn(move || {
                while running_clone.load(Ordering::Relaxed) {
                    match sample_rx.recv_timeout(std::time::Duration::from_millis(100)) {
                        Ok(samples) => {
                            let messages = decoder.process(&samples);
                            for msg in messages {
                                if event_tx.send(msg).is_err() {
                                    // Event channel closed, stop decoder
                                    return;
                                }
                            }
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
                    }
                }
            });

        match thread {
            Ok(thread) => Self {
                thread: Some(thread),
                sample_tx: Some(sample_tx),
                running,
                name,
            },
            Err(e) => {
                tracing::error!("Failed to spawn decoder thread for {name}: {e}");
                // Return a handle that is already stopped — safe to call stop() on
                running.store(false, Ordering::Relaxed);
                Self {
                    thread: None,
                    sample_tx: None,
                    running,
                    name,
                }
            }
        }
    }

    /// Returns true if the decoder thread is alive and accepting samples.
    pub fn is_alive(&self) -> bool {
        match &self.thread {
            Some(t) => !t.is_finished() && self.running.load(Ordering::Relaxed),
            None => false,
        }
    }

    /// Send a sample block to the decoder thread.
    ///
    /// If the decoder's input buffer is full, the samples are dropped
    /// silently. This ensures the realtime DSP path is never blocked.
    pub fn feed(&self, samples: Vec<Sample>) -> bool {
        let Some(ref tx) = self.sample_tx else {
            return false;
        };
        match tx.try_send(samples) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                // Decoder can't keep up — drop samples, don't block
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Get the decoder name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Stop the decoder thread and wait for it to finish.
    pub fn stop(mut self) {
        self.running.store(false, Ordering::Relaxed);
        // Drop the sender to unblock recv
        self.sample_tx.take();
        if let Some(thread) = self.thread.take() {
            thread.join().ok();
        }
    }
}

impl Drop for DecoderHandle {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        // Thread will exit on next recv timeout or channel disconnect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Minimal test decoder that counts samples.
    struct CountDecoder {
        total: usize,
    }

    impl CountDecoder {
        fn new() -> Self {
            Self { total: 0 }
        }
    }

    impl DecoderPlugin for CountDecoder {
        fn name(&self) -> &str {
            "count"
        }

        fn requirements(&self) -> DecoderRequirements {
            DecoderRequirements {
                center_frequency: 100e6,
                sample_rate: 48000.0,
                bandwidth: 10000.0,
                wants_iq: true,
            }
        }

        fn process(&mut self, samples: &[Sample]) -> Vec<DecodedMessage> {
            self.total += samples.len();
            if self.total >= 1000 {
                let msg = DecodedMessage {
                    decoder: "count".to_string(),
                    timestamp: Instant::now(),
                    summary: format!("{} samples processed", self.total),
                    fields: std::collections::BTreeMap::new(),
                    raw_bits: None,
                };
                self.total = 0;
                vec![msg]
            } else {
                vec![]
            }
        }

        fn reset(&mut self) {
            self.total = 0;
        }
    }

    #[test]
    fn registry_create_and_list() {
        let mut reg = DecoderRegistry::new();
        reg.register("count", || Box::new(CountDecoder::new()));

        let names = reg.list();
        assert!(names.contains(&"count"));

        let decoder = reg.create("count");
        assert!(decoder.is_some());
        assert_eq!(decoder.unwrap().name(), "count");

        assert!(reg.create("nonexistent").is_none());
    }

    #[test]
    fn decoder_handle_spawn_and_stop() {
        let (msg_tx, msg_rx) = crossbeam_channel::unbounded();
        let decoder = Box::new(CountDecoder::new());
        let handle = DecoderHandle::spawn(decoder, msg_tx, 16);

        // Feed some samples
        let samples: Vec<Sample> = (0..500).map(|i| Sample::new(i as f32 / 500.0, 0.0)).collect();
        assert!(handle.feed(samples.clone()));
        assert!(handle.feed(samples));

        // Wait a bit for decoder thread to process
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Should have received a message (1000 samples triggers output)
        let msg = msg_rx.try_recv();
        assert!(msg.is_ok());
        assert_eq!(msg.unwrap().decoder, "count");

        handle.stop();
    }

    #[test]
    fn decoder_handle_feed_overflow() {
        let (msg_tx, _msg_rx) = crossbeam_channel::unbounded();

        // Create a decoder that sleeps to simulate slow processing
        struct SlowDecoder;
        impl DecoderPlugin for SlowDecoder {
            fn name(&self) -> &str { "slow" }
            fn requirements(&self) -> DecoderRequirements {
                DecoderRequirements {
                    center_frequency: 100e6,
                    sample_rate: 48000.0,
                    bandwidth: 10000.0,
                    wants_iq: true,
                }
            }
            fn process(&mut self, _samples: &[Sample]) -> Vec<DecodedMessage> {
                std::thread::sleep(std::time::Duration::from_millis(100));
                vec![]
            }
            fn reset(&mut self) {}
        }

        let handle = DecoderHandle::spawn(Box::new(SlowDecoder), msg_tx, 2);

        // Fill the bounded channel
        let samples: Vec<Sample> = vec![Sample::new(0.0, 0.0); 10];
        handle.feed(samples.clone());
        handle.feed(samples.clone());
        handle.feed(samples.clone());

        // Third feed should overflow (buffer_depth=2) — returns false
        let result = handle.feed(samples);
        assert!(!result);

        handle.stop();
    }

    #[test]
    fn decoder_plugin_trait() {
        let mut decoder = CountDecoder::new();
        assert_eq!(decoder.name(), "count");
        assert!(decoder.requirements().wants_iq);

        let samples: Vec<Sample> = vec![Sample::new(1.0, 0.0); 500];
        let msgs = decoder.process(&samples);
        assert!(msgs.is_empty()); // Not enough yet

        let msgs = decoder.process(&samples);
        assert_eq!(msgs.len(), 1); // 1000 total triggers message

        decoder.reset();
        let msgs = decoder.process(&samples);
        assert!(msgs.is_empty()); // Reset counter
    }
}
