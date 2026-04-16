//! Protocol decoder implementations.
//!
//! Each decoder implements the [`DecoderPlugin`](super::decoder::DecoderPlugin) trait
//! and is registered in the [`DecoderRegistry`] via
//! the [`register_all`] function.
//!
//! ## External Tool Bridges
//!
//! Most decoders delegate to battle-tested external tools:
//! - `rtl_433` — OOK/FSK sensors (433/315/868/915 MHz)
//! - `redsea` — FM broadcast RDS/RBDS
//! - `multimon-ng` — POCSAG, APRS, DTMF, EAS, FLEX
//! - `dump1090` — ADS-B aircraft tracking
//!
//! Run `waverunner tools` to see which tools are installed.

pub mod adsb;
pub mod ais;
pub mod aprs;
pub mod multimon;
pub mod noaa_apt;
pub mod ook;
pub mod pocsag;
pub mod rds;
pub mod rtl433;
pub mod subprocess;
pub mod tools;
pub mod util;

use super::decoder::DecoderRegistry;

/// Decoder implementation backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecoderBackend {
    Native,
    External,
}

impl DecoderBackend {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::External => "external",
        }
    }
}

/// Public metadata for each decoder exposed by [`register_all`].
pub struct DecoderDescriptor {
    pub name: &'static str,
    pub backend: DecoderBackend,
    pub required_tool: Option<&'static str>,
    pub summary: &'static str,
}

/// Stable decoder metadata for CLI/TUI/GUI surfaces.
pub const DECODER_DESCRIPTORS: &[DecoderDescriptor] = &[
    DecoderDescriptor {
        name: "rds",
        backend: DecoderBackend::External,
        required_tool: Some("redsea"),
        summary: "FM broadcast RDS/RBDS via redsea",
    },
    DecoderDescriptor {
        name: "pocsag",
        backend: DecoderBackend::External,
        required_tool: Some("multimon-ng"),
        summary: "POCSAG pager decoding via multimon-ng at 1200 baud",
    },
    DecoderDescriptor {
        name: "pocsag-512",
        backend: DecoderBackend::External,
        required_tool: Some("multimon-ng"),
        summary: "POCSAG pager decoding via multimon-ng at 512 baud",
    },
    DecoderDescriptor {
        name: "pocsag-1200",
        backend: DecoderBackend::External,
        required_tool: Some("multimon-ng"),
        summary: "POCSAG pager decoding via multimon-ng at 1200 baud",
    },
    DecoderDescriptor {
        name: "pocsag-2400",
        backend: DecoderBackend::External,
        required_tool: Some("multimon-ng"),
        summary: "POCSAG pager decoding via multimon-ng at 2400 baud",
    },
    DecoderDescriptor {
        name: "aprs",
        backend: DecoderBackend::External,
        required_tool: Some("multimon-ng"),
        summary: "APRS AFSK1200 via multimon-ng",
    },
    DecoderDescriptor {
        name: "dtmf",
        backend: DecoderBackend::External,
        required_tool: Some("multimon-ng"),
        summary: "DTMF tone decoding via multimon-ng",
    },
    DecoderDescriptor {
        name: "eas",
        backend: DecoderBackend::External,
        required_tool: Some("multimon-ng"),
        summary: "EAS/SAME alert headers via multimon-ng",
    },
    DecoderDescriptor {
        name: "flex",
        backend: DecoderBackend::External,
        required_tool: Some("multimon-ng"),
        summary: "FLEX pager decoding via multimon-ng",
    },
    DecoderDescriptor {
        name: "adsb",
        backend: DecoderBackend::External,
        required_tool: Some("dump1090"),
        summary: "ADS-B / Mode-S via a dump1090-compatible backend",
    },
    DecoderDescriptor {
        name: "rtl433",
        backend: DecoderBackend::External,
        required_tool: Some("rtl_433"),
        summary: "433.92 MHz ISM sensors via rtl_433",
    },
    DecoderDescriptor {
        name: "rtl433-315",
        backend: DecoderBackend::External,
        required_tool: Some("rtl_433"),
        summary: "315 MHz ISM sensors via rtl_433",
    },
    DecoderDescriptor {
        name: "rtl433-868",
        backend: DecoderBackend::External,
        required_tool: Some("rtl_433"),
        summary: "868 MHz ISM sensors via rtl_433",
    },
    DecoderDescriptor {
        name: "rtl433-915",
        backend: DecoderBackend::External,
        required_tool: Some("rtl_433"),
        summary: "915 MHz ISM sensors via rtl_433",
    },
    DecoderDescriptor {
        name: "ais",
        backend: DecoderBackend::Native,
        required_tool: None,
        summary: "AIS maritime traffic, defaulting to channel A",
    },
    DecoderDescriptor {
        name: "ais-a",
        backend: DecoderBackend::Native,
        required_tool: None,
        summary: "AIS maritime traffic on channel A (161.975 MHz)",
    },
    DecoderDescriptor {
        name: "ais-b",
        backend: DecoderBackend::Native,
        required_tool: None,
        summary: "AIS maritime traffic on channel B (162.025 MHz)",
    },
    DecoderDescriptor {
        name: "ook",
        backend: DecoderBackend::Native,
        required_tool: None,
        summary: "Native OOK/ASK device decoding",
    },
    DecoderDescriptor {
        name: "ook-weather",
        backend: DecoderBackend::Native,
        required_tool: None,
        summary: "Native OOK weather sensor filtering",
    },
    DecoderDescriptor {
        name: "ook-tpms",
        backend: DecoderBackend::Native,
        required_tool: None,
        summary: "Native OOK TPMS filtering",
    },
    DecoderDescriptor {
        name: "noaa-apt-15",
        backend: DecoderBackend::Native,
        required_tool: None,
        summary: "NOAA-15 APT weather satellite imagery",
    },
    DecoderDescriptor {
        name: "noaa-apt-18",
        backend: DecoderBackend::Native,
        required_tool: None,
        summary: "NOAA-18 APT weather satellite imagery",
    },
    DecoderDescriptor {
        name: "noaa-apt-19",
        backend: DecoderBackend::Native,
        required_tool: None,
        summary: "NOAA-19 APT weather satellite imagery",
    },
];

