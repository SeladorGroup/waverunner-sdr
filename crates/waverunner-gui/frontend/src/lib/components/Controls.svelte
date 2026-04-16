<script lang="ts">
  import { onMount } from 'svelte';
  import {
    frequency, gain, connected, demodMode, sampleRate,
    activeMode, modeStatus,
    cmdTune, cmdSetGain, cmdStartDemod, cmdStopDemod,
    cmdEnableDecoder, cmdDisableDecoder,
    cmdStartRecord, cmdStopRecord,
    getAvailableDecoders, listProfiles,
    activateProfile, activateGeneralScan, deactivateMode,
  } from '../stores/radio';
  import type { DemodConfig } from '../types';
  import type { ProfileInfo } from '../stores/radio';

  const STEP_SIZES = [
    { label: '1 Hz', value: 1 },
    { label: '10 Hz', value: 10 },
    { label: '100 Hz', value: 100 },
    { label: '1 kHz', value: 1000 },
    { label: '5 kHz', value: 5000 },
    { label: '10 kHz', value: 10000 },
    { label: '25 kHz', value: 25000 },
    { label: '100 kHz', value: 100000 },
    { label: '1 MHz', value: 1000000 },
  ];

  const DEMOD_MODES = ['OFF', 'am', 'am-sync', 'fm', 'wfm', 'wfm-stereo', 'usb', 'lsb', 'cw'];
  let decoders: string[] = [];
  let profiles: ProfileInfo[] = [];

  onMount(async () => {
    try {
      decoders = await getAvailableDecoders();
    } catch {
      decoders = [];
    }
    try {
      profiles = await listProfiles();
    } catch {
      profiles = [];
    }
  });

  let freqInput = '';
  let stepIndex = 6; // 25 kHz default
  let selectedDemod = 'OFF';
  let selectedDecoder = '';
  let activeDecoder = '';
  let isRecording = false;
  let gainValue = 0;
  let gainAuto = true;
  let selectedProfile = '';

  function tuneUp() {
    const newFreq = $frequency + STEP_SIZES[stepIndex].value;
    cmdTune(newFreq);
  }

  function tuneDown() {
    const newFreq = Math.max(0, $frequency - STEP_SIZES[stepIndex].value);
    cmdTune(newFreq);
  }

  function submitFreq() {
    const val = parseFrequency(freqInput);
    if (val !== null) {
      cmdTune(val);
      freqInput = '';
    }
  }

  function parseFrequency(s: string): number | null {
    const match = s.trim().match(/^([\d.]+)\s*([gGmMkK])?/);
    if (!match) return null;
    let val = parseFloat(match[1]);
    if (isNaN(val)) return null;
    const suffix = match[2]?.toUpperCase();
    if (suffix === 'G') val *= 1e9;
    else if (suffix === 'M') val *= 1e6;
    else if (suffix === 'K') val *= 1e3;
    return val;
  }

  function setDemod() {
    if (selectedDemod === 'OFF') {
      cmdStopDemod();
    } else {
      const config: DemodConfig = {
        mode: selectedDemod,
        audio_rate: 48000,
        bandwidth: null,
        bfo: null,
        squelch: null,
        deemph_us: null,
        output_wav: null,
        emit_visualization: true,
        spectrum_update_interval_blocks: 1,
      };
      cmdStartDemod(config);
    }
  }

  function toggleDecoder() {
    if (activeDecoder === selectedDecoder) {
      cmdDisableDecoder(activeDecoder);
      activeDecoder = '';
    } else {
      if (activeDecoder) cmdDisableDecoder(activeDecoder);
      cmdEnableDecoder(selectedDecoder);
      activeDecoder = selectedDecoder;
    }
  }

  function toggleGainMode() {
    gainAuto = !gainAuto;
    if (gainAuto) {
      cmdSetGain('Auto');
    } else {
      cmdSetGain({ Manual: gainValue });
    }
  }

  function onGainSlider() {
    if (!gainAuto) {
      cmdSetGain({ Manual: gainValue });
    }
  }
</script>

