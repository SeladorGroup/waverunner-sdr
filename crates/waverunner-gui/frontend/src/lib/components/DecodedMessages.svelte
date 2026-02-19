<script lang="ts">
  import { decodedMessages } from '../stores/radio';
  import { tick } from 'svelte';

  let listEl: HTMLDivElement;

  const DECODER_COLORS: Record<string, string> = {
    'pocsag': '#58a6ff',
    'pocsag-512': '#58a6ff',
    'pocsag-1200': '#58a6ff',
    'pocsag-2400': '#58a6ff',
    'POCSAG': '#58a6ff',
    'adsb': '#3fb950',
    'ADS-B': '#3fb950',
    'rds': '#d29922',
    'RDS': '#d29922',
  };

  function decoderColor(name: string): string {
    return DECODER_COLORS[name] ?? '#c9d1d9';
  }

  function formatElapsed(ms: number): string {
    const s = Math.floor(ms / 1000);
    const m = Math.floor(s / 60);
    const ss = s % 60;
    return `${m}:${ss.toString().padStart(2, '0')}`;
  }

  $effect(() => {
    if ($decodedMessages.length > 0) {
      tick().then(() => {
        if (listEl) listEl.scrollTop = listEl.scrollHeight;
      });
    }
  });
</script>

<div class="decoded-panel">
  <div class="panel-header">Decoded Messages</div>
  <div class="message-list" bind:this={listEl}>
    {#each $decodedMessages as msg}
      <div class="message">
        <span class="time">{formatElapsed(msg.elapsed_ms)}</span>
        <span class="decoder" style="color: {decoderColor(msg.decoder)}">[{msg.decoder}]</span>
        <span class="summary">{msg.summary}</span>
        {#if Object.keys(msg.fields).length > 0}
          <div class="fields">
            {#each Object.entries(msg.fields) as [k, v]}
              <span class="field"><span class="fk">{k}:</span> {v}</span>
            {/each}
          </div>
        {/if}
      </div>
    {/each}
    {#if $decodedMessages.length === 0}
      <div class="empty">No messages yet</div>
    {/if}
  </div>
</div>

<style>
  .decoded-panel {
    display: flex;
    flex-direction: column;
    height: 100%;
  }

  .panel-header {
    font-size: 10px;
    text-transform: uppercase;
    color: var(--text-secondary);
    letter-spacing: 1px;
    padding: 4px 8px;
    background: var(--bg-tertiary);
    border-bottom: 1px solid var(--border);
  }

  .message-list {
    flex: 1;
    overflow-y: auto;
    padding: 4px 8px;
  }

  .message {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 3px 0;
    border-bottom: 1px solid var(--border);
    font-size: 12px;
    align-items: baseline;
  }

  .time {
    color: var(--text-secondary);
    min-width: 40px;
  }

  .decoder {
    font-weight: bold;
    min-width: 70px;
  }

  .summary {
    flex: 1;
  }

  .fields {
    width: 100%;
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    padding-left: 46px;
    font-size: 11px;
    color: var(--text-secondary);
  }

  .fk {
    color: var(--accent-cyan);
  }

  .empty {
    color: var(--text-secondary);
    padding: 12px 0;
    text-align: center;
    font-size: 12px;
  }
</style>
