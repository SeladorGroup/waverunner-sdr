<script lang="ts">
  import { frequency, sampleRate, gain, demodMode, connected } from '../stores/radio';

  function formatFreq(hz: number): string {
    if (hz >= 1e9) return (hz / 1e9).toFixed(6) + ' GHz';
    if (hz >= 1e6) return (hz / 1e6).toFixed(3) + ' MHz';
    if (hz >= 1e3) return (hz / 1e3).toFixed(1) + ' kHz';
    return hz.toFixed(0) + ' Hz';
  }

  function formatRate(hz: number): string {
    if (hz >= 1e6) return (hz / 1e6).toFixed(3) + ' MS/s';
    if (hz >= 1e3) return (hz / 1e3).toFixed(1) + ' kS/s';
    return hz.toFixed(0) + ' S/s';
  }

  function formatGain(g: typeof $gain): string {
    if (g === 'Auto') return 'AGC';
    if (typeof g === 'object' && 'Manual' in g) return g.Manual.toFixed(1) + ' dB';
    return '?';
  }
</script>

<header class="header">
  <div class="status-dot" class:connected={$connected}></div>
  <div class="freq-display">{formatFreq($frequency)}</div>
  <div class="info-group">
    <span class="label">Rate</span>
    <span class="value">{formatRate($sampleRate)}</span>
  </div>
  <div class="info-group">
    <span class="label">Gain</span>
    <span class="value">{formatGain($gain)}</span>
  </div>
  <div class="info-group">
    <span class="label">Mode</span>
    <span class="value mode">{$demodMode}</span>
  </div>
</header>

<style>
  .header {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 6px 12px;
    background: var(--bg-secondary);
    border-bottom: 1px solid var(--border);
    min-height: 36px;
  }

  .status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accent-red);
    flex-shrink: 0;
  }

  .status-dot.connected {
    background: var(--accent-green);
    box-shadow: 0 0 6px var(--accent-green);
  }

  .freq-display {
    font-size: 18px;
    font-weight: bold;
    color: var(--accent-cyan);
    letter-spacing: 0.5px;
    min-width: 200px;
  }

  .info-group {
    display: flex;
    align-items: baseline;
    gap: 4px;
  }

  .label {
    color: var(--text-secondary);
    font-size: 10px;
    text-transform: uppercase;
  }

  .value {
    color: var(--text-primary);
  }

  .mode {
    color: var(--accent-green);
    font-weight: bold;
  }
</style>
