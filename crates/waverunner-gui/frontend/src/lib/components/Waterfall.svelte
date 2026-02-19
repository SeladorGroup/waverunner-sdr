<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { WaterfallRenderer } from '../canvas/waterfall';
  import { spectrumData, displayDbMin, displayDbMax } from '../stores/radio';

  let canvasEl: HTMLCanvasElement;
  let containerEl: HTMLDivElement;
  let renderer: WaterfallRenderer;
  let observer: ResizeObserver;

  onMount(() => {
    renderer = new WaterfallRenderer(canvasEl);
    renderer.setRange(-120, 0);

    observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const { width, height } = entry.contentRect;
        if (width > 0 && height > 0) {
          renderer.resize(width, height);
        }
      }
    });
    observer.observe(containerEl);

    let currentDbMin = -120;
    let currentDbMax = 0;

    const unsubs = [
      spectrumData.subscribe((data) => {
        if (data.length > 0) {
          renderer.pushRow(data);
        }
      }),
      displayDbMin.subscribe(v => {
        currentDbMin = v;
        renderer?.setRange(currentDbMin, currentDbMax);
      }),
      displayDbMax.subscribe(v => {
        currentDbMax = v;
        renderer?.setRange(currentDbMin, currentDbMax);
      }),
    ];

    return () => { unsubs.forEach(u => u()); };
  });

  onDestroy(() => {
    observer?.disconnect();
  });
</script>

<div class="waterfall-container" bind:this={containerEl}>
  <canvas bind:this={canvasEl}></canvas>
</div>

<style>
  .waterfall-container {
    width: 100%;
    height: 100%;
    overflow: hidden;
  }

  canvas {
    display: block;
  }
</style>
