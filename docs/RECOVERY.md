# WaveRunner Crash Recovery

## Overview

WaveRunner automatically saves session checkpoints during operation. If the application crashes or is killed, the checkpoint preserves enough state to understand and resume work.

## Checkpoint Behavior

- **Location:** `~/.cache/waverunner/checkpoint.json` (respects `XDG_CACHE_HOME`)
- **Frequency:** Every 1000 blocks (~8 seconds at 2.048 MS/s)
- **Write method:** Atomic (write to `.tmp`, then rename) — never leaves a corrupt file
- **Clean shutdown:** Checkpoint is deleted on normal exit

## Checkpoint Contents

| Field | Description |
|---|---|
| `schema_version` | Format version (currently 1) |
| `timestamp` | ISO 8601 when checkpoint was written |
| `config` | Full SessionConfig (frequency, sample rate, gain, etc.) |
| `frequency` | Current tuned frequency (may differ from config if retuned) |
| `gain` | Current gain mode |
| `active_decoders` | List of enabled decoder names |
| `recording_path` | Path to in-progress recording (if any) |
| `tracking_active` | Whether signal tracking was running |
| `timeline_entries` | Count of timeline events logged |
| `blocks_processed` | Total blocks processed before crash |
| `events_dropped` | Total events dropped before crash |

## CLI Commands

```bash
# Check for a checkpoint
waverunner recover

# View full checkpoint as JSON
waverunner recover --show

# Remove a stale checkpoint
waverunner recover --clear
```

## Recovery Scenarios

### Application crash during recording
The checkpoint will show `recording_path` with the file being written. The partial recording file is valid up to the last flushed write. Use the checkpoint's `config` to understand the recording parameters.

### Application crash during analysis
Analysis results are not checkpointed (they're on-demand). Re-run the analysis after restarting.

### Corrupt checkpoint
If the checkpoint file is damaged (partial write before atomic rename completed), `load_checkpoint()` returns `None` and logs a warning. Use `waverunner recover --clear` to remove it.

### Newer version checkpoint
If a checkpoint was written by a newer WaveRunner version (higher `schema_version`), it will be ignored with a warning. Use `--clear` to remove it.
