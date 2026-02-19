<script lang="ts">
  import {
    displayDbMin, displayDbMax, showPeakHold as showPeakHoldStore,
  } from '../stores/radio';

  let dbMin = $state(-120);
  let dbMax = $state(0);
  let peakHold = $state(true);

  // Sync local state to stores
  $effect(() => { displayDbMin.set(dbMin); });
  $effect(() => { displayDbMax.set(dbMax); });
  $effect(() => { showPeakHoldStore.set(peakHold); });
</script>

<div class="settings-panel">
  <div class="panel-header">Display</div>
  <div class="setting">
    <label for="set-dbmin">dB Min</label>
    <input id="set-dbmin" type="range" min="-160" max="-20" bind:value={dbMin} />
    <span>{dbMin}</span>
  </div>
  <div class="setting">
    <label for="set-dbmax">dB Max</label>
    <input id="set-dbmax" type="range" min="-40" max="20" bind:value={dbMax} />
    <span>{dbMax}</span>
  </div>
  <div class="setting">
    <span class="setting-label">Peak Hold</span>
    <button onclick={() => peakHold = !peakHold} class:primary={peakHold}>
      {peakHold ? 'ON' : 'OFF'}
    </button>
  </div>
</div>

<style>
  .settings-panel {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 8px;
  }

  .panel-header {
    font-size: 10px;
    text-transform: uppercase;
    color: var(--text-secondary);
    letter-spacing: 1px;
    padding: 4px 0;
  }

  .setting {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
  }

  .setting label,
  .setting .setting-label {
    min-width: 70px;
    color: var(--text-secondary);
  }

  .setting input[type="range"] {
    flex: 1;
  }

  .setting span {
    min-width: 40px;
    text-align: right;
    font-size: 11px;
  }
</style>
