# WaveRunner Support Matrix

## Operating Systems

| OS | Status | Notes |
|---|---|---|
| Linux (x86_64) | Primary | Tested on Arch Linux; requires librtlsdr |
| Linux (aarch64) | Expected compatible | Untested; should work with ARM librtlsdr |
| macOS (Apple Silicon) | Untested | Requires Homebrew librtlsdr; cpal uses CoreAudio |
| macOS (Intel) | Untested | Same as above |
| Windows | Untested | Requires Zadig USB driver; cpal uses WASAPI |

## SDR Hardware

| Device | Status | Notes |
|---|---|---|
| RTL-SDR Blog v3 | Tested | Primary development hardware |
| RTL-SDR Blog v4 | Tested | Live-soak tested on Linux with a real dongle |
| Generic RTL2832U | Expected compatible | Any RTL-SDR compatible dongle |
| Other SDR devices | Not supported | SdrDevice trait allows future backends |

## Driver Requirements

| Driver | Version | Required | Notes |
|---|---|---|---|
| librtlsdr | ≥ 0.6 | Yes (for hardware) | `apt install librtlsdr-dev` or equivalent |
| rtl_433 | ≥ 23.11 | Optional | Required for OOK/ISM band decoders (rtl433-*) |
| redsea | recent | Optional | Required for `rds`; Arch users will usually need AUR (`paru -S redsea`) and some distros may require a source build |
| multimon-ng | recent | Optional | Required for `pocsag*`, `aprs`, `dtmf`, `eas`, and `flex` decoders |
| dump1090 / dump1090-fa / readsb | recent | Optional | Required for `adsb`; stdin-compatible backends only, `dump1090_rs` is not supported, and Arch users will usually need AUR |

## Audio Backend

| Platform | Backend | Notes |
|---|---|---|
| Linux | ALSA / PulseAudio / PipeWire | Via cpal; requires `audio` feature |
| macOS | CoreAudio | Via cpal |
| Windows | WASAPI | Via cpal |

## Rust Toolchain

| Requirement | Value |
|---|---|
| Edition | 2024 |
| Minimum Rust version | 1.85 |
| Recommended | Latest stable |

## Feature Flags

| Feature | Default | Description |
|---|---|---|
| `rtlsdr` | Yes | RTL-SDR hardware support via rtlsdr_mt |
| `audio` | Yes | Audio output via cpal |

## Replay Mode

File-based replay (`ReplayDevice`) works on all platforms without hardware:
- Supported formats: `.cf32` (complex float32 LE), `.cu8` (complex uint8)
- No driver dependencies required for replay

## Decoder Validation Snapshot (2026-04-16)

| Path | Status | Notes |
|---|---|---|
| `tune` / `scan` / `record` / `listen` | Live-soaked | Verified on a real RTL-SDR Blog v4 on `athena`; `listen` overflow regression was fixed during hardening |
| `rtl433` | Live payload observed | Decoded TPMS traffic on `athena` during soak |
| `rds` | Backend stable, no live payload observed | `redsea` is integrated and now installed on `athena`, but neither WaveRunner nor direct `rtl_fm \| redsea` control runs produced RDS payload with the current antenna placement |
| `adsb` | Backend stable, no live payload observed | `dump1090` bridge now starts cleanly with `UC8` input; neither WaveRunner nor direct `dump1090` control runs produced aircraft payload on the current test host / antenna placement |
| `pocsag*` / `aprs` / `dtmf` / `eas` / `flex` | Runtime verified | `multimon-ng` bridge starts cleanly; no positive live payload was captured in the current soak window |
| `ais-*` | Runtime verified, native-only | Decoder path stayed up, but no positive live payload was observed in this soak window |
| `noaa-apt-*` | Not live-soaked in this pass | Needs a real satellite pass for positive validation; still a native-only path in this repo |

## Native-Only Gaps

The two least-backed decoder ranges in the current beta are:

- **137 MHz NOAA APT** via `noaa-apt-*`
- **161.975 / 162.025 MHz AIS** via `ais-*`

Those bands are still native-only inside WaveRunner. This hardening pass did not replace them with an alternate external backend, and they remain the areas that need the strongest future validation work.
