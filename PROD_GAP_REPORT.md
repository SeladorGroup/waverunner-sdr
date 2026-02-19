# Production Readiness Gap Report

**Date:** 2026-02-15
**Phase:** 11 — Production Readiness
**Baseline:** 472 tests, 0 clippy warnings, Phases 1-10 complete

---

## Critical Severity

### C1: Production-path panics in decoder spawn
- **File:** `crates/wavecore/src/dsp/decoder.rs:166`
- **Issue:** `.expect("failed to spawn decoder thread")` panics if thread creation fails
- **Impact:** Unrecoverable crash during runtime decoder enable
- **Fix:** Return `Result<DecoderHandle, WaveError>`, handle in manager.rs

### C2: Production-path panics in rtl_433 subprocess
- **File:** `crates/wavecore/src/dsp/decoders/rtl433.rs:104-113`
- **Issue:** Three `.expect()` calls on `stdin.take()`, `stdout.take()`, thread spawn
- **Impact:** Unrecoverable crash if subprocess setup partially fails
- **Fix:** Replace with `ok_or()` error propagation

### C3: NaN-triggered panics in DSP estimation
- **File:** `crates/wavecore/src/dsp/estimation.rs:383,451-452,496-498`
- **Issue:** `.partial_cmp(b).unwrap()` panics on NaN comparison
- **Impact:** Corrupted input or numerical instability crashes processing thread
- **Fix:** `.unwrap_or(std::cmp::Ordering::Equal)` on all `partial_cmp` calls

### C4: NaN-triggered panics in IIR filter design
- **File:** `crates/wavecore/src/dsp/iir.rs:455,468,483,495`
- **Issue:** Same `.partial_cmp().unwrap()` pattern
- **Impact:** NaN propagation from upstream stages crashes filter design
- **Fix:** Same NaN-safe comparison pattern

### C5: No CI pipeline
- **File:** (missing) `.github/workflows/`
- **Issue:** No automated testing, no regression detection, no release gates
- **Impact:** Breaking changes can merge silently; soak tests never run automatically
- **Fix:** Add GitHub Actions with clippy, test, soak, security gates

---

## High Severity

### H1: Recording finalization error silenced
- **File:** `crates/wavecore/src/session/manager.rs:690`
- **Issue:** `rec.writer.finish().unwrap_or(rec.samples_written)` hides write failure
- **Impact:** User told recording succeeded when finalization (flush/close) failed; data loss
- **Fix:** Match on Result, emit Error event on failure

### H2: Recording error-path finalization silenced
- **File:** `crates/wavecore/src/session/manager.rs:969`
- **Issue:** `rec.writer.finish().ok()` after write error silences further failures
- **Impact:** Partial recording not properly closed; no indication to user
- **Fix:** Log error via tracing on finalize failure

### H3: No schema versioning on persisted formats
- **Files:** `session/mod.rs:235` (SessionConfig), `recording.rs:12` (RecordingMetadata), `mode/profile.rs:18` (MissionProfile)
- **Issue:** No version field; any struct change breaks deserialization of old files
- **Impact:** Silent data corruption or crash on loading old sessions/configs after upgrade
- **Fix:** Add `schema_version: u32` with `#[serde(default)]` for backward compat

### H4: No failure injection tests
- **Files:** (missing) `tests/resilience_test.rs`
- **Issue:** No tests for disk full, corrupted input, malformed config, channel disconnection
- **Impact:** Unknown behavior under adverse conditions; potential silent data loss
- **Fix:** Add 12 targeted resilience tests

### H5: No crash recovery mechanism
- **Files:** (missing) `session/checkpoint.rs`
- **Issue:** All session state lost on crash; no autosave, no restore
- **Impact:** Long recording sessions completely lost on unexpected termination
- **Fix:** Periodic checkpoint to `~/.cache/waverunner/checkpoint.json`

### H6: No supply-chain security baseline
- **Files:** (missing) `deny.toml`
- **Issue:** No cargo-deny, no vulnerability scanning, no license audit, no SBOM
- **Impact:** Known CVEs in dependencies go undetected
- **Fix:** Add cargo-deny config + CI integration

---

## Medium Severity

### M1: No written SLO targets
- **Files:** (missing) `slo.toml`, `slo.rs`
- **Issue:** Health thresholds exist but no machine-readable targets for drop rate, latency, throughput
- **Impact:** No automated regression detection for performance characteristics
- **Fix:** Create SLO struct with default thresholds, TOML config, test checker

### M2: No shutdown timeout on thread joins
- **File:** `crates/wavecore/src/session/manager.rs:449-454`
- **Issue:** `h.join().ok()` blocks indefinitely if decoder thread deadlocks
- **Impact:** Application hangs on shutdown; Ctrl+C ineffective
- **Fix:** Document as known limitation (thread join timeout requires platform-specific code)

### M3: No structured logging contract
- **Files:** `waverunner-cli/src/main.rs:37-41`, `wavecore/src/session/manager.rs` (various)
- **Issue:** Tracing configured but no session_id, component, event_code fields
- **Impact:** Log aggregation and correlation across threads/sessions not possible
- **Fix:** Add logging.rs with structured field helpers, --json-log CLI flag

### M4: No hardware compatibility documentation
- **Files:** (missing) `docs/SUPPORT_MATRIX.md`
- **Issue:** SdrDevice trait abstraction exists but no documented OS/hardware/driver matrix
- **Impact:** Users cannot determine compatibility before installation
- **Fix:** Create support matrix with tested vs. expected-compatible entries

### M5: No environment probe command
- **Files:** (missing) `commands/probe.rs`
- **Issue:** No way to capture environment details for bug reports
- **Impact:** Debugging hardware/driver issues requires manual investigation
- **Fix:** Add `waverunner probe` CLI command

### M6: Replay device accepts empty/corrupt files without validation
- **File:** `crates/wavecore/src/session/replay.rs`
- **Issue:** No file size validation in `open()`; empty file may cause unclear errors
- **Impact:** Confusing error messages when replaying corrupt recordings
- **Fix:** Validate minimum file size, return descriptive HardwareError

---

## Summary

| Severity | Count | Fixed by |
|---|---|---|
| Critical | 5 | Sub-phases A, F |
| High | 6 | Sub-phases A, B, C, D, E |
| Medium | 6 | Sub-phases A, B, E |
| **Total** | **17** | |
