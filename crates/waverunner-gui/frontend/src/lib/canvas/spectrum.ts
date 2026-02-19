import type { Detection } from "../types";

export class SpectrumRenderer {
  private canvas: HTMLCanvasElement;
  private ctx: CanvasRenderingContext2D;
  private animId: number = 0;

  // Current data
  private spectrum: number[] = [];
  private peakHold: number[] = [];
  private detections: Detection[] = [];
  private noiseFloor = -100;

  // Display range
  private dbMin = -120;
  private dbMax = 0;
  private peakHoldVisible = true;

  // Layout
  private marginLeft = 50;
  private marginBottom = 24;
  private marginTop = 8;
  private marginRight = 8;

  constructor(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d")!;
  }

  setData(
    spectrum: number[],
    peakHold: number[],
    detections: Detection[],
    noiseFloor: number,
  ): void {
    this.spectrum = spectrum;
    this.peakHold = peakHold;
    this.detections = detections;
    this.noiseFloor = noiseFloor;
  }

  setRange(dbMin: number, dbMax: number): void {
    this.dbMin = dbMin;
    this.dbMax = dbMax;
  }

  setPeakHoldVisible(visible: boolean): void {
    this.peakHoldVisible = visible;
  }

  start(): void {
    const render = () => {
      this.draw();
      this.animId = requestAnimationFrame(render);
    };
    this.animId = requestAnimationFrame(render);
  }

  stop(): void {
    cancelAnimationFrame(this.animId);
  }

  resize(width: number, height: number): void {
    const dpr = window.devicePixelRatio || 1;
    this.canvas.width = width * dpr;
    this.canvas.height = height * dpr;
    this.canvas.style.width = `${width}px`;
    this.canvas.style.height = `${height}px`;
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }

  private draw(): void {
    const w = this.canvas.width / (window.devicePixelRatio || 1);
    const h = this.canvas.height / (window.devicePixelRatio || 1);
    const ctx = this.ctx;

    // Clear
    ctx.fillStyle = "#0d1117";
    ctx.fillRect(0, 0, w, h);

    const plotW = w - this.marginLeft - this.marginRight;
    const plotH = h - this.marginTop - this.marginBottom;
    const plotX = this.marginLeft;
    const plotY = this.marginTop;

    if (this.spectrum.length === 0 || plotW <= 0 || plotH <= 0) return;

    // Grid
    this.drawGrid(ctx, plotX, plotY, plotW, plotH);

    // Noise floor line
    const nfY = this.dbToY(this.noiseFloor, plotY, plotH);
    ctx.strokeStyle = "#3fb95066";
    ctx.lineWidth = 1;
    ctx.setLineDash([4, 4]);
    ctx.beginPath();
    ctx.moveTo(plotX, nfY);
    ctx.lineTo(plotX + plotW, nfY);
    ctx.stroke();
    ctx.setLineDash([]);

    // Spectrum fill
    const grad = ctx.createLinearGradient(0, plotY, 0, plotY + plotH);
    grad.addColorStop(0, "#f8514966");
    grad.addColorStop(0.3, "#d2992266");
    grad.addColorStop(0.6, "#3fb95033");
    grad.addColorStop(1, "#58a6ff11");

    ctx.beginPath();
    ctx.moveTo(plotX, plotY + plotH);
    const step = plotW / (this.spectrum.length - 1);
    for (let i = 0; i < this.spectrum.length; i++) {
      const x = plotX + i * step;
      const y = this.dbToY(this.spectrum[i], plotY, plotH);
      if (i === 0) ctx.lineTo(x, y);
      else ctx.lineTo(x, y);
    }
    ctx.lineTo(plotX + plotW, plotY + plotH);
    ctx.closePath();
    ctx.fillStyle = grad;
    ctx.fill();

    // Spectrum line
    ctx.beginPath();
    for (let i = 0; i < this.spectrum.length; i++) {
      const x = plotX + i * step;
      const y = this.dbToY(this.spectrum[i], plotY, plotH);
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    }
    ctx.strokeStyle = "#58a6ff";
    ctx.lineWidth = 1.2;
    ctx.stroke();

    // Peak hold
    if (this.peakHoldVisible && this.peakHold.length === this.spectrum.length) {
      ctx.beginPath();
      for (let i = 0; i < this.peakHold.length; i++) {
        const x = plotX + i * step;
        const y = this.dbToY(this.peakHold[i], plotY, plotH);
        if (i === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      }
      ctx.strokeStyle = "#f8514988";
      ctx.lineWidth = 0.8;
      ctx.setLineDash([3, 3]);
      ctx.stroke();
      ctx.setLineDash([]);
    }

    // Detection markers
    for (const det of this.detections) {
      if (det.bin < this.spectrum.length) {
        const x = plotX + det.bin * step;
        const y = this.dbToY(det.power_db, plotY, plotH);
        ctx.fillStyle = "#d29922";
        ctx.beginPath();
        ctx.arc(x, y, 3, 0, Math.PI * 2);
        ctx.fill();
      }
    }
  }

  private drawGrid(
    ctx: CanvasRenderingContext2D,
    x: number,
    y: number,
    w: number,
    h: number,
  ): void {
    ctx.strokeStyle = "#30363d";
    ctx.lineWidth = 0.5;
    ctx.fillStyle = "#8b949e";
    ctx.font = "10px monospace";
    ctx.textAlign = "right";

    // dB grid lines
    const dbStep = 20;
    for (
      let db = Math.ceil(this.dbMin / dbStep) * dbStep;
      db <= this.dbMax;
      db += dbStep
    ) {
      const py = this.dbToY(db, y, h);
      ctx.beginPath();
      ctx.moveTo(x, py);
      ctx.lineTo(x + w, py);
      ctx.stroke();
      ctx.fillText(`${db}`, x - 4, py + 3);
    }

    // Frequency grid lines (5 divisions)
    ctx.textAlign = "center";
    for (let i = 0; i <= 4; i++) {
      const px = x + (w * i) / 4;
      ctx.beginPath();
      ctx.moveTo(px, y);
      ctx.lineTo(px, y + h);
      ctx.stroke();
    }
  }

  private dbToY(db: number, plotY: number, plotH: number): number {
    const normalized = (db - this.dbMin) / (this.dbMax - this.dbMin);
    return plotY + plotH * (1 - Math.max(0, Math.min(1, normalized)));
  }
}
