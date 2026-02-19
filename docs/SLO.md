# WaveRunner Service Level Objectives (SLOs)

Machine-readable targets are defined in `slo.toml` at the project root.

## Drop Budget

| Metric | Target | Rationale |
|---|---|---|
| Block drop rate | 0.0 (zero tolerance) | Dropped blocks mean lost RF data — unacceptable for recording or decode |
| Event drop rate | ≤ 0.1% | UI refresh can tolerate occasional skipped frames |

## Latency

| Metric | Target | Rationale |
|---|---|---|
| Total block processing | ≤ 500 µs | Must complete within one block period at 2.048 MS/s (≈500 µs) |
| FFT computation | ≤ 200 µs | Largest single stage; budget leaves room for CFAR + decoders |
| CFAR detection | ≤ 100 µs | Should not dominate the pipeline |

## Throughput

| Metric | Target | Rationale |
|---|---|---|
| Sustained throughput | ≥ 2.0 MS/s | Must keep up with RTL-SDR's default sample rate |

## Startup

| Metric | Target | Rationale |
|---|---|---|
| First Stats event | Within 50 blocks | Session should be producing data within ~0.4s of start |

## Export

| Metric | Target | Rationale |
|---|---|---|
| Export completion | ≤ 5000 ms | Typical spectrum/tracking exports should be near-instant |

## Enforcement

- SLO checks run in soak tests via `Slo::check_stats()`
- Violations cause test failures in CI
- Runtime: `SessionStats` includes `health` field with Normal/Warning/Critical levels
- Load shedding activates at buffer occupancy ≥25% (light) and ≥50% (heavy)
