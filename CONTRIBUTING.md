# Contributing to WaveRunner

## Building

```bash
git clone https://github.com/SeladorGroup/waverunner-sdr.git
cd waverunner-sdr
cargo build --workspace --all-features
```

System dependencies:
- `librtlsdr-dev` (Debian/Ubuntu) or `rtl-sdr` (Arch) for RTL-SDR support
- ALSA/PulseAudio/PipeWire dev libraries for audio output
- Optional decoder backends on `PATH`: `rtl_433`, `redsea`, `multimon-ng`, and one of `dump1090` / `dump1090-fa` / `readsb`
- On Arch, `redsea` and compatible ADS-B backends are typically AUR packages (`paru -S redsea dump1090-fa-git`)
- `dump1090_rs` is not currently compatible with the ADS-B stdin bridge
- Node.js 20+ and npm for the GUI frontend (`cd crates/waverunner-gui/frontend && npm ci`)

Check optional backend availability with:

```bash
cargo run -p waverunner-cli -- tools
```

## Code Quality

All PRs must pass:

```bash
cargo fmt --all --check          # formatting
cargo clippy --workspace --all-targets --all-features -- -D warnings  # lints
cargo test --workspace --all-features   # tests
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps  # doc warnings
cd crates/waverunner-gui/frontend && npm ci && npm audit --audit-level=high && npm run build  # GUI supply chain + bundle
cd /path/to/waverunner-sdr && cargo package --allow-dirty --workspace --no-verify  # manifest/package hygiene
```

Run `cargo fmt --all` before committing.

## Release Validation

Before tagging a GitHub release:

```bash
cargo build --workspace --all-features
cargo test --workspace --all-features
cargo test -p wavecore --test soak_test -- --ignored --nocapture
```

Do at least one live RTL-SDR smoke or soak pass on real hardware in addition to replay tests. At minimum, validate `scan`, `record`, `listen`, and one external decoder backend that is installed on the target host.

If a release candidate still has decoder paths that do not produce positive payload on real RF, document that explicitly in `README.md` and `docs/SUPPORT_MATRIX.md` before shipping.

## Pull Requests

1. Fork the repo and create a feature branch from `main`
2. Keep commits focused — one logical change per commit
3. Add tests for new functionality
4. Update doc comments for any changed public API
5. Open a PR with a clear description of what and why

## Reporting Issues

Open an issue on GitHub. Include:
- What you expected vs what happened
- Hardware setup (SDR device, OS, driver version)
- Steps to reproduce
- Relevant log output (`RUST_LOG=debug waverunner ...`)

## License

By contributing, you agree that your contributions will be licensed under the [GPL-3.0](LICENSE).
