<script lang="ts">
  import { connected, connectDevice, disconnectDevice, replayFile, getAvailableDevices } from '../stores/radio';
  import type { DeviceInfo, SessionConfig } from '../types';

  let showModal = $state(false);
  let devices = $state<DeviceInfo[]>([]);
  let selectedIndex = $state(0);
  let freqStr = $state('100');
  let rateStr = $state('2.048');
  let fftSize = $state(2048);
  let replayPath = $state('');
  let replayRate = $state('2.048');
  let replayFreq = $state('100');
  let loading = $state(false);
  let error = $state('');

  async function refresh() {
    try {
      devices = (await getAvailableDevices()) as DeviceInfo[];
    } catch (e) {
      devices = [];
    }
  }

  function open() {
    showModal = true;
    refresh();
  }

  function close() {
    showModal = false;
    error = '';
  }

  async function doConnect() {
    loading = true;
    error = '';
    try {
      const config: SessionConfig = {
        device_index: selectedIndex,
        frequency: parseFloat(freqStr) * 1e6,
        sample_rate: parseFloat(rateStr) * 1e6,
        gain: 'Auto',
        ppm: 0,
        fft_size: fftSize,
        pfa: 1e-4,
      };
      await connectDevice(config);
      close();
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function doReplay() {
    loading = true;
    error = '';
    try {
      await replayFile(
        replayPath,
        parseFloat(replayRate) * 1e6,
        parseFloat(replayFreq) * 1e6,
      );
      close();
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function doDisconnect() {
    try {
      await disconnectDevice();
    } catch (e) {
      error = String(e);
    }
  }
</script>

<div class="device-buttons">
  {#if $connected}
    <button class="danger" onclick={doDisconnect}>Disconnect</button>
  {:else}
    <button class="primary" onclick={open}>Connect</button>
  {/if}
</div>

{#if showModal}
  <div class="modal-backdrop" onclick={close} onkeydown={(e) => { if (e.key === 'Escape') close(); }} role="button" tabindex="-1">
    <!-- svelte-ignore a11y_interactive_supports_focus -->
    <div class="modal" onclick={(e) => e.stopPropagation()} onkeydown={() => {}} role="dialog">
      <h2>Device Configuration</h2>

      <div class="section">
        <h3>Live Device</h3>
        <div class="form-row">
          <label for="dc-device">Device</label>
          <select id="dc-device" bind:value={selectedIndex}>
            {#each devices as dev, i}
              <option value={i}>{dev.name}</option>
            {/each}
            {#if devices.length === 0}
              <option value={0}>No devices found</option>
            {/if}
          </select>
        </div>
        <div class="form-row">
          <label for="dc-freq">Freq (MHz)</label>
          <input id="dc-freq" type="text" bind:value={freqStr} />
        </div>
        <div class="form-row">
          <label for="dc-rate">Rate (MS/s)</label>
          <input id="dc-rate" type="text" bind:value={rateStr} />
        </div>
        <div class="form-row">
          <label for="dc-fft">FFT Size</label>
          <select id="dc-fft" bind:value={fftSize}>
            <option value={512}>512</option>
            <option value={1024}>1024</option>
            <option value={2048}>2048</option>
            <option value={4096}>4096</option>
            <option value={8192}>8192</option>
          </select>
        </div>
        <button class="primary" onclick={doConnect} disabled={loading || devices.length === 0}>
          {loading ? 'Connecting...' : 'Connect'}
        </button>
      </div>

      <div class="section">
        <h3>Replay File</h3>
        <div class="form-row">
          <label for="dc-rpath">Path</label>
          <input id="dc-rpath" type="text" bind:value={replayPath} placeholder="/path/to/recording.cf32" />
        </div>
        <div class="form-row">
          <label for="dc-rrate">Rate (MS/s)</label>
          <input id="dc-rrate" type="text" bind:value={replayRate} />
        </div>
        <div class="form-row">
          <label for="dc-rfreq">Freq (MHz)</label>
          <input id="dc-rfreq" type="text" bind:value={replayFreq} />
        </div>
        <button class="primary" onclick={doReplay} disabled={loading || !replayPath}>
          {loading ? 'Loading...' : 'Replay'}
        </button>
      </div>

      {#if error}
        <div class="error">{error}</div>
      {/if}

      <button onclick={close}>Cancel</button>
    </div>
  </div>
{/if}

<style>
  .device-buttons {
    display: flex;
    gap: 6px;
  }

  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.7);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }

  .modal {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 20px;
    min-width: 380px;
    max-width: 460px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .modal h2 {
    font-size: 16px;
    color: var(--accent-cyan);
  }

  .section {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px;
    background: var(--bg-tertiary);
    border-radius: 6px;
  }

  .section h3 {
    font-size: 11px;
    text-transform: uppercase;
    color: var(--text-secondary);
    letter-spacing: 1px;
    margin-bottom: 2px;
  }

  .form-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .form-row label {
    min-width: 80px;
    font-size: 12px;
    color: var(--text-secondary);
  }

  .form-row input,
  .form-row select {
    flex: 1;
  }

  .error {
    color: var(--accent-red);
    font-size: 12px;
    padding: 6px;
    background: rgba(248, 81, 73, 0.1);
    border-radius: 4px;
  }
</style>
