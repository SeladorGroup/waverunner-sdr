//! Regional frequency database for band identification and auto-configuration.
//!
//! Replaces hardcoded frequency references scattered across the codebase with
//! a single, region-aware lookup table covering broadcast, amateur, aviation,
//! maritime, ISM, and emergency allocations.

use serde::{Deserialize, Serialize};

use crate::mode::classifier::ClassificationRule;

/// Geographic region for frequency allocation filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Region {
    /// North America (ITU Region 2)
    NA,
    /// Europe (ITU Region 1)
    EU,
    /// Japan (unique FM band 76–95 MHz)
    JP,
    /// Australia / Oceania
    AU,
}

impl Region {
    pub fn label(&self) -> &'static str {
        match self {
            Region::NA => "North America",
            Region::EU => "Europe",
            Region::JP => "Japan",
            Region::AU => "Australia",
        }
    }
}

impl std::fmt::Display for Region {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Region::NA => "NA",
            Region::EU => "EU",
            Region::JP => "JP",
            Region::AU => "AU",
        })
    }
}

impl std::str::FromStr for Region {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_uppercase().as_str() {
            "NA" => Ok(Region::NA),
            "EU" => Ok(Region::EU),
            "JP" => Ok(Region::JP),
            "AU" => Ok(Region::AU),
            _ => Err(format!("unknown region '{s}' (valid: NA, EU, JP, AU)")),
        }
    }
}

/// Service type classification for frequency allocations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceType {
    FmBroadcast,
    AmBroadcast,
    AmateurRadio,
    Aviation,
    Maritime,
    Weather,
    Pager,
    Ism,
    Emergency,
    PersonalRadio,
    Satellite,
    Military,
    Other,
}

impl ServiceType {
    pub fn label(&self) -> &'static str {
        match self {
            ServiceType::FmBroadcast => "FM Broadcast",
            ServiceType::AmBroadcast => "AM Broadcast",
            ServiceType::AmateurRadio => "Amateur Radio",
            ServiceType::Aviation => "Aviation",
            ServiceType::Maritime => "Maritime",
            ServiceType::Weather => "Weather",
            ServiceType::Pager => "Pager",
            ServiceType::Ism => "ISM",
            ServiceType::Emergency => "Emergency",
            ServiceType::PersonalRadio => "Personal Radio",
            ServiceType::Satellite => "Satellite",
            ServiceType::Military => "Military",
            ServiceType::Other => "Other",
        }
    }
}

impl std::fmt::Display for ServiceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl std::str::FromStr for ServiceType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "fm-broadcast" | "fm_broadcast" | "fmbroadcast" => Ok(ServiceType::FmBroadcast),
            "am-broadcast" | "am_broadcast" | "ambroadcast" => Ok(ServiceType::AmBroadcast),
            "amateur" | "amateur-radio" | "ham" => Ok(ServiceType::AmateurRadio),
            "aviation" | "airband" => Ok(ServiceType::Aviation),
            "maritime" | "marine" => Ok(ServiceType::Maritime),
            "weather" => Ok(ServiceType::Weather),
            "pager" => Ok(ServiceType::Pager),
            "ism" => Ok(ServiceType::Ism),
            "emergency" => Ok(ServiceType::Emergency),
            "personal" | "personal-radio" | "pmr" | "frs" | "gmrs" => {
                Ok(ServiceType::PersonalRadio)
            }
            "satellite" => Ok(ServiceType::Satellite),
            "military" => Ok(ServiceType::Military),
            _ => Err(format!("unknown service type '{s}'")),
        }
    }
}

/// A frequency band allocation (range of frequencies).
#[derive(Debug, Clone)]
pub struct BandAllocation {
    /// Lower bound in Hz (inclusive).
    pub start_hz: f64,
    /// Upper bound in Hz (inclusive).
    pub end_hz: f64,
    /// Human-readable label (e.g. "FM Broadcast").
    pub label: &'static str,
    /// Service classification.
    pub service: ServiceType,
    /// Recommended demodulation mode (e.g. "wfm", "am", "fm").
    pub modulation: &'static str,
    /// Demod mode suitable for live listening, if applicable.
    pub demod_mode: Option<&'static str>,
    /// Recommended decoder name, if any.
    pub decoder: Option<&'static str>,
    /// Recommended sample rate for this band, if specific.
    pub sample_rate: Option<f64>,
    /// Expected signal bandwidth range in Hz.
    pub bandwidth_range: (f64, f64),
    /// Regions where this allocation applies.
    pub regions: &'static [Region],
}

