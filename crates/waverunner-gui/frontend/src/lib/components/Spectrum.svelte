<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { SpectrumRenderer } from '../canvas/spectrum';
  import { spectrumData, peakHold, detections, noiseFloorDb, displayDbMin, displayDbMax, showPeakHold } from '../stores/radio';

  let canvasEl: HTMLCanvasElement;
  let containerEl: HTMLDivElement;
  let renderer: SpectrumRenderer;
  let observer: ResizeObserver;

  let currentDbMin = -120;
  let currentDbMax = 0;

  onMount(() => {
    renderer = new SpectrumRenderer(canvasEl);
    renderer.setRange(currentDbMin, currentDbMax);

    observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const { width, height } = entry.contentRect;
        if (width > 0 && height > 0) {
          renderer.resize(width, height);
        }
      }
    });
    observer.observe(containerEl);

    renderer.start();

    const unsubs = [
      spectrumData.subscribe(() => updateRenderer()),
      peakHold.subscribe(() => updateRenderer()),
      detections.subscribe(() => updateRenderer()),
      noiseFloorDb.subscribe(() => updateRenderer()),
      displayDbMin.subscribe(v => {
        currentDbMin = v;
        renderer?.setRange(currentDbMin, currentDbMax);
      }),
      displayDbMax.subscribe(v => {
        currentDbMax = v;
        renderer?.setRange(currentDbMin, currentDbMax);
      }),
      showPeakHold.subscribe(v => {
        renderer?.setPeakHoldVisible(v);
      }),
    ];

    return () => {
      unsubs.forEach(u => u());
    };
  });

  onDestroy(() => {
    renderer?.stop();
    observer?.disconnect();
  });

  function updateRenderer() {
    let s: number[], p: number[], d: typeof $detections, nf: number;
    spectrumData.subscribe(v => s = v)();
    peakHold.subscribe(v => p = v)();
    detections.subscribe(v => d = v)();
    noiseFloorDb.subscribe(v => nf = v)();
    renderer?.setData(s!, p!, d!, nf!);
  }
</script>

<div class="spectrum-container" bind:this={containerEl}>
  <canvas bind:this={canvasEl}></canvas>
</div>

<style>
  .spectrum-container {
    width: 100%;
    height: 100%;
    overflow: hidden;
  }

  canvas {
    display: block;
  }
</style>
