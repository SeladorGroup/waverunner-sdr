# WaveRunner Gap Audit Report — Phase 10

**Date:** 2026-02-15
**Baseline:** 451 tests, 0 clippy warnings, phases 1–9 complete

## Capability Matrix

| Capability | Status | Location | Notes |
|---|---|---|---|
| **Workflow** | | | |
| Scan -> Detect -> Decode/Record | done | `manager.rs:533-1082` | Full pipeline |
| Scan result export to file | **missing** | `scan.rs` | stdout-only, no `--output` |
| Session event log / timeline | **missing** | — | Events are ephemeral ring buffers |
| Bookmarking / annotations | **missing** | — | Zero annotation system |
| Structured report generation | **missing** | — | No session summary export |
| **Observability** | | | |
| Pipeline health (drops, occupancy) | done | `mod.rs:136-151` | SessionStats has buffer_occupancy, events_dropped |
| Load shedding indicator | done | `mod.rs:212-213` | StatusUpdate::LoadShedding(u8) |
| Per-stage latency breakdown | **missing** | `manager.rs:1009` | Single processing_time_us only |
| Health severity states | **missing** | — | No Normal/Warning/Critical model |
| **Export & Interop** | | | |
| SigMF recording + meta | done | `sigmf.rs` | Full v1.0.0 |
| CSV/JSON for analysis | done | `export.rs:22-27` | ExportFormat::Csv, ExportFormat::Json |
| WAV audio export | done | `manager.rs:252-277` | IQ WAV + demod audio |
| TSV format variant | **missing** | `export.rs` | Only CSV and JSON |
| Session bundle (ZIP) | **missing** | — | No composite export |
| **Session Workspace** | | | |
| Pause / resume | **missing** | — | No pause command |
| Session state persistence | **missing** | — | All state lost on exit |
| Workspace directory structure | **missing** | — | No organized storage |
| TUI 50-item ring buffer | needs hardening | `app.rs:66` | MAX_DECODED_MESSAGES = 50 |
| **Macro Workflows** | | | |
| Profile auto-record (SNR) | done | `mode/profile.rs` | mode system |
| Condition-based rules | **missing** | — | No rule engine |
| Scan-then-record automation | **missing** | — | Manual intervention required |
| Decoder rolling summaries | **missing** | — | No aggregation |
| **Replay + Annotation** | | | |
| ReplayDevice (cf32/cu8/WAV) | done | `replay.rs` | Pacing, looping, format support |
| SigMF annotation read on replay | **missing** | — | read_sigmf_meta exists but not wired |
| Interactive annotation during replay | **missing** | — | No annotation UI |
| **Hardening** | | | |
| Blocking sends in hot path | done | `manager.rs:136-140` | All high-freq events use try_send |
| Soak test coverage | needs hardening | `soak_test.rs` | Only 1.5s duration |

## Phase 10 Scope

### Implemented (Sub-phases A–D)

- **A:** Session event log + annotations (timeline.rs, Command/Event variants, TUI keys, tests)
- **B:** Scan export + session reports (--output/--format CLI, report.rs, TSV, TUI/GUI integration)
- **C:** Health model + latency breakdown (HealthStatus, LatencyBreakdown, instrumentation, TUI/GUI)
- **D:** Extended soak tests + health transition tests

### Deferred to Phase 11+ (Sub-phase E)

- **Session workspace / pause-resume:** Requires careful design around hardware lifecycle. Pause is not just "stop RX" — decoder state, demod PLL lock, tracker state all need preservation.
- **ZIP session bundle:** Adds `zip` crate dependency, moderate scope. Can be done independently later.
- **Macro/rule engine:** YAML-based automation — significant scope, unclear demand. Profile auto-record covers the common case.
- **Replay annotation overlay:** SigMF annotations exist and `read_sigmf_meta()` works. Wiring into replay display is straightforward but depends on Sub-phase A being done first.
- **Decoder rolling summaries:** Needs requirements clarification (time windows, metrics).
