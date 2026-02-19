//! Protocol decoder implementations.
//!
//! Each decoder implements the [`DecoderPlugin`](super::decoder::DecoderPlugin) trait
//! and is registered in the [`DecoderRegistry`](super::decoder::DecoderRegistry) via
//! the [`register_all`] function.

pub mod adsb;
pub mod ais;
pub mod aprs;
pub mod noaa_apt;
pub mod ook;
pub mod pocsag;
pub mod rds;
pub mod rtl433;
pub mod util;

use super::decoder::DecoderRegistry;

/// All decoder names exposed by [`register_all`].
///
/// Frontends (TUI, GUI) must use these names — never hard-code
/// decoder names independently.
pub const DECODER_NAMES: &[&str] = &[
    // Phase 2 decoders
    "pocsag",      // alias for pocsag-1200 (most common baud rate)
    "pocsag-512",
    "pocsag-1200",
    "pocsag-2400",
    "adsb",
    "rds",
    // Phase 6 decoders
    "aprs",
    "ais",
    "ais-a",
    "ais-b",
    "ook",
    "ook-weather",
    "ook-tpms",
    "noaa-apt-15",
    "noaa-apt-18",
    "noaa-apt-19",
    // rtl_433 subprocess integration
    "rtl433",
    "rtl433-315",
    "rtl433-868",
    "rtl433-915",
];

/// Register all built-in decoders.
pub fn register_all(registry: &mut DecoderRegistry) {
    // ---- Phase 2 decoders ----

    // "pocsag" is a convenience alias for the most common 1200-baud rate.
    registry.register("pocsag", || {
        Box::new(pocsag::PocsagDecoder::new(pocsag::PocsagBaudRate::Rate1200))
    });
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

    // ---- Phase 6 decoders ----

    // APRS: Bell 202 AFSK on 144.390 MHz
    registry.register("aprs", || {
        Box::new(aprs::AprsDecoder::new(22050.0))
    });

    // AIS: GMSK 9600 baud on VHF maritime channels
    registry.register("ais", || {
        Box::new(ais::AisDecoder::new(48000.0, 161.975e6))
    });
    registry.register("ais-a", || {
        Box::new(ais::AisDecoder::new(48000.0, 161.975e6))
    });
    registry.register("ais-b", || {
        Box::new(ais::AisDecoder::new(48000.0, 162.025e6))
    });

    // OOK: ISM band pulse decoder (433.92 MHz default)
    registry.register("ook", || {
        Box::new(ook::OokDecoder::new(250000.0, 433.92e6, ook::OokFilter::All))
    });
    registry.register("ook-weather", || {
        Box::new(ook::OokDecoder::new(250000.0, 433.92e6, ook::OokFilter::Weather))
    });
    registry.register("ook-tpms", || {
        Box::new(ook::OokDecoder::new(250000.0, 433.92e6, ook::OokFilter::Tpms))
    });

    // NOAA APT: Weather satellite image decoder
    registry.register("noaa-apt-15", || {
        Box::new(noaa_apt::NoaaAptDecoder::new(48000.0, "NOAA-15"))
    });
    registry.register("noaa-apt-18", || {
        Box::new(noaa_apt::NoaaAptDecoder::new(48000.0, "NOAA-18"))
    });
    registry.register("noaa-apt-19", || {
        Box::new(noaa_apt::NoaaAptDecoder::new(48000.0, "NOAA-19"))
    });

    // ---- rtl_433 subprocess integration ----

    // 433.92 MHz ISM band (worldwide, most common)
    registry.register("rtl433", || {
        Box::new(rtl433::Rtl433Decoder::new(250_000.0, 433.92e6))
    });
    // 315 MHz ISM band (US)
    registry.register("rtl433-315", || {
        Box::new(rtl433::Rtl433Decoder::new(250_000.0, 315.0e6))
    });
    // 868 MHz ISM band (EU)
    registry.register("rtl433-868", || {
        Box::new(rtl433::Rtl433Decoder::new(250_000.0, 868.0e6))
    });
    // 915 MHz ISM band (US)
    registry.register("rtl433-915", || {
        Box::new(rtl433::Rtl433Decoder::new(250_000.0, 915.0e6))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name in DECODER_NAMES must resolve to a decoder in a
    /// fully-registered DecoderRegistry.  This catches drift between
    /// the public name list and register_all().
    #[test]
    fn all_decoder_names_resolve_in_registry() {
        let mut registry = DecoderRegistry::new();
        register_all(&mut registry);

        for &name in DECODER_NAMES {
            assert!(
                registry.create(name).is_some(),
                "DECODER_NAMES contains '{}' but register_all() did not register it",
                name,
            );
        }
    }

    /// Every name registered by register_all() must appear in
    /// DECODER_NAMES.  This catches decoders added to register_all()
    /// but forgotten in the public list.
    #[test]
    fn all_registered_decoders_in_names_list() {
        let mut registry = DecoderRegistry::new();
        register_all(&mut registry);

        for name in registry.list() {
            assert!(
                DECODER_NAMES.contains(&name),
                "register_all() registers '{}' but DECODER_NAMES does not include it",
                name,
            );
        }
    }

    /// DECODER_NAMES must not contain duplicates.
    #[test]
    fn decoder_names_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for &name in DECODER_NAMES {
            assert!(
                seen.insert(name),
                "DECODER_NAMES contains duplicate '{}'",
                name,
            );
        }
    }
}
