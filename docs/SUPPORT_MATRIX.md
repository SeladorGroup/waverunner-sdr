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
| RTL-SDR Blog v4 | Expected compatible | Uses same RTL2832U chipset |
| Generic RTL2832U | Expected compatible | Any RTL-SDR compatible dongle |
| Other SDR devices | Not supported | SdrDevice trait allows future backends |

## Driver Requirements

| Driver | Version | Required | Notes |
|---|---|---|---|
| librtlsdr | ≥ 0.6 | Yes (for hardware) | `apt install librtlsdr-dev` or equivalent |
| rtl_433 | ≥ 23.11 | Optional | Required for OOK/ISM band decoders (rtl433-*) |

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
