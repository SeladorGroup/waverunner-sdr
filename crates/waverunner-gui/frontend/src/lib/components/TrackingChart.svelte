<script lang="ts">
  import { trackingData, trackingActive } from '../stores/radio';
  import { onMount } from 'svelte';

  let canvas: HTMLCanvasElement;
  let width = 400;
  let height = 100;

  $: if (canvas && $trackingData) {
    drawChart();
  }

  onMount(() => {
    const observer = new ResizeObserver(entries => {
      for (const entry of entries) {
        width = entry.contentRect.width;
        height = entry.contentRect.height;
        if (canvas) {
          canvas.width = width;
          canvas.height = height;
          drawChart();
        }
      }
    });
    if (canvas?.parentElement) observer.observe(canvas.parentElement);
    return () => observer.disconnect();
  });

  function drawChart() {
    if (!canvas || !$trackingData) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    const data = $trackingData.snr;
    if (data.length < 2) return;

    ctx.clearRect(0, 0, width, height);

    // Background
    ctx.fillStyle = '#0a0f14';
    ctx.fillRect(0, 0, width, height);

    // Grid lines
    ctx.strokeStyle = '#1a2430';
    ctx.lineWidth = 1;
    for (let i = 1; i < 4; i++) {
      const y = (height / 4) * i;
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(width, y);
      ctx.stroke();
    }

    // Find value range
    let minVal = Infinity, maxVal = -Infinity;
    for (const [, v] of data) {
      if (v < minVal) minVal = v;
      if (v > maxVal) maxVal = v;
    }
    const range = Math.max(maxVal - minVal, 1);
    const padding = range * 0.1;
    const yMin = minVal - padding;
    const yMax = maxVal + padding;

    // Draw SNR line
    ctx.strokeStyle = '#4af';
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    for (let i = 0; i < data.length; i++) {
      const x = (i / (data.length - 1)) * width;
      const y = height - ((data[i][1] - yMin) / (yMax - yMin)) * height;
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    }
    ctx.stroke();

    // Labels
    ctx.fillStyle = '#6a7a8a';
    ctx.font = '9px monospace';
    ctx.textAlign = 'left';
    ctx.fillText(`SNR ${maxVal.toFixed(1)} dB`, 4, 10);
    ctx.fillText(`${minVal.toFixed(1)} dB`, 4, height - 4);

    // Summary
    const s = $trackingData.summary;
    ctx.textAlign = 'right';
    ctx.fillText(`mean ${s.snr_mean.toFixed(1)} dB | drift ${s.freq_drift_hz_per_sec.toFixed(2)} Hz/s | stab ${(s.stability_score * 100).toFixed(0)}%`, width - 4, 10);
  }
</script>

<div class="tracking-chart" class:active={$trackingActive}>
  <canvas bind:this={canvas} {width} {height}></canvas>
  {#if !$trackingData}
    <div class="empty">Tracking not active</div>
  {/if}
</div>

<style>
  .tracking-chart {
    position: relative;
    width: 100%;
    height: 80px;
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
  }

  .tracking-chart.active {
    border-color: var(--accent-green, #4a9);
  }

  canvas {
    display: block;
    width: 100%;
    height: 100%;
  }

  .empty {
    position: absolute;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    color: var(--text-secondary);
    font-size: 11px;
    font-style: italic;
  }
</style>