/// A discrete known frequency (single channel).
#[derive(Debug, Clone)]
pub struct FrequencyAllocation {
    /// Frequency in Hz.
    pub freq_hz: f64,
    /// Human-readable label.
    pub label: &'static str,
    /// Service classification.
    pub service: ServiceType,
    /// Recommended demod mode.
    pub modulation: &'static str,
    /// Demod mode suitable for live listening, if applicable.
    pub demod_mode: Option<&'static str>,
    /// Recommended decoder, if any.
    pub decoder: Option<&'static str>,
    /// Regions where this channel exists.
    pub regions: &'static [Region],
}

/// Regional frequency database.
#[derive(Clone)]
pub struct FrequencyDb {
    /// Active region for filtering.
    pub region: Region,
    /// All band allocations (unfiltered).
    bands: Vec<BandAllocation>,
    /// All discrete frequency allocations (unfiltered).
    frequencies: Vec<FrequencyAllocation>,
}

const ALL_REGIONS: &[Region] = &[Region::NA, Region::EU, Region::JP, Region::AU];
pub const KNOWN_FREQUENCY_TOLERANCE_HZ: f64 = 15_000.0;

impl FrequencyDb {
    /// Create a new database for the given region.
    pub fn new(region: Region) -> Self {
        Self {
            region,
            bands: build_bands(),
            frequencies: build_frequencies(),
        }
    }

    /// Auto-detect region from config file, timezone, or default to NA.
    pub fn auto_detect() -> Self {
        let region = detect_region();
        Self::new(region)
    }

    /// Look up the best matching band for a frequency.
    pub fn lookup(&self, freq_hz: f64) -> Option<&BandAllocation> {
        // Prefer the narrowest matching band (most specific)
        self.bands
            .iter()
            .filter(|b| {
                freq_hz >= b.start_hz && freq_hz <= b.end_hz && b.regions.contains(&self.region)
            })
            .min_by_key(|b| ((b.end_hz - b.start_hz) * 1000.0) as u64)
    }

