export class WaterfallRenderer {
  private canvas: HTMLCanvasElement;
  private ctx: CanvasRenderingContext2D;
  private imageData: ImageData | null = null;
  private colormap: Uint8Array; // 256 * 4 RGBA entries

  // Display range
  private dbMin = -120;
  private dbMax = 0;

  constructor(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d", { willReadFrequently: true })!;
    this.colormap = buildTurboColormap();
  }

  setRange(dbMin: number, dbMax: number): void {
    this.dbMin = dbMin;
    this.dbMax = dbMax;
  }

  resize(width: number, height: number): void {
    const dpr = window.devicePixelRatio || 1;
    this.canvas.width = width * dpr;
    this.canvas.height = height * dpr;
    this.canvas.style.width = `${width}px`;
    this.canvas.style.height = `${height}px`;
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    this.imageData = null;
  }

  pushRow(spectrumDb: number[]): void {
    const w = this.canvas.width;
    const h = this.canvas.height;
    if (w === 0 || h === 0) return;

    const ctx = this.ctx;

    // Initialize imageData if needed
    if (!this.imageData || this.imageData.width !== w || this.imageData.height !== h) {
      this.imageData = ctx.createImageData(w, h);
      // Fill with black
      for (let i = 3; i < this.imageData.data.length; i += 4) {
        this.imageData.data[i] = 255;
      }
    }

    const data = this.imageData.data;
    const rowBytes = w * 4;

    // Scroll down: copy rows [0..h-2] to [1..h-1]
    data.copyWithin(rowBytes, 0, (h - 1) * rowBytes);

    // Write new row at top (row 0)
    const bins = spectrumDb.length;
    for (let x = 0; x < w; x++) {
      const binIdx = Math.floor((x / w) * bins);
      const db = binIdx < bins ? spectrumDb[binIdx] : this.dbMin;
      const normalized = Math.max(0, Math.min(1, (db - this.dbMin) / (this.dbMax - this.dbMin)));
      const colorIdx = Math.floor(normalized * 255);

      const offset = x * 4;
      const cmOffset = colorIdx * 4;
      data[offset] = this.colormap[cmOffset];
      data[offset + 1] = this.colormap[cmOffset + 1];
      data[offset + 2] = this.colormap[cmOffset + 2];
      data[offset + 3] = 255;
    }

    ctx.putImageData(this.imageData, 0, 0);
  }
}

/** Build a 256-entry turbo colormap (matching waveviz/colormap.rs). */
function buildTurboColormap(): Uint8Array {
  // Turbo colormap control points (simplified)
  const points: [number, number, number, number][] = [
    [0.0, 0.18, 0.01, 0.53],
    [0.07, 0.23, 0.15, 0.75],
    [0.14, 0.19, 0.35, 0.93],
    [0.21, 0.09, 0.53, 0.99],
    [0.28, 0.01, 0.68, 0.89],
    [0.35, 0.05, 0.80, 0.72],
    [0.42, 0.20, 0.88, 0.55],
    [0.50, 0.40, 0.93, 0.38],
    [0.57, 0.58, 0.95, 0.24],
    [0.64, 0.74, 0.91, 0.14],
    [0.71, 0.87, 0.82, 0.09],
    [0.78, 0.96, 0.68, 0.08],
    [0.85, 0.99, 0.51, 0.11],
    [0.92, 0.95, 0.33, 0.14],
    [1.0, 0.80, 0.16, 0.11],
  ];

  const lut = new Uint8Array(256 * 4);

  for (let i = 0; i < 256; i++) {
    const t = i / 255;

    // Find segment
    let seg = 0;
    for (let j = 1; j < points.length; j++) {
      if (t <= points[j][0]) {
        seg = j - 1;
        break;
      }
      if (j === points.length - 1) seg = j - 1;
    }

    const p0 = points[seg];
    const p1 = points[seg + 1];
    const frac = (t - p0[0]) / (p1[0] - p0[0]);

    const r = Math.round((p0[1] + (p1[1] - p0[1]) * frac) * 255);
    const g = Math.round((p0[2] + (p1[2] - p0[2]) * frac) * 255);
    const b = Math.round((p0[3] + (p1[3] - p0[3]) * frac) * 255);

    lut[i * 4] = Math.max(0, Math.min(255, r));
    lut[i * 4 + 1] = Math.max(0, Math.min(255, g));
    lut[i * 4 + 2] = Math.max(0, Math.min(255, b));
    lut[i * 4 + 3] = 255;
  }

  return lut;
}
