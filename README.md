# WaveRunner SDR

**An SDR workbench for seeing, hearing, recording, and decoding radio signals from one place.**

WaveRunner is built around a simple idea: the normal SDR workflow should not require a pile of disconnected utilities. You should be able to plug in an RTL-SDR, see the spectrum, listen, record IQ, replay captures, run protocol decoders, and inspect results without stitching together a different tool for every step.

The same engine powers a CLI, a TUI, and a desktop GUI. Underneath that, WaveRunner combines native Rust DSP with explicit bridges to proven OSS decoders where that is the better engineering choice.

> **Beta snapshot (April 16, 2026)** — Functional and heavily tested, but still has explicit gaps. Hardware support is RTL-SDR only. See **Known Shortcomings** before treating every decoder path as production-proven.

## If You're New to SDR

WaveRunner is meant to shorten the "what am I even looking at?" phase.

- Use it to scan bands, look at a waterfall, listen to broadcast or narrowband signals, record IQ, and replay captures later.
- It gives you one place to move from "I found a signal" to "what is this?" to "can I decode or analyze it?"
- You do not need to know every external decoder ahead of time; `waverunner tools` and `waverunner decode list` tell you what is installed and what the app can actually use right now.

If your goal is: "I have an RTL-SDR and I want a practical starting point that is not a tutorial maze," this is the audience fit.

## If You Already Know SDR

WaveRunner is a shared SDR engine with multiple frontends, replay-first workflows, and explicit backend boundaries.

- Native Rust handles the core DSP path: ingest, DDC, FFT, CFAR, demodulation, analysis, export, and session health.
- External tools are integrated deliberately instead of reimplemented badly: `rtl_433`, `redsea`, `multimon-ng`, and dump1090-compatible ADS-B backends.
- The project is useful if you want one codebase for CLI automation, TUI monitoring, GUI control, SigMF capture/replay, and protocol experimentation without losing sight of which parts are native vs delegated.

If your goal is: "give me a coherent SDR platform I can inspect, extend, and run from the terminal or a UI," this is the audience fit.

## What Can It Actually Do?

- **See everything** — real-time spectrum display, waterfall plots, signal detection (CFAR)
- **Decode a broad set of protocol targets** — POCSAG pagers, ADS-B aircraft, RDS radio text, FLEX, EAS/SAME alert headers, APRS ham radio, AIS maritime, OOK devices (weather stations, TPMS, remotes), NOAA satellite images, plus an `rtl_433` bridge for 250+ additional device types
- **Analyze signals** — power/bandwidth measurement, burst detection, modulation estimation, bitstream inspection, spectral comparison, signal tracking over time
- **Record and export** — raw IQ capture with SigMF metadata, export to CSV/JSON/TSV/PNG
- **Scan intelligently** — auto-scanner with audio output, operating profiles (aviation, pager, FM broadcast), frequency bookmarks, ITU band database with region auto-detection
- **Stay healthy** — pipeline health monitoring, session checkpoints, latency tracking, load shedding under pressure

## Why Rust?

Because DSP at 2.048 MS/s doesn't forgive sloppy memory management, and because you should be able to read and verify the code that's listening to your local RF environment. WaveRunner is Rust-first, but not ideological: native DSP stays in-tree, and protocol backends bridge to proven OSS tools like `rtl_433`, `redsea`, `multimon-ng`, and dump1090-compatible ADS-B decoders when that is the more reliable option.

## Prerequisites

- Rust 1.85+ (edition 2024)
- RTL-SDR hardware + drivers (`librtlsdr-dev` on Debian/Ubuntu, `rtl-sdr` on Arch)
- Optional: `rtl_433` on PATH for ISM/OOK sensor decoding
- Optional: `redsea` on PATH for FM RDS/RBDS decoding. On Arch this is typically AUR (`paru -S redsea`); on some distros it may require a source build.
- Optional: `multimon-ng` on PATH for POCSAG/APRS/DTMF/EAS/FLEX decoding
- Optional: `dump1090`, `dump1090-fa`, or `readsb` on PATH for ADS-B decoding. On Arch this is typically AUR (`paru -S dump1090-fa-git` or `readsb-git`). `dump1090_rs` is not currently supported by the stdin bridge.
- Optional: audio output libraries (ALSA/PulseAudio/PipeWire dev packages)
- Node.js 20+ and npm for GUI builds

## Installation

```bash
git clone https://github.com/SeladorGroup/waverunner-sdr.git
cd waverunner-sdr
cargo build --release
```

