<script lang="ts">
  import {
    rmsDbfs, snrDb, noiseFloorDb, signalStats,
    sessionStats, agcGainDb, spectralFlatness,
  } from '../stores/radio';

  function fmt(v: number, decimals: number = 1): string {
    return v.toFixed(decimals);
  }
</script>

<div class="stats-panel">
  <div class="panel-header">Signal</div>
  <div class="stats-grid">
    <div class="stat">
      <span class="label">RMS</span>
      <span class="value">{fmt($rmsDbfs)} dBFS</span>
    </div>
    <div class="stat">
      <span class="label">SNR</span>
      <span class="value">{fmt($snrDb)} dB</span>
    </div>
    <div class="stat">
      <span class="label">Noise</span>
      <span class="value">{fmt($noiseFloorDb)} dB</span>
    </div>
    <div class="stat">
      <span class="label">Flatness</span>
      <span class="value">{fmt($spectralFlatness, 3)}</span>
    </div>
    <div class="stat">
      <span class="label">Skewness</span>
      <span class="value">{fmt($signalStats.skewness, 2)}</span>
    </div>
    <div class="stat">
      <span class="label">Kurtosis</span>
      <span class="value">{fmt($signalStats.excess_kurtosis, 2)}</span>
    </div>
    <div class="stat">
      <span class="label">Crest</span>
      <span class="value">{fmt($signalStats.crest_factor_db)} dB</span>
    </div>
    <div class="stat">
      <span class="label">AGC</span>
      <span class="value">{fmt($agcGainDb)} dB</span>
    </div>
  </div>

  <div class="panel-header">Performance</div>
  <div class="stats-grid">
    <div class="stat">
      <span class="label">CPU</span>
      <span class="value">{fmt($sessionStats.cpu_load_percent)}%</span>
    </div>
    <div class="stat">
      <span class="label">Throughput</span>
      <span class="value">{fmt($sessionStats.throughput_msps, 3)} MS/s</span>
    </div>
    <div class="stat">
      <span class="label">Blocks</span>
      <span class="value">{$sessionStats.blocks_processed}</span>
    </div>
    <div class="stat">
      <span class="label">Drops</span>
      <span class="value" class:warn={$sessionStats.blocks_dropped > 0}>{$sessionStats.blocks_dropped}</span>
    </div>
  </div>
</div>

<style>
  .stats-panel {
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow-y: auto;
  }

  .panel-header {
    font-size: 10px;
    text-transform: uppercase;
    color: var(--text-secondary);
    letter-spacing: 1px;
    padding: 4px 8px;
    background: var(--bg-tertiary);
  }

  .stats-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 2px;
    padding: 4px 8px;
  }

  .stat {
    display: flex;
    justify-content: space-between;
    padding: 2px 0;
    font-size: 11px;
  }

  .label {
    color: var(--text-secondary);
  }

  .value {
    color: var(--text-primary);
    text-align: right;
  }

  .warn {
    color: var(--accent-red);
  }
</style>
