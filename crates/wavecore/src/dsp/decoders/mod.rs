//! Protocol decoder implementations.
//!
//! Each decoder implements the [`DecoderPlugin`](super::decoder::DecoderPlugin) trait
//! and is registered in the [`DecoderRegistry`](super::decoder::DecoderRegistry) via
//! the [`register_all`] function.

pub mod adsb;
pub mod pocsag;
pub mod rds;

use super::decoder::DecoderRegistry;

/// Register all built-in decoders.
pub fn register_all(registry: &mut DecoderRegistry) {
    registry.register("pocsag-512", || {
        Box::new(pocsag::PocsagDecoder::new(pocsag::PocsagBaudRate::Rate512))
    });
    registry.register("pocsag-1200", || {
        Box::new(pocsag::PocsagDecoder::new(pocsag::PocsagBaudRate::Rate1200))
    });
    registry.register("pocsag-2400", || {
        Box::new(pocsag::PocsagDecoder::new(pocsag::PocsagBaudRate::Rate2400))
    });
    registry.register("adsb", || {
        Box::new(adsb::AdsbDecoder::new())
    });
    registry.register("rds", || {
        Box::new(rds::RdsDecoder::new(228000.0))
    });
}
