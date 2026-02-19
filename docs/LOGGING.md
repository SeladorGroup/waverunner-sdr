# WaveRunner Structured Logging Contract

## Overview

WaveRunner uses the `tracing` crate for structured logging. All log events follow a consistent field schema for machine-parseable output.

## Field Contract

| Field | Type | Source | Description |
|---|---|---|---|
| `timestamp` | ISO 8601 | tracing-subscriber | When the event occurred |
| `level` | string | tracing | TRACE, DEBUG, INFO, WARN, ERROR |
| `session_id` | string (8 hex chars) | `logging::new_session_id()` | Unique per session invocation |
| `component` | string | `logging::components::*` | Subsystem: manager, decoder, demod, etc. |
| `event` | string | `logging::events::*` | Machine-readable event code |
| `message` | string | tracing | Human-readable description |

## Component Names

Defined in `wavecore::logging::components`:

- `manager` — session management and pipeline orchestration
- `decoder` — protocol decoders (POCSAG, ADS-B, etc.)
- `demod` — audio demodulation chain
- `hardware` — SDR device interaction
- `pipeline` — sample buffer and backpressure
- `analysis` — measurement, tracking, modulation analysis
- `recording` — IQ recording to disk
- `mode` — mission profiles and general scan
- `export` — data export (CSV, JSON, TSV)
- `cli` / `tui` / `gui` — frontend-specific events

## Event Codes

Defined in `wavecore::logging::events`:

- `session_start` / `session_stop`
- `block_processed`
- `load_shedding` — backpressure level changed
- `health_changed` — pipeline health status changed
- `decoder_enabled` / `decoder_disabled`
- `recording_start` / `recording_stop`
- `checkpoint_saved`
- `export_complete`
- `tune` / `gain_change`
- `error`

## JSON Log Output

Enable with `--json-log` CLI flag. Example output:

```json
{"timestamp":"2026-02-15T12:00:00Z","level":"INFO","session_id":"a1b2c3d4","component":"manager","event":"session_start","message":"Session started at 100.0 MHz"}
```

## Usage Pattern

```rust
tracing::info!(
    session_id = %ctx.session_id,
    component = logging::components::MANAGER,
    event = logging::events::BLOCK_PROCESSED,
    blocks = count,
    "Processed {} blocks", count
);
```