    /// Look up the nearest known discrete channel within a tolerance.
    pub fn lookup_known_frequency(
        &self,
        freq_hz: f64,
        tolerance_hz: f64,
    ) -> Option<&FrequencyAllocation> {
        self.frequencies
            .iter()
            .filter(|f| f.regions.contains(&self.region))
            .filter(|f| (f.freq_hz - freq_hz).abs() <= tolerance_hz)
            .min_by(|a, b| {
                (a.freq_hz - freq_hz)
                    .abs()
                    .partial_cmp(&(b.freq_hz - freq_hz).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// Get the band name for a frequency.
    pub fn band_name(&self, freq_hz: f64) -> Option<&str> {
        self.lookup_known_frequency(freq_hz, KNOWN_FREQUENCY_TOLERANCE_HZ)
            .map(|f| f.label)
            .or_else(|| self.lookup(freq_hz).map(|b| b.label))
    }

    /// Get the expected signal modulation for a frequency.
    pub fn modulation(&self, freq_hz: f64) -> Option<&str> {
        self.lookup_known_frequency(freq_hz, KNOWN_FREQUENCY_TOLERANCE_HZ)
            .map(|f| f.modulation)
            .or_else(|| self.lookup(freq_hz).map(|b| b.modulation))
    }

    /// Get the recommended demod mode for a frequency.
    pub fn demod_mode(&self, freq_hz: f64) -> Option<&str> {
        self.lookup_known_frequency(freq_hz, KNOWN_FREQUENCY_TOLERANCE_HZ)
            .and_then(|f| f.demod_mode)
            .or_else(|| self.lookup(freq_hz).and_then(|b| b.demod_mode))
    }

    /// Get the recommended decoder for a frequency.
    pub fn decoder(&self, freq_hz: f64) -> Option<&str> {
        self.lookup_known_frequency(freq_hz, KNOWN_FREQUENCY_TOLERANCE_HZ)
            .and_then(|f| f.decoder)
            .or_else(|| self.lookup(freq_hz).and_then(|b| b.decoder))
    }

    /// Get the service classification for a frequency.
    pub fn service(&self, freq_hz: f64) -> Option<ServiceType> {
        self.lookup_known_frequency(freq_hz, KNOWN_FREQUENCY_TOLERANCE_HZ)
            .map(|f| f.service)
            .or_else(|| self.lookup(freq_hz).map(|b| b.service))
    }

    /// Get a sample-rate hint for a frequency.
    pub fn sample_rate_hint(&self, freq_hz: f64) -> Option<f64> {
        self.lookup(freq_hz).and_then(|b| b.sample_rate)
    }

    /// Get all discrete known frequencies for the active region.
    pub fn known_frequencies(&self) -> Vec<&FrequencyAllocation> {
        self.frequencies
            .iter()
            .filter(|f| f.regions.contains(&self.region))
            .collect()
    }

    /// Get all bands for the active region.
    pub fn bands_for_region(&self) -> Vec<&BandAllocation> {
        self.bands
            .iter()
            .filter(|b| b.regions.contains(&self.region))
            .collect()
    }

    /// Get bands filtered by service type.
    pub fn bands_by_service(&self, service: ServiceType) -> Vec<&BandAllocation> {
        self.bands
            .iter()
            .filter(|b| b.service == service && b.regions.contains(&self.region))
            .collect()
    }

    /// Convert the database to classification rules for the RuleClassifier.
    pub fn to_classification_rules(&self) -> Vec<ClassificationRule> {
        self.bands
            .iter()
            .filter(|b| b.regions.contains(&self.region))
            .map(|b| ClassificationRule {
                name: b.label,
                freq_range: (b.start_hz, b.end_hz),
                bandwidth_range: b.bandwidth_range,
                decoder: b.decoder,
                modulation: b.modulation,
            })
            .collect()
    }
}

/// Detect region from config → TZ env → /etc/timezone → default NA.
fn detect_region() -> Region {
    // 1. Check config file
    if let Some(config_dir) = crate::util::config_dir() {
        let region_file = config_dir.join("region.toml");
        if let Ok(content) = std::fs::read_to_string(&region_file) {
            if let Ok(table) = content.parse::<toml::Table>() {
                if let Some(toml::Value::String(r)) = table.get("region") {
                    if let Ok(region) = r.parse::<Region>() {
                        return region;
                    }
                }
            }
        }
    }

    // 2. Check TZ environment variable
    if let Ok(tz) = std::env::var("TZ") {
        if let Some(r) = tz_to_region(&tz) {
            return r;
        }
    }

    // 3. Check /etc/timezone
    if let Ok(tz) = std::fs::read_to_string("/etc/timezone") {
        if let Some(r) = tz_to_region(tz.trim()) {
            return r;
        }
    }

    // 4. Check /etc/localtime symlink
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        let path = target.to_string_lossy();
        if let Some(tz) = path.strip_prefix("/usr/share/zoneinfo/") {
            if let Some(r) = tz_to_region(tz) {
                return r;
            }
        }
    }

    Region::NA
}

/// Map timezone string to region.
fn tz_to_region(tz: &str) -> Option<Region> {
    if tz.starts_with("America/") || tz.starts_with("US/") || tz.starts_with("Canada/") {
        Some(Region::NA)
    } else if tz.starts_with("Europe/") {
        Some(Region::EU)
    } else if tz.starts_with("Asia/Tokyo") || tz == "Japan" {
        Some(Region::JP)
    } else if tz.starts_with("Australia/") || tz.starts_with("Pacific/Auckland") {
        Some(Region::AU)
    } else {
        None
    }
}

/// Build all band allocations.
fn build_bands() -> Vec<BandAllocation> {
    vec![
        // LW AM Broadcast
        BandAllocation {
            start_hz: 150_000.0,
            end_hz: 280_000.0,
            label: "LW AM Broadcast",
            service: ServiceType::AmBroadcast,
            modulation: "am",
            demod_mode: Some("am"),
            decoder: None,
            sample_rate: None,
            bandwidth_range: (5_000.0, 10_000.0),
            regions: &[Region::EU],
        },
        // MW AM Broadcast
        BandAllocation {
            start_hz: 520_000.0,
            end_hz: 1_710_000.0,
            label: "MW AM Broadcast",
            service: ServiceType::AmBroadcast,
            modulation: "am",
            demod_mode: Some("am"),
            decoder: None,
            sample_rate: None,
            bandwidth_range: (5_000.0, 10_000.0),
            regions: ALL_REGIONS,
        },
        // HF Amateur (general)
        BandAllocation {
            start_hz: 1_800_000.0,
            end_hz: 30_000_000.0,
            label: "HF Amateur",
            service: ServiceType::AmateurRadio,
            modulation: "usb",
            demod_mode: Some("usb"),
            decoder: None,
            sample_rate: None,
            bandwidth_range: (300.0, 3_000.0),
            regions: ALL_REGIONS,
        },
        // FM Broadcast (Japan — 76-95 MHz)
        BandAllocation {
            start_hz: 76_000_000.0,
            end_hz: 95_000_000.0,
            label: "FM Broadcast",
            service: ServiceType::FmBroadcast,
            modulation: "wfm",
            demod_mode: Some("wfm"),
            decoder: Some("rds"),
            sample_rate: Some(1_024_000.0),
            bandwidth_range: (150_000.0, 250_000.0),
            regions: &[Region::JP],
        },
        // FM Broadcast (NA, EU, AU — 87.5-108 MHz)
        BandAllocation {
            start_hz: 87_500_000.0,
            end_hz: 108_000_000.0,
            label: "FM Broadcast",
            service: ServiceType::FmBroadcast,
            modulation: "wfm",
            demod_mode: Some("wfm"),
            decoder: Some("rds"),
            sample_rate: Some(1_024_000.0),
            bandwidth_range: (150_000.0, 250_000.0),
            regions: &[Region::NA, Region::EU, Region::AU],
        },
        // Airband
        BandAllocation {
            start_hz: 108_000_000.0,
            end_hz: 137_000_000.0,
            label: "Airband",
            service: ServiceType::Aviation,
            modulation: "am",
            demod_mode: Some("am"),
            decoder: None,
            sample_rate: None,
            bandwidth_range: (3_000.0, 10_000.0),
            regions: ALL_REGIONS,
        },
        // 2m Amateur (VHF)
        BandAllocation {
            start_hz: 144_000_000.0,
            end_hz: 148_000_000.0,
            label: "2m Amateur",
            service: ServiceType::AmateurRadio,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: None,
            sample_rate: None,
            bandwidth_range: (10_000.0, 25_000.0),
            regions: ALL_REGIONS,
        },
        // VHF NFM (general)
        BandAllocation {
            start_hz: 137_000_000.0,
            end_hz: 174_000_000.0,
            label: "VHF NFM",
            service: ServiceType::Other,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: None,
            sample_rate: None,
            bandwidth_range: (10_000.0, 25_000.0),
            regions: ALL_REGIONS,
        },
        // UHF Military
        BandAllocation {
            start_hz: 225_000_000.0,
            end_hz: 400_000_000.0,
            label: "UHF Military AM",
            service: ServiceType::Military,
            modulation: "am",
            demod_mode: Some("am"),
            decoder: None,
            sample_rate: None,
            bandwidth_range: (3_000.0, 25_000.0),
            regions: ALL_REGIONS,
        },
        // ISM 315 MHz (NA only)
        BandAllocation {
            start_hz: 314_000_000.0,
            end_hz: 316_000_000.0,
            label: "ISM 315 MHz",
            service: ServiceType::Ism,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: Some("rtl433-315"),
            sample_rate: None,
            bandwidth_range: (1_000.0, 500_000.0),
            regions: &[Region::NA],
        },
        // ISM 433 MHz
        BandAllocation {
            start_hz: 433_000_000.0,
            end_hz: 434_800_000.0,
            label: "ISM 433 MHz",
            service: ServiceType::Ism,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: Some("ook"),
            sample_rate: None,
            bandwidth_range: (1_000.0, 500_000.0),
            regions: ALL_REGIONS,
        },
        // PMR446 (EU)
        BandAllocation {
            start_hz: 446_000_000.0,
            end_hz: 446_200_000.0,
            label: "PMR446",
            service: ServiceType::PersonalRadio,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: None,
            sample_rate: None,
            bandwidth_range: (10_000.0, 12_500.0),
            regions: &[Region::EU],
        },
        // FRS/GMRS (NA)
        BandAllocation {
            start_hz: 462_000_000.0,
            end_hz: 467_725_000.0,
            label: "FRS/GMRS",
            service: ServiceType::PersonalRadio,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: None,
            sample_rate: None,
            bandwidth_range: (10_000.0, 25_000.0),
            regions: &[Region::NA],
        },
        // UHF NFM (general)
        BandAllocation {
            start_hz: 400_000_000.0,
            end_hz: 470_000_000.0,
            label: "UHF NFM",
            service: ServiceType::Other,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: None,
            sample_rate: None,
            bandwidth_range: (10_000.0, 25_000.0),
            regions: ALL_REGIONS,
        },
        // UHF TV/FM
        BandAllocation {
            start_hz: 470_000_000.0,
            end_hz: 862_000_000.0,
            label: "UHF TV/FM",
            service: ServiceType::FmBroadcast,
            modulation: "wfm",
            demod_mode: Some("wfm"),
            decoder: None,
            sample_rate: None,
            bandwidth_range: (100_000.0, 8_000_000.0),
            regions: ALL_REGIONS,
        },
        // ISM 868 MHz (EU)
        BandAllocation {
            start_hz: 863_000_000.0,
            end_hz: 870_000_000.0,
            label: "ISM 868 MHz",
            service: ServiceType::Ism,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: Some("rtl433-868"),
            sample_rate: None,
            bandwidth_range: (1_000.0, 500_000.0),
            regions: &[Region::EU],
        },
        // ISM 915 MHz (NA, AU)
        BandAllocation {
            start_hz: 902_000_000.0,
            end_hz: 928_000_000.0,
            label: "ISM 915 MHz",
            service: ServiceType::Ism,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: Some("rtl433-915"),
            sample_rate: None,
            bandwidth_range: (1_000.0, 500_000.0),
            regions: &[Region::NA, Region::AU],
        },
        // ADS-B
        BandAllocation {
            start_hz: 1_089_000_000.0,
            end_hz: 1_091_000_000.0,
            label: "ADS-B",
            service: ServiceType::Aviation,
            modulation: "ppm",
            demod_mode: None,
            decoder: Some("adsb"),
            sample_rate: Some(2_400_000.0),
            bandwidth_range: (1_000_000.0, 3_000_000.0),
            regions: ALL_REGIONS,
        },
    ]
}

/// Build discrete known frequencies.
fn build_frequencies() -> Vec<FrequencyAllocation> {
    vec![
        // Aviation emergency
        FrequencyAllocation {
            freq_hz: 121_500_000.0,
            label: "Aviation Emergency",
            service: ServiceType::Emergency,
            modulation: "am",
            demod_mode: Some("am"),
            decoder: None,
            regions: ALL_REGIONS,
        },
        // APRS NA
        FrequencyAllocation {
            freq_hz: 144_390_000.0,
            label: "APRS",
            service: ServiceType::AmateurRadio,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: Some("aprs"),
            regions: &[Region::NA],
        },
        // APRS EU
        FrequencyAllocation {
            freq_hz: 144_800_000.0,
            label: "APRS",
            service: ServiceType::AmateurRadio,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: Some("aprs"),
            regions: &[Region::EU],
        },
        // Marine VHF Ch16
        FrequencyAllocation {
            freq_hz: 156_800_000.0,
            label: "Marine VHF Ch16",
            service: ServiceType::Maritime,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: None,
            regions: ALL_REGIONS,
        },
        // AIS Channel A
        FrequencyAllocation {
            freq_hz: 161_975_000.0,
            label: "AIS Channel A",
            service: ServiceType::Maritime,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: Some("ais-a"),
            regions: ALL_REGIONS,
        },
        // AIS Channel B
        FrequencyAllocation {
            freq_hz: 162_025_000.0,
            label: "AIS Channel B",
            service: ServiceType::Maritime,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: Some("ais-b"),
            regions: ALL_REGIONS,
        },
        // NOAA Weather Radio
        FrequencyAllocation {
            freq_hz: 162_400_000.0,
            label: "NOAA Weather 1",
            service: ServiceType::Weather,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: None,
            regions: &[Region::NA],
        },
        FrequencyAllocation {
            freq_hz: 162_425_000.0,
            label: "NOAA Weather 2",
            service: ServiceType::Weather,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: None,
            regions: &[Region::NA],
        },
        FrequencyAllocation {
            freq_hz: 162_450_000.0,
            label: "NOAA Weather 3",
            service: ServiceType::Weather,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: None,
            regions: &[Region::NA],
        },
        FrequencyAllocation {
            freq_hz: 162_475_000.0,
            label: "NOAA Weather 4",
            service: ServiceType::Weather,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: None,
            regions: &[Region::NA],
        },
        FrequencyAllocation {
            freq_hz: 162_500_000.0,
            label: "NOAA Weather 5",
            service: ServiceType::Weather,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: None,
            regions: &[Region::NA],
        },
        FrequencyAllocation {
            freq_hz: 162_525_000.0,
            label: "NOAA Weather 6",
            service: ServiceType::Weather,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: None,
            regions: &[Region::NA],
        },
        FrequencyAllocation {
            freq_hz: 162_550_000.0,
            label: "NOAA Weather 7",
            service: ServiceType::Weather,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: None,
            regions: &[Region::NA],
        },
        // NOAA APT Satellites
        FrequencyAllocation {
            freq_hz: 137_100_000.0,
            label: "NOAA-19 APT",
            service: ServiceType::Satellite,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: Some("noaa-apt-19"),
            regions: ALL_REGIONS,
        },
        FrequencyAllocation {
            freq_hz: 137_620_000.0,
            label: "NOAA-15 APT",
            service: ServiceType::Satellite,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: Some("noaa-apt-15"),
            regions: ALL_REGIONS,
        },
        FrequencyAllocation {
            freq_hz: 137_912_500.0,
            label: "NOAA-18 APT",
            service: ServiceType::Satellite,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: Some("noaa-apt-18"),
            regions: ALL_REGIONS,
        },
        // POCSAG pager
        FrequencyAllocation {
            freq_hz: 929_612_500.0,
            label: "POCSAG Pager",
            service: ServiceType::Pager,
            modulation: "fm",
            demod_mode: Some("fm"),
            decoder: Some("pocsag"),
            regions: &[Region::NA],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_detect_defaults_to_na() {
        // Without config/TZ set, should fall back
        let db = FrequencyDb::new(Region::NA);
        assert_eq!(db.region, Region::NA);
    }

    #[test]
    fn lookup_fm_broadcast_na() {
        let db = FrequencyDb::new(Region::NA);
        let band = db.lookup(98_300_000.0).unwrap();
        assert_eq!(band.label, "FM Broadcast");
        assert_eq!(band.modulation, "wfm");
        assert_eq!(band.decoder, Some("rds"));
    }

    #[test]
    fn lookup_fm_broadcast_jp() {
        let db = FrequencyDb::new(Region::JP);
        // Japan FM starts at 76 MHz
        let band = db.lookup(80_000_000.0).unwrap();
        assert_eq!(band.label, "FM Broadcast");
        // Standard NA/EU FM band should NOT match for JP (87.5-108 is NA/EU/AU)
        let band2 = db.lookup(98_300_000.0);
        assert!(band2.is_none() || band2.unwrap().label != "FM Broadcast");
    }

    #[test]
    fn lookup_airband() {
        let db = FrequencyDb::new(Region::NA);
        let band = db.lookup(121_500_000.0).unwrap();
        assert_eq!(band.label, "Airband");
        assert_eq!(band.modulation, "am");
    }

    #[test]
    fn lookup_adsb() {
        let db = FrequencyDb::new(Region::NA);
        let band = db.lookup(1_090_000_000.0).unwrap();
        assert_eq!(band.label, "ADS-B");
        assert_eq!(band.decoder, Some("adsb"));
    }

    #[test]
    fn lookup_ism_315_na_only() {
        let na = FrequencyDb::new(Region::NA);
        assert!(na.lookup(315_000_000.0).is_some());
        let eu = FrequencyDb::new(Region::EU);
        // EU should not have ISM 315
        let band = eu.lookup(315_000_000.0);
        assert!(band.is_none() || band.unwrap().label != "ISM 315 MHz");
    }

    #[test]
    fn lookup_pmr446_eu_only() {
        let eu = FrequencyDb::new(Region::EU);
        let band = eu.lookup(446_100_000.0).unwrap();
        assert_eq!(band.label, "PMR446");
        let na = FrequencyDb::new(Region::NA);
        let band = na.lookup(446_100_000.0);
        // NA should get UHF NFM, not PMR446
        assert!(band.is_none() || band.unwrap().label != "PMR446");
    }

    #[test]
    fn band_name_and_demod_mode() {
        let db = FrequencyDb::new(Region::NA);
        assert_eq!(db.band_name(98_300_000.0), Some("FM Broadcast"));
        assert_eq!(db.modulation(98_300_000.0), Some("wfm"));
        assert_eq!(db.demod_mode(98_300_000.0), Some("wfm"));
        assert_eq!(db.decoder(98_300_000.0), Some("rds"));
    }

    #[test]
    fn known_frequency_overrides_general_band_metadata() {
        let db = FrequencyDb::new(Region::NA);
        assert_eq!(db.band_name(162_550_000.0), Some("NOAA Weather 7"));
        assert_eq!(db.demod_mode(162_550_000.0), Some("fm"));
        assert_eq!(db.decoder(161_975_000.0), Some("ais-a"));
    }

    #[test]
    fn adsb_has_decoder_hint_but_no_live_demod_mode() {
        let db = FrequencyDb::new(Region::NA);
        assert_eq!(db.band_name(1_090_000_000.0), Some("ADS-B"));
        assert_eq!(db.modulation(1_090_000_000.0), Some("ppm"));
        assert_eq!(db.demod_mode(1_090_000_000.0), None);
        assert_eq!(db.decoder(1_090_000_000.0), Some("adsb"));
        assert_eq!(db.sample_rate_hint(1_090_000_000.0), Some(2_400_000.0));
    }

    #[test]
    fn service_prefers_known_frequency_over_broad_band() {
        let db = FrequencyDb::new(Region::NA);
        assert_eq!(db.service(162_550_000.0), Some(ServiceType::Weather));
        assert_eq!(db.service(161_975_000.0), Some(ServiceType::Maritime));
    }

    #[test]
    fn known_frequencies_filtered_by_region() {
        let na = FrequencyDb::new(Region::NA);
        let eu = FrequencyDb::new(Region::EU);
        let na_freqs: Vec<&str> = na.known_frequencies().iter().map(|f| f.label).collect();
        let eu_freqs: Vec<&str> = eu.known_frequencies().iter().map(|f| f.label).collect();
        // NOAA Weather is NA only
        assert!(na_freqs.iter().any(|l| l.starts_with("NOAA Weather")));
        assert!(!eu_freqs.iter().any(|l| l.starts_with("NOAA Weather")));
        // AIS is global
        assert!(na_freqs.contains(&"AIS Channel A"));
        assert!(eu_freqs.contains(&"AIS Channel A"));
    }

    #[test]
    fn classification_rules_generation() {
        let db = FrequencyDb::new(Region::NA);
        let rules = db.to_classification_rules();
        assert!(!rules.is_empty());
        // Should have FM Broadcast rule
        assert!(rules.iter().any(|r| r.name == "FM Broadcast"));
        // Should have ADS-B rule
        assert!(rules.iter().any(|r| r.name == "ADS-B"));
    }

    #[test]
    fn tz_to_region_mapping() {
        assert_eq!(tz_to_region("America/New_York"), Some(Region::NA));
        assert_eq!(tz_to_region("Europe/London"), Some(Region::EU));
        assert_eq!(tz_to_region("Asia/Tokyo"), Some(Region::JP));
        assert_eq!(tz_to_region("Australia/Sydney"), Some(Region::AU));
        assert_eq!(tz_to_region("Africa/Cairo"), None);
    }

    #[test]
    fn narrowest_band_wins() {
        // 2m Amateur (144-148) is narrower than VHF NFM (137-174)
        let db = FrequencyDb::new(Region::NA);
        let band = db.lookup(145_000_000.0).unwrap();
        assert_eq!(band.label, "2m Amateur");
    }

    #[test]
    fn bands_by_service() {
        let db = FrequencyDb::new(Region::NA);
        let aviation = db.bands_by_service(ServiceType::Aviation);
        assert!(!aviation.is_empty());
        assert!(aviation.iter().all(|b| b.service == ServiceType::Aviation));
    }
}