<div class="controls">
  <section>
    <h3>Frequency</h3>
    <div class="freq-row">
      <input
        type="text"
        placeholder="e.g. 433.92M"
        bind:value={freqInput}
        onkeydown={(e) => { if (e.key === 'Enter') submitFreq(); }}
        disabled={!$connected}
      />
      <button onclick={submitFreq} disabled={!$connected}>Go</button>
    </div>
    <div class="step-row">
      <button onclick={tuneDown} disabled={!$connected}>-</button>
      <select bind:value={stepIndex}>
        {#each STEP_SIZES as s, i}
          <option value={i}>{s.label}</option>
        {/each}
      </select>
      <button onclick={tuneUp} disabled={!$connected}>+</button>
    </div>
  </section>

  <section>
    <h3>Gain</h3>
    <div class="gain-row">
      <button onclick={toggleGainMode} class:primary={gainAuto}>
        {gainAuto ? 'AGC' : 'Manual'}
      </button>
      {#if !gainAuto}
        <input
          type="range"
          min="0"
          max="50"
          step="0.5"
          bind:value={gainValue}
          oninput={onGainSlider}
        />
        <span>{gainValue.toFixed(1)} dB</span>
      {/if}
    </div>
  </section>

  <section>
    <h3>Demodulator</h3>
    <div class="demod-row">
      <select bind:value={selectedDemod} onchange={setDemod} disabled={!$connected}>
        {#each DEMOD_MODES as mode}
          <option value={mode}>{mode.toUpperCase()}</option>
        {/each}
      </select>
    </div>
  </section>

  <section>
    <h3>Decoder</h3>
    <div class="decoder-row">
      <select bind:value={selectedDecoder} disabled={!$connected}>
        <option value="">None</option>
        {#each decoders as dec}
          <option value={dec}>{dec}</option>
        {/each}
      </select>
      <button onclick={toggleDecoder} disabled={!$connected || !selectedDecoder}>
        {activeDecoder === selectedDecoder && activeDecoder ? 'Stop' : 'Start'}
      </button>
    </div>
  </section>

  <section>
    <h3>Mode</h3>
    <div class="mode-row">
      <select bind:value={selectedProfile} disabled={!$connected}>
        <option value="">Off</option>
        {#each profiles as p}
          <option value={p.name}>{p.name}</option>
        {/each}
        <option value="general">General Scan</option>
      </select>
      <button
        onclick={() => {
          if (selectedProfile === '') {
            deactivateMode();
          } else if (selectedProfile === 'general') {
            activateGeneralScan($frequency - $sampleRate / 2, $frequency + $sampleRate / 2);
          } else {
            activateProfile(selectedProfile);
          }
        }}
        disabled={!$connected}
      >
        {$activeMode ? 'Switch' : 'Start'}
      </button>
    </div>
    {#if $modeStatus}
      <div class="mode-status">{$modeStatus}</div>
    {/if}
  </section>

  <section>
    <h3>Record</h3>
    <button
      onclick={() => { isRecording = !isRecording; isRecording ? cmdStartRecord('recording.cf32', 'cf32') : cmdStopRecord(); }}
      disabled={!$connected}
      class:danger={isRecording}
    >
      {isRecording ? 'Stop' : 'Record'}
    </button>
  </section>
</div>

<style>
  .controls {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 8px;
    overflow-y: auto;
  }

  section {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  h3 {
    font-size: 10px;
    text-transform: uppercase;
    color: var(--text-secondary);
    letter-spacing: 1px;
  }

  .freq-row,
  .step-row,
  .gain-row,
  .demod-row,
  .decoder-row {
    display: flex;
    gap: 4px;
    align-items: center;
  }

  .freq-row input {
    flex: 1;
    min-width: 0;
  }

  .step-row select {
    flex: 1;
  }

  .gain-row input[type="range"] {
    flex: 1;
  }

  .gain-row span {
    min-width: 55px;
    text-align: right;
    font-size: 11px;
  }

  .demod-row select,
  .decoder-row select,
  .mode-row select {
    flex: 1;
  }

  .mode-row {
    display: flex;
    gap: 4px;
    align-items: center;
  }

  .mode-status {
    font-size: 10px;
    color: var(--text-secondary);
    padding: 2px 0;
  }
</style>
