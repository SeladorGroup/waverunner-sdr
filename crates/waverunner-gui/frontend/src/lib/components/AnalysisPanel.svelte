<script lang="ts">
  import {
    analysisResult, trackingActive, referenceCapture,
    measureSignal, analyzeBurst, estimateModulation,
    compareSpectra, captureReference, toggleTracking, exportData,
  } from '../stores/radio';
  import type { AnalysisResult } from '../types';

  function fmt(v: number, decimals: number = 1): string {
    return v.toFixed(decimals);
  }

  function resultType(r: AnalysisResult): string {
    if ('Measurement' in r) return 'measurement';
    if ('Burst' in r) return 'burst';
    if ('Modulation' in r) return 'modulation';
    if ('Comparison' in r) return 'comparison';
    if ('Bitstream' in r) return 'bitstream';
    if ('Tracking' in r) return 'tracking';
    if ('ExportComplete' in r) return 'export';
    return 'unknown';
  }

  async function handleExport() {
    const ts = Math.floor(Date.now() / 1000);
    await exportData('csv', `/tmp/waverunner_export_${ts}.csv`);
  }
</script>

<div class="analysis-panel">
  <div class="panel-header">Analysis</div>

  <div class="button-row">
    <button on:click={measureSignal} title="Measure signal">Measure</button>
    <button on:click={() => analyzeBurst(10)} title="Analyze bursts">Burst</button>
    <button on:click={estimateModulation} title="Estimate modulation">Mod</button>
    <button on:click={captureReference} title="Capture reference spectrum">Ref</button>
    <button on:click={compareSpectra} disabled={!$referenceCapture} title="Compare with reference">Cmp</button>
    <button on:click={handleExport} title="Export to CSV">Export</button>
  </div>

  <div class="button-row">
    <button on:click={toggleTracking} class:active={$trackingActive}>
      {$trackingActive ? 'Stop Track' : 'Start Track'}
    </button>
    {#if $referenceCapture}
      <span class="badge">REF</span>
    {/if}
  </div>

  {#if $analysisResult}
    <div class="results">
      {#if 'Measurement' in $analysisResult}
        {@const r = $analysisResult.Measurement}
        <div class="result-header">Measurement</div>
        <div class="result-row"><span class="label">-3dB BW</span><span class="value">{fmt(r.bandwidth_3db_hz / 1e3)} kHz</span></div>
        <div class="result-row"><span class="label">-6dB BW</span><span class="value">{fmt(r.bandwidth_6db_hz / 1e3)} kHz</span></div>
        <div class="result-row"><span class="label">OBW</span><span class="value">{fmt(r.occupied_bw_hz / 1e3)} kHz ({fmt(r.obw_percent)}%)</span></div>
        <div class="result-row"><span class="label">Ch Pwr</span><span class="value">{fmt(r.channel_power_dbfs)} dBFS</span></div>
        <div class="result-row"><span class="label">ACPR</span><span class="value">L:{fmt(r.acpr_lower_dbc)} U:{fmt(r.acpr_upper_dbc)} dBc</span></div>
        <div class="result-row"><span class="label">PAPR</span><span class="value">{fmt(r.papr_db)} dB</span></div>
        <div class="result-row"><span class="label">Offset</span><span class="value">{fmt(r.freq_offset_hz)} Hz</span></div>
      {:else if 'Burst' in $analysisResult}
        {@const r = $analysisResult.Burst}
        <div class="result-header">Burst Analysis</div>
        <div class="result-row"><span class="label">Bursts</span><span class="value">{r.burst_count}</span></div>
        <div class="result-row"><span class="label">Width</span><span class="value">{fmt(r.mean_pulse_width_us)} us</span></div>
        <div class="result-row"><span class="label">PRI</span><span class="value">{fmt(r.mean_pri_us)} us</span></div>
        <div class="result-row"><span class="label">Duty</span><span class="value">{fmt(r.duty_cycle * 100)}%</span></div>
        <div class="result-row"><span class="label">SNR</span><span class="value">{fmt(r.mean_burst_snr_db)} dB</span></div>
      {:else if 'Modulation' in $analysisResult}
        {@const r = $analysisResult.Modulation}
        <div class="result-header">Modulation</div>
        <div class="result-row"><span class="label">Type</span><span class="value mod-type">{r.modulation_type}</span></div>
        <div class="result-row"><span class="label">Confidence</span><span class="value">{fmt(r.confidence * 100)}%</span></div>
        {#if r.symbol_rate_hz}
          <div class="result-row"><span class="label">Symbol Rate</span><span class="value">{fmt(r.symbol_rate_hz)} baud</span></div>
        {/if}
        {#if r.fm_deviation_hz}
          <div class="result-row"><span class="label">FM Dev</span><span class="value">{fmt(r.fm_deviation_hz / 1e3)} kHz</span></div>
        {/if}
      {:else if 'Comparison' in $analysisResult}
        {@const r = $analysisResult.Comparison}
        <div class="result-header">Comparison</div>
        <div class="result-row"><span class="label">RMS Diff</span><span class="value">{fmt(r.rms_diff_db, 2)} dB</span></div>
        <div class="result-row"><span class="label">Peak</span><span class="value">{fmt(r.peak_diff_db, 2)} dB (bin {r.peak_diff_bin})</span></div>
        <div class="result-row"><span class="label">Corr</span><span class="value">{fmt(r.correlation, 4)}</span></div>
        <div class="result-row"><span class="label">New/Lost</span><span class="value">{r.new_signals.length} / {r.lost_signals.length}</span></div>
      {:else if 'ExportComplete' in $analysisResult}
        {@const r = $analysisResult.ExportComplete}
        <div class="result-header">Export Complete</div>
        <div class="result-row"><span class="label">File</span><span class="value">{r.path}</span></div>
        <div class="result-row"><span class="label">Format</span><span class="value">{r.format}</span></div>
      {/if}
    </div>
  {:else}
    <div class="empty">No analysis results yet</div>
  {/if}
</div>

<style>
  .analysis-panel {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .panel-header {
    font-size: 10px;
    text-transform: uppercase;
    color: var(--text-secondary);
    letter-spacing: 1px;
    padding: 4px 8px;
    background: var(--bg-tertiary);
  }

  .button-row {
    display: flex;
    gap: 4px;
    padding: 4px 8px;
    flex-wrap: wrap;
    align-items: center;
  }

  button {
    font-size: 10px;
    padding: 2px 8px;
    border: 1px solid var(--border);
    background: var(--bg-secondary);
    color: var(--text-primary);
    cursor: pointer;
    border-radius: 3px;
  }

  button:hover {
    background: var(--bg-tertiary);
  }

  button:disabled {
    opacity: 0.4;
    cursor: default;
  }

  button.active {
    background: var(--accent-green, #2d5a3d);
    border-color: var(--accent-green, #4a9);
  }

  .badge {
    font-size: 9px;
    padding: 1px 6px;
    background: var(--accent-cyan, #1a4a5a);
    color: var(--accent-cyan, #6cf);
    border-radius: 3px;
    text-transform: uppercase;
    letter-spacing: 1px;
  }

  .results {
    padding: 4px 8px;
  }

  .result-header {
    font-size: 10px;
    text-transform: uppercase;
    color: var(--accent-cyan, #6cf);
    letter-spacing: 1px;
    padding-bottom: 2px;
    border-bottom: 1px solid var(--border);
    margin-bottom: 2px;
  }

  .result-row {
    display: flex;
    justify-content: space-between;
    padding: 1px 0;
    font-size: 11px;
  }

  .label {
    color: var(--text-secondary);
  }

  .value {
    color: var(--text-primary);
    text-align: right;
  }

  .mod-type {
    color: var(--accent-cyan, #6cf);
    font-weight: bold;
  }

  .empty {
    padding: 8px;
    color: var(--text-secondary);
    font-size: 11px;
    font-style: italic;
  }
</style>
