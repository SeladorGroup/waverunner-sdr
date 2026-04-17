<script lang="ts">
  import { onMount } from 'svelte';
  import type { Bookmark, CaptureRecord } from '../types';
  import {
    connected, frequency, demodMode, enabledDecoders, recordingActive,
    cmdTune, replayFile,
    listRecentCaptures, listBookmarks,
    saveCurrentBookmark, removeBookmark, removeCapture,
  } from '../stores/radio';

  let captures: CaptureRecord[] = [];
  let bookmarks: Bookmark[] = [];
  let bookmarkName = '';
  let bookmarkNotes = '';
  let loading = false;
  let error = '';

  onMount(() => {
    let lastRecordingActive = false;
    const unsubscribe = recordingActive.subscribe((active) => {
      if (lastRecordingActive && !active) {
        refresh();
      }
      lastRecordingActive = active;
    });
    refresh();
    return unsubscribe;
  });

  async function refresh() {
    loading = true;
    error = '';
    try {
      [captures, bookmarks] = await Promise.all([
        listRecentCaptures(8),
        listBookmarks(),
      ]);
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function saveBookmark() {
    const name = bookmarkName.trim() || `${($frequency / 1e6).toFixed(3)} MHz`;
    try {
      await saveCurrentBookmark(
        name,
        $demodMode !== 'OFF' ? $demodMode.toLowerCase() : null,
        $enabledDecoders.length === 1 ? $enabledDecoders[0] : null,
        bookmarkNotes.trim() || null,
      );
      bookmarkName = '';
      bookmarkNotes = '';
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }

  async function deleteBookmark(name: string) {
    try {
      await removeBookmark(name);
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }

  function tuneBookmark(bookmark: Bookmark) {
    cmdTune(bookmark.frequency_hz);
  }

  async function replayCapture(capture: CaptureRecord) {
    try {
      error = '';
      await replayFile(capture.metadata_path ?? capture.path);
    } catch (e) {
      error = String(e);
    }
  }

  async function deleteCapture(capture: CaptureRecord) {
    try {
      error = '';
      await removeCapture(capture.id, false);
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }
</script>

<div class="library-panel">
  <div class="panel-header">
    <span>Workflow</span>
    <button class="ghost" onclick={refresh} disabled={loading}>
      {loading ? '...' : 'Refresh'}
    </button>
  </div>

  <div class="section">
    <div class="section-title">Save Bookmark</div>
    <input bind:value={bookmarkName} placeholder="Bookmark name" />
    <input bind:value={bookmarkNotes} placeholder="Optional note" />
    <button onclick={saveBookmark} disabled={!$connected}>Save Current</button>
  </div>

  <div class="section">
    <div class="section-title">Bookmarks</div>
    {#if bookmarks.length === 0}
      <div class="empty">No bookmarks yet</div>
    {:else}
      {#each bookmarks as bookmark}
        <div class="row">
          <div class="row-main">
            <div class="row-title">{bookmark.name}</div>
            <div class="row-meta">
              {(bookmark.frequency_hz / 1e6).toFixed(3)} MHz
              {#if bookmark.mode} | {bookmark.mode.toUpperCase()}{/if}
              {#if bookmark.decoder} | {bookmark.decoder}{/if}
            </div>
          </div>
          <div class="row-actions">
            <button class="ghost" onclick={() => tuneBookmark(bookmark)}>Tune</button>
            <button class="ghost danger" onclick={() => deleteBookmark(bookmark.name)}>Del</button>
          </div>
        </div>
      {/each}
    {/if}
  </div>

  <div class="section">
    <div class="section-title">Recent Captures</div>
    {#if captures.length === 0}
      <div class="empty">No captures indexed yet</div>
    {:else}
      {#each captures as capture}
        <div class="row">
          <div class="row-main">
            <div class="row-title">{capture.label ?? capture.id}</div>
            <div class="row-meta">
              {(capture.center_freq / 1e6).toFixed(3)} MHz
              {#if capture.duration_secs} | {capture.duration_secs.toFixed(1)}s{/if}
              | {capture.format}
              {#if capture.timeline_path} | timeline{/if}
              {#if capture.report_path} | report{/if}
            </div>
            {#if capture.notes}
              <div class="row-notes">{capture.notes}</div>
            {/if}
            {#if capture.tags.length > 0}
              <div class="row-notes">tags: {capture.tags.join(', ')}</div>
            {/if}
            <div class="row-path">{capture.path}</div>
          </div>
          <div class="row-actions">
            <button class="ghost" onclick={() => cmdTune(capture.center_freq)} disabled={!$connected}>Tune</button>
            <button class="ghost" onclick={() => replayCapture(capture)}>Replay</button>
            <button class="ghost danger" onclick={() => deleteCapture(capture)}>Hide</button>
          </div>
        </div>
      {/each}
    {/if}
  </div>

  {#if error}
    <div class="error">{error}</div>
  {/if}
</div>

<style>
  .library-panel {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 8px;
  }

  .panel-header,
  .row,
  .row-actions {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
  }

  .panel-header {
    font-size: 10px;
    text-transform: uppercase;
    color: var(--text-secondary);
    letter-spacing: 1px;
    padding-bottom: 4px;
    border-bottom: 1px solid var(--border);
  }

  .section {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px;
    background: var(--bg-tertiary);
    border-radius: 6px;
  }

  .section-title {
    font-size: 10px;
    text-transform: uppercase;
    color: var(--accent-cyan);
    letter-spacing: 1px;
  }

  .row {
    align-items: flex-start;
    border-top: 1px solid rgba(255, 255, 255, 0.05);
    padding-top: 6px;
  }

  .row:first-of-type {
    border-top: 0;
    padding-top: 0;
  }

  .row-main {
    min-width: 0;
    flex: 1;
  }

  .row-title {
    font-size: 12px;
    color: var(--text-primary);
  }

  .row-meta,
  .row-notes,
  .row-path,
  .empty,
  .error {
    font-size: 11px;
    color: var(--text-secondary);
  }

  .row-path {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .ghost {
    background: transparent;
  }

  .danger {
    color: var(--accent-red);
  }

  input {
    width: 100%;
  }

  .error {
    color: var(--accent-red);
  }
</style>