/// All decoder names exposed by [`register_all`].
///
/// Frontends (TUI, GUI) must use these names — never hard-code
/// decoder names independently.
pub const DECODER_NAMES: &[&str] = &[
    "rds",
    "pocsag",
    "pocsag-512",
    "pocsag-1200",
    "pocsag-2400",
    "aprs",
    "dtmf",
    "eas",
    "flex",
    "adsb",
    "rtl433",
    "rtl433-315",
    "rtl433-868",
    "rtl433-915",
    "ais",
    "ais-a",
    "ais-b",
    "ook",
    "ook-weather",
    "ook-tpms",
    "noaa-apt-15",
    "noaa-apt-18",
    "noaa-apt-19",
];

/// Look up public decoder metadata by name.
pub fn decoder_descriptor(name: &str) -> Option<&'static DecoderDescriptor> {
    DECODER_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.name == name)
}

/// Register all decoder factories.
pub fn register_all(registry: &mut DecoderRegistry) {
    // ---- redsea bridge ----
    registry.register("rds", || Box::new(rds::RdsDecoder::new(2_048_000.0)));

    // ---- multimon-ng bridges ----
    registry.register("pocsag", || {
        Box::new(pocsag::PocsagDecoder::named(
            pocsag::PocsagBaudRate::Rate1200,
            "pocsag",
        ))
    });
    registry.register("pocsag-512", || {
        Box::new(pocsag::PocsagDecoder::named(
            pocsag::PocsagBaudRate::Rate512,
            "pocsag-512",
        ))
    });
    registry.register("pocsag-1200", || {
        Box::new(pocsag::PocsagDecoder::named(
            pocsag::PocsagBaudRate::Rate1200,
            "pocsag-1200",
        ))
    });
    registry.register("pocsag-2400", || {
        Box::new(pocsag::PocsagDecoder::named(
            pocsag::PocsagBaudRate::Rate2400,
            "pocsag-2400",
        ))
    });
    registry.register("aprs", || Box::new(aprs::AprsDecoder::new(22050.0)));
    registry.register("dtmf", || {
        Box::new(multimon::MultimonDecoder::new(
            multimon::MultimonProtocol::Dtmf,
        ))
    });
    registry.register("eas", || {
        Box::new(multimon::MultimonDecoder::new(
            multimon::MultimonProtocol::Eas,
        ))
    });
    registry.register("flex", || {
        Box::new(multimon::MultimonDecoder::new(
            multimon::MultimonProtocol::Flex,
        ))
    });

    // ---- dump1090 bridge ----
    registry.register("adsb", || Box::new(adsb::AdsbDecoder::new()));

    // ---- rtl_433 bridges ----
    registry.register("rtl433", || {
        Box::new(rtl433::Rtl433Decoder::named(250_000.0, 433.92e6, "rtl433"))
    });
    registry.register("rtl433-315", || {
        Box::new(rtl433::Rtl433Decoder::named(
            250_000.0,
            315.0e6,
            "rtl433-315",
        ))
    });
    registry.register("rtl433-868", || {
        Box::new(rtl433::Rtl433Decoder::named(
            250_000.0,
            868.0e6,
            "rtl433-868",
        ))
    });
    registry.register("rtl433-915", || {
        Box::new(rtl433::Rtl433Decoder::named(
            250_000.0,
            915.0e6,
            "rtl433-915",
        ))
    });

    // ---- Built-in (experimental, no external tool) ----
    registry.register("ais", || {
        Box::new(ais::AisDecoder::named(48000.0, 161.975e6, "ais"))
    });
    registry.register("ais-a", || {
        Box::new(ais::AisDecoder::named(48000.0, 161.975e6, "ais-a"))
    });
    registry.register("ais-b", || {
        Box::new(ais::AisDecoder::named(48000.0, 162.025e6, "ais-b"))
    });

    registry.register("ook", || {
        Box::new(ook::OokDecoder::new(
            250000.0,
            433.92e6,
            ook::OokFilter::All,
        ))
    });
    registry.register("ook-weather", || {
        Box::new(ook::OokDecoder::new(
            250000.0,
            433.92e6,
            ook::OokFilter::Weather,
        ))
    });
    registry.register("ook-tpms", || {
        Box::new(ook::OokDecoder::new(
            250000.0,
            433.92e6,
            ook::OokFilter::Tpms,
        ))
    });

    registry.register("noaa-apt-15", || {
        Box::new(noaa_apt::NoaaAptDecoder::new(48000.0, "NOAA-15"))
    });
    registry.register("noaa-apt-18", || {
        Box::new(noaa_apt::NoaaAptDecoder::new(48000.0, "NOAA-18"))
    });
    registry.register("noaa-apt-19", || {
        Box::new(noaa_apt::NoaaAptDecoder::new(48000.0, "NOAA-19"))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn all_decoder_names_have_descriptors() {
        for &name in DECODER_NAMES {
            assert!(
                decoder_descriptor(name).is_some(),
                "DECODER_NAMES contains '{}' but DECODER_DESCRIPTORS does not include it",
                name,
            );
        }
    }

    #[test]
    fn all_descriptors_match_names_list() {
        for descriptor in DECODER_DESCRIPTORS {
            assert!(
                DECODER_NAMES.contains(&descriptor.name),
                "DECODER_DESCRIPTORS contains '{}' but DECODER_NAMES does not include it",
                descriptor.name,
            );
        }
    }

    #[test]
    fn registered_decoders_report_registered_runtime_names() {
        let mut registry = DecoderRegistry::new();
        register_all(&mut registry);

        for &name in DECODER_NAMES {
            let decoder = registry
                .create(name)
                .unwrap_or_else(|| panic!("missing decoder for {name}"));
            assert_eq!(
                decoder.name(),
                name,
                "decoder '{}' reports runtime name '{}'",
                name,
                decoder.name(),
            );
        }
    }
}
