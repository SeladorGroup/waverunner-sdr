<script lang="ts">
  import { onMount } from 'svelte';
  import type { Bookmark, CaptureRecord } from '../types';
  import {
    connected, frequency, demodMode, enabledDecoders, recordingActive,
    cmdTune, replayFile,
    listRecentCaptures, listBookmarks,
    saveCurrentBookmark, removeBookmark, removeCapture, updateCaptureMetadata,
  } from '../stores/radio';

  let captures: CaptureRecord[] = [];
  let bookmarks: Bookmark[] = [];
  let bookmarkName = '';
  let bookmarkNotes = '';
  let captureFilter = '';
  let captureQuery = '';
  let filteredCaptures: CaptureRecord[] = [];
  let loading = false;
  let error = '';
  let editingCaptureId: string | null = null;
  let editLabel = '';
  let editNotes = '';
  let editTags = '';

  $: captureQuery = captureFilter.trim().toLowerCase();
  $: filteredCaptures = captures.filter((capture) => {
    if (!captureQuery) {
      return true;
    }
    const haystack = [
      capture.id,
      capture.label ?? '',
      capture.notes ?? '',
      capture.tags.join(' '),
      capture.format,
      capture.demod_mode ?? '',
      capture.decoder ?? '',
      capture.source,
      capture.path,
      capture.metadata_path ?? '',
      capture.report_path ?? '',
      capture.timeline_path ?? '',
      (capture.center_freq / 1e6).toFixed(3),
    ]
      .join(' ')
      .toLowerCase();
    return haystack.includes(captureQuery);
  });

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
        listRecentCaptures(24),
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
      if (editingCaptureId === capture.id) {
        cancelEdit();
      }
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }

  function parseTagList(value: string): string[] {
    return value
      .split(',')
      .map((tag) => tag.trim())
      .filter((tag) => tag.length > 0);
  }

  function startEdit(capture: CaptureRecord) {
    editingCaptureId = capture.id;
    editLabel = capture.label ?? '';
    editNotes = capture.notes ?? '';
    editTags = capture.tags.join(', ');
  }

  function cancelEdit() {
    editingCaptureId = null;
    editLabel = '';
    editNotes = '';
    editTags = '';
  }

  async function saveCapture(capture: CaptureRecord) {
    try {
      error = '';
      await updateCaptureMetadata(
        capture.id,
        editLabel.trim() || null,
        editNotes.trim() || null,
        parseTagList(editTags),
      );
      await refresh();
      cancelEdit();
    } catch (e) {
      error = String(e);
    }
  }

  async function deleteCaptureFiles(capture: CaptureRecord) {
    const name = capture.label ?? capture.id;
    if (!window.confirm(`Delete capture files for "${name}"? This cannot be undone.`)) {
      return;
    }

    try {
      error = '';
      await removeCapture(capture.id, true);
      if (editingCaptureId === capture.id) {
        cancelEdit();
      }
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
    <input bind:value={captureFilter} placeholder="Filter captures by label, tag, mode, decoder, or path" />
    <div class="section-subtitle">
      showing {filteredCaptures.length} of {captures.length}
    </div>
    {#if captures.length === 0}
      <div class="empty">No captures indexed yet</div>
    {:else if filteredCaptures.length === 0}
      <div class="empty">No captures match that filter</div>
    {:else}
      {#each filteredCaptures as capture}
        <div class="row">
          <div class="row-main">
            <div class="row-title">{capture.label ?? capture.id}</div>
            <div class="row-meta">
              {(capture.center_freq / 1e6).toFixed(3)} MHz
              {#if capture.duration_secs} | {capture.duration_secs.toFixed(1)}s{/if}
              | {capture.format}
              {#if capture.demod_mode} | {capture.demod_mode.toUpperCase()}{/if}
              {#if capture.decoder} | {capture.decoder}{/if}
              | {capture.source}
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
            {#if capture.metadata_path}
              <div class="row-path">meta: {capture.metadata_path}</div>
            {/if}
            {#if editingCaptureId === capture.id}
              <div class="edit-panel">
                <input bind:value={editLabel} placeholder="Label" />
                <textarea bind:value={editNotes} rows="3" placeholder="Notes"></textarea>
                <input bind:value={editTags} placeholder="tags, comma, separated" />
                <div class="row-actions">
                  <button class="ghost" onclick={() => saveCapture(capture)}>Save</button>
                  <button class="ghost" onclick={cancelEdit}>Cancel</button>
                </div>
              </div>
            {/if}
          </div>
          <div class="row-actions">
            <button class="ghost" onclick={() => cmdTune(capture.center_freq)} disabled={!$connected}>Tune</button>
            <button class="ghost" onclick={() => replayCapture(capture)}>Replay</button>
            <button class="ghost" onclick={() => startEdit(capture)}>
              {editingCaptureId === capture.id ? 'Editing' : 'Edit'}
            </button>
            <button class="ghost danger" onclick={() => deleteCapture(capture)}>Hide</button>
            <button class="ghost danger" onclick={() => deleteCaptureFiles(capture)}>Delete Files</button>
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

  .section-subtitle {
    font-size: 10px;
    color: var(--text-secondary);
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
    display: flex;
    flex-direction: column;
    gap: 4px;
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

  .edit-panel {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 6px;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 6px;
    background: rgba(255, 255, 255, 0.02);
  }

  input {
    width: 100%;
  }

  textarea {
    width: 100%;
    resize: vertical;
  }

  .error {
    color: var(--accent-red);
  }
</style>