The binary lands at `target/release/waverunner`.

To build the desktop GUI bundle from a clean clone:

```bash
cd crates/waverunner-gui/frontend
npm ci
npm run build
cd ../../..
cargo build -p waverunner-gui
```

No hardware? No problem — build without RTL-SDR/audio dependencies and use replay mode with recorded IQ files:

```bash
cargo build --release --no-default-features
```

## Quick Start

```bash
# Check which optional decoder backends are available
waverunner tools

# Watch the spectrum in your terminal
waverunner tui --freq 162.55M

# Decode pager traffic
waverunner listen --freq 152.48M --decoder pocsag

# Track aircraft overhead
waverunner listen --freq 1090M --decoder adsb

# Auto-scan with audio — police scanner style
waverunner mode general --listen

# Record raw IQ for later analysis
waverunner record --freq 433.92M --duration 30

# Analyze a recording
waverunner analyze capture.cf32 measure
waverunner analyze capture.cf32 modulation

# Replay a recording through the TUI
waverunner tui --replay capture.cf32

# What's on this frequency?
waverunner identify 433.92M

# List known frequency allocations for your region
waverunner bands
```

If you are brand new, the simplest path is:

1. Run `waverunner tools` to see what optional decoder backends are actually available.
2. Run `waverunner tui --freq 162.55M` or another known local signal to get comfortable with the spectrum and waterfall.
3. Run `waverunner scan` or `waverunner mode general --listen` to find active channels.
4. Record a short capture with `waverunner record` and replay it later instead of trying to learn everything live.
5. Use `waverunner identify` or `waverunner decode list` when you want help choosing the next step.

## Architecture

Six crates in a Cargo workspace:

| Crate | What it does |
|-------|-------------|
| `wavecore` | DSP engine, session manager, decoders, analysis, hardware abstraction — the brains |
| `waveplugins` | Plugin interface (placeholder for custom decoders) |
| `waveviz` | GPU-accelerated spectrum rendering (wgpu) |
| `waverunner-cli` | Command-line interface |
| `waverunner-tui` | Terminal UI (ratatui) |
| `waverunner-gui` | Desktop app (Tauri 2 + Svelte 5) |

Data flows through a `SessionManager` that owns the DSP pipeline and communicates with frontends via command/event channels. The pipeline runs in a dedicated thread:

```
IQ samples → DC removal → FFT → CFAR detection → demodulation → decoders
                                                                    ↓
                                              spectrum frames, decoded messages → frontend
```

Some protocol decoders are native Rust implementations (`ais`, `ook`, `noaa-apt-*`). Others are explicit bridges to external OSS backends that you can inspect and swap at the system level (`rtl_433`, `redsea`, `multimon-ng`, and dump1090-compatible ADS-B tools). Run `waverunner decode list` or `waverunner tools` to see the current backend and availability state.

## Known Shortcomings

As of the beta hardening pass on **April 16, 2026**, the known gaps are:

- RTL-SDR is the only hardware backend wired and tested end to end.
- `dump1090_rs` is still unsupported by the ADS-B stdin bridge. Arch users need a compatible `dump1090` / `dump1090-fa` / `readsb` backend instead.
- `rds` and `adsb` are now positively validated on live RF on the `athena` test host, but only on the exact toolchain used there (`redsea` plus `dump1090-fa`). They should still be treated as backend-dependent features, not generic guarantees across every distro package variant.
- The least-proven decoder ranges in this beta are the native-only paths around **137 MHz** (`noaa-apt-*`) and **161.975 / 162.025 MHz** (`ais-*`). They do not have an alternate external backend wired into WaveRunner today, and they did not receive positive live-payload validation in this hardening pass.
- `ACARS` is not implemented yet and is intentionally not advertised as supported.
- Weather alert coverage is currently the generic `eas` / `multimon-ng` path, not a dedicated NOAA Weather Radio workflow.

## Roadmap

- **Local LLM integration** — on-device signal classification and anomaly detection using local models, no cloud required. Your RF guardian that learns what's normal and alerts you when something isn't.
- **Plugin system** — drop-in custom decoders without forking
- **Multi-device support** — multiple SDR dongles in parallel
- **Gamification** — achievements and challenges to flatten the SDR learning curve. Make the invisible world of radio fun to explore.
- **Session replay** — annotation playback and pattern analysis over time

## License

This project is licensed under the [GNU General Public License v3.0](LICENSE).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build instructions and guidelines.
