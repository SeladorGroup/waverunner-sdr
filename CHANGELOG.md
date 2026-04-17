# Changelog

This file tracks work that exists after the last commit and before the next one.

Add new notes to the top of the `Unreleased` section.

## Unreleased

### 2026-04-17

- Added real capture-library management in the CLI: `library import`, `library edit`, and `library remove`, including raw-capture import with explicit sample-rate/frequency overrides, catalog metadata edits, and optional file deletion on removal.
- Added catalog-side metadata sync and artifact deletion helpers in `wavecore`, so library edits can update JSON sidecars and removals can clean up associated files instead of only dropping catalog rows.
- Made `waverunner record` generate a default output path automatically when `-o/--output` is omitted, using the capture library path generator and the recording label when present.
- Added a GUI capture-management slice by wiring recent-capture removal into the library panel and surfacing capture tags there.
- Added low-level tests for catalog import/remove/sync plus binary-level tests for library import/edit/remove.
- Added replay-mode startup to `waverunner-tui`, including `--replay` and `--latest`, metadata-aware sample-rate/frequency resolution, and clean fallback to explicit overrides when metadata is missing.
- Added binary-level CLI regression tests covering `replay --latest --fast` and `analyze --latest measure` so the newest-capture workflow and the short-capture analysis fix stay exercised through the real executable path.
- Added repo-local `AGENTS.md` with stable working rules for this project, including changelog discipline, honest validation standards, screenshot expectations for substantial UI work, and the recurring user preference to keep docs candid and human.
- Added this root `CHANGELOG.md` to track everything that exists after `643378f` and before the next commit, with newest notes kept at the top of `Unreleased`.
- Added metadata-aware capture inspection and reopen support in `wavecore` for raw capture sidecars, `.sigmf-meta`, `.sigmf-data`, and SigMF stem paths.
- Added a first-class `waverunner replay` CLI workflow with demod/decoder support, metadata inference, `--fast`, `--loop`, block-size control, and clean EOF shutdown.
- Added `waverunner library inspect` and `waverunner library latest`, plus `waverunner replay --latest` and `waverunner analyze --latest` so the newest indexed capture can be reopened directly.
- Added the CLI wiring and helper refactors needed to support the new replay flow, including the new command registration plus shared demod-mode and decoded-message formatting hooks.
- Fixed replay session shutdown in the session manager so non-looping file replay exits cleanly instead of idling after the replay thread ends.
- Fixed offline analysis on short captures by replaying without real-time pacing and looping during analysis, which keeps the session alive long enough for the analysis command to execute.
- Updated the Tauri backend and GUI replay flow so the desktop app can inspect replay metadata, auto-fill sample rate and center frequency, replay indexed captures without fake manual defaults, and replay from the recent-capture library using the backend-resolved metadata path.
- Updated README examples for `library inspect`, replay/analyze with metadata or SigMF, and the newest-capture workflow.
- Validation for this uncommitted pass:
  - `cargo fmt --all`
  - `cargo test --workspace --all-features`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `npm run check`
  - `npm run build`
  - isolated smoke of `waverunner library latest`, `waverunner replay --latest --fast`, and `waverunner analyze --latest measure`
  - TUI replay smoke under a real pseudo-terminal: `timeout 3 cargo run -q -p waverunner-tui -- --replay <metadata-sidecar>` -> timed out cleanly with `124`
