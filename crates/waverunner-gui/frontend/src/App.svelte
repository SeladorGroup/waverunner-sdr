<script lang="ts">
  import { onMount } from 'svelte';
  import {
    setupEventListeners,
    connected, frequency, sampleRate, sessionStats,
    statusMessage, errorMessage,
    cmdTune, cmdStartDemod, cmdStopDemod,
  } from './lib/stores/radio';
  import type { DemodConfig } from './lib/types';

  import Header from './lib/components/Header.svelte';
  import Spectrum from './lib/components/Spectrum.svelte';
  import Waterfall from './lib/components/Waterfall.svelte';
  import Controls from './lib/components/Controls.svelte';
  import DeviceConfig from './lib/components/DeviceConfig.svelte';
  import DecodedMessages from './lib/components/DecodedMessages.svelte';
  import SignalStats from './lib/components/SignalStats.svelte';
  import Settings from './lib/components/Settings.svelte';
  import AnalysisPanel from './lib/components/AnalysisPanel.svelte';
  import TrackingChart from './lib/components/TrackingChart.svelte';

  const STEP_SIZES = [1, 10, 100, 1000, 5000, 10000, 25000, 100000, 1000000, 10000000];
  const DEMOD_MODES = ['OFF', 'am', 'am-sync', 'fm', 'wfm', 'wfm-stereo', 'usb', 'lsb', 'cw'];

  let stepIndex = 6;
  let demodIndex = 0;

  onMount(() => {
    setupEventListeners();

    function handleKeydown(e: KeyboardEvent) {
      if (e.target instanceof HTMLInputElement || e.target instanceof HTMLSelectElement) return;
      if (!$connected) return;

      switch (e.key) {
        case 'ArrowUp':
        case 'j':
          e.preventDefault();
          cmdTune($frequency + STEP_SIZES[stepIndex]);
          break;
        case 'ArrowDown':
        case 'k':
          e.preventDefault();
          cmdTune(Math.max(0, $frequency - STEP_SIZES[stepIndex]));
          break;
        case 'ArrowRight':
        case 'l':
          e.preventDefault();
          if (stepIndex < STEP_SIZES.length - 1) stepIndex++;
          break;
        case 'ArrowLeft':
        case 'h':
          e.preventDefault();
          if (stepIndex > 0) stepIndex--;
          break;
        case 'm':
          demodIndex = (demodIndex + 1) % DEMOD_MODES.length;
          applyDemod();
          break;
        case 'M':
          demodIndex = (demodIndex + DEMOD_MODES.length - 1) % DEMOD_MODES.length;
          applyDemod();
          break;
      }
    }

    window.addEventListener('keydown', handleKeydown);
    return () => window.removeEventListener('keydown', handleKeydown);
  });

  function applyDemod() {
    const mode = DEMOD_MODES[demodIndex];
    if (mode === 'OFF') {
      cmdStopDemod();
    } else {
      const config: DemodConfig = {
        mode, audio_rate: 48000,
        bandwidth: null, bfo: null, squelch: null, deemph_us: null, output_wav: null,
      };
      cmdStartDemod(config);
    }
  }
</script>

<div class="app">
  <div class="top-bar">
    <Header />
    <div class="top-right">
      <DeviceConfig />
    </div>
  </div>

  <div class="main-area">
    <div class="viz-column">
      <div class="spectrum-area">
        <Spectrum />
      </div>
      <div class="waterfall-area">
        <Waterfall />
      </div>
      <TrackingChart />
    </div>

    <div class="side-column">
      <Controls />
      <SignalStats />
      <AnalysisPanel />
      <Settings />
    </div>
  </div>

  <div class="decoded-area">
    <DecodedMessages />
  </div>

  <div class="status-bar">
    <span class="status-left">
      {#if $errorMessage}
        <span class="error">{$errorMessage}</span>
      {:else if $statusMessage}
        {$statusMessage}
      {:else}
        Ready
      {/if}
    </span>
    <span class="status-right">
      CPU {$sessionStats.cpu_load_percent.toFixed(1)}%
      | {$sessionStats.blocks_processed} blocks
      | {$sessionStats.throughput_msps.toFixed(3)} MS/s
      {#if $sessionStats.blocks_dropped > 0}
        | <span class="error">{$sessionStats.blocks_dropped} drops</span>
      {/if}
    </span>
  </div>
</div>

<style>
  .app {
    display: grid;
    grid-template-rows: auto 1fr auto auto;
    height: 100vh;
    width: 100vw;
    overflow: hidden;
  }

  .top-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    border-bottom: 1px solid var(--border);
  }

  .top-bar :global(.header) {
    flex: 1;
  }

  .top-right {
    padding: 0 12px;
  }

  .main-area {
    display: grid;
    grid-template-columns: 1fr 260px;
    overflow: hidden;
  }

  .viz-column {
    display: grid;
    grid-template-rows: 1fr 1fr auto;
    overflow: hidden;
    border-right: 1px solid var(--border);
  }

  .spectrum-area {
    border-bottom: 1px solid var(--border);
  }

  .side-column {
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    background: var(--bg-secondary);
  }

  .decoded-area {
    height: 140px;
    border-top: 1px solid var(--border);
    background: var(--bg-secondary);
  }

  .status-bar {
    display: flex;
    justify-content: space-between;
    padding: 3px 12px;
    background: var(--bg-tertiary);
    border-top: 1px solid var(--border);
    font-size: 11px;
    color: var(--text-secondary);
  }

  .error {
    color: var(--accent-red);
  }
</style>
