use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use tracing::warn;

use crate::types::SampleBlock;

/// Configuration for the sample pipeline.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Maximum number of SampleBlocks in the channel before dropping.
    pub buffer_depth: usize,
    /// Whether to drop newest samples on overflow (true) or block the producer (false).
    pub drop_on_overflow: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            buffer_depth: 256,
            drop_on_overflow: true,
        }
    }
}

/// Producer side of the sample pipeline. Held by the hardware reader thread.
pub struct SampleProducer {
    tx: Sender<SampleBlock>,
    drop_on_overflow: bool,
    dropped_count: Arc<AtomicU64>,
}

impl SampleProducer {
    /// Send a sample block into the pipeline.
    ///
    /// If `drop_on_overflow` is true and the channel is full, the block is
    /// dropped and the drop counter is incremented. This prevents the hardware
    /// reader thread from blocking (which would cause USB buffer overflows).
    pub fn send(&self, block: SampleBlock) -> Result<(), crate::error::WaveError> {
        if self.drop_on_overflow {
            match self.tx.try_send(block) {
                Ok(()) => Ok(()),
                Err(TrySendError::Full(_)) => {
                    let count = self.dropped_count.fetch_add(1, Ordering::Relaxed) + 1;
                    if count % 100 == 1 {
                        warn!(count, "Pipeline overflow: dropping sample block");
                    }
                    Ok(())
                }
                Err(TrySendError::Disconnected(_)) => Err(crate::error::WaveError::ChannelClosed),
            }
        } else {
            self.tx
                .send(block)
                .map_err(|_| crate::error::WaveError::ChannelClosed)
        }
    }

    /// Number of sample blocks dropped due to overflow.
    pub fn dropped_count(&self) -> u64 {
        self.dropped_count.load(Ordering::Relaxed)
    }

    /// Get a shared handle to the drop counter for use in other threads.
    pub fn dropped_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.dropped_count)
    }
}

/// Consumer side of the sample pipeline.
pub struct SampleConsumer {
    rx: Receiver<SampleBlock>,
}

impl SampleConsumer {
    /// Block until a sample block is available.
    pub fn recv(&self) -> Result<SampleBlock, crate::error::WaveError> {
        self.rx
            .recv()
            .map_err(|_| crate::error::WaveError::ChannelClosed)
    }

    /// Try to receive a sample block without blocking.
    pub fn try_recv(&self) -> Option<SampleBlock> {
        self.rx.try_recv().ok()
    }

    /// Returns an iterator that yields sample blocks until the channel is closed.
    pub fn iter(&self) -> impl Iterator<Item = SampleBlock> + '_ {
        self.rx.iter()
    }

    /// Number of blocks currently buffered.
    pub fn len(&self) -> usize {
        self.rx.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.rx.is_empty()
    }
}

/// Create a linked producer/consumer pair for the sample pipeline.
pub fn sample_pipeline(config: PipelineConfig) -> (SampleProducer, SampleConsumer) {
    let (tx, rx) = bounded(config.buffer_depth);
    (
        SampleProducer {
            tx,
            drop_on_overflow: config.drop_on_overflow,
            dropped_count: Arc::new(AtomicU64::new(0)),
        },
        SampleConsumer { rx },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Sample, SampleBlock};

    fn make_block(seq: u64) -> SampleBlock {
        SampleBlock {
            samples: vec![Sample::new(0.0, 0.0); 128],
            center_freq: 100e6,
            sample_rate: 2.048e6,
            sequence: seq,
            timestamp_ns: 0,
        }
    }

    #[test]
    fn pipeline_basic_transfer() {
        let (producer, consumer) = sample_pipeline(PipelineConfig::default());
        producer.send(make_block(0)).unwrap();
        producer.send(make_block(1)).unwrap();

        let b0 = consumer.recv().unwrap();
        assert_eq!(b0.sequence, 0);
        let b1 = consumer.recv().unwrap();
        assert_eq!(b1.sequence, 1);
    }

    #[test]
    fn pipeline_overflow_drops() {
        let (producer, consumer) = sample_pipeline(PipelineConfig {
            buffer_depth: 2,
            drop_on_overflow: true,
        });

        // Fill the channel
        producer.send(make_block(0)).unwrap();
        producer.send(make_block(1)).unwrap();

        // This should be dropped, not block
        producer.send(make_block(2)).unwrap();
        assert_eq!(producer.dropped_count(), 1);

        // Consumer should get the first two
        assert_eq!(consumer.recv().unwrap().sequence, 0);
        assert_eq!(consumer.recv().unwrap().sequence, 1);
    }

    #[test]
    fn pipeline_closed_channel() {
        let (producer, consumer) = sample_pipeline(PipelineConfig::default());
        drop(consumer);
        assert!(producer.send(make_block(0)).is_err());
    }

    #[test]
    fn pipeline_threaded() {
        let (producer, consumer) = sample_pipeline(PipelineConfig {
            buffer_depth: 128,
            drop_on_overflow: true,
        });
        let handle = std::thread::spawn(move || {
            for i in 0..100 {
                producer.send(make_block(i)).unwrap();
            }
        });

        let mut received = 0;
        for block in consumer.iter() {
            received += 1;
            if block.sequence == 99 {
                break;
            }
        }
        assert_eq!(received, 100);
        handle.join().unwrap();
    }
}
