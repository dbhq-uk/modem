// The waterfall canvas renderer - shared by page.js (the /demo route's
// live cross-wired call) and explained.js (the overture's own playback),
// which is why this lives in its own module rather than either file:
// before this split, page.js carried the only copy and explained.js
// would otherwise have needed a second one that could drift from it.
//
// Frequency up the axis (300-3400 Hz, the whole telephone band), time
// across, newest column at the right - same layout modem-tui's own
// renderer uses, so a visitor recognises this when they later run the
// binary (see brand/README.md and the design spec's "Components" table).
//
// Phosphor decay is read from the brand token, never a fresh guess here -
// the same rule modem-tui/src/waterfall.rs follows for its own decay, and
// for the same reason: it is the strongest CRT cue there is, and the two
// surfaces have to agree on it or they read as different products. Each
// frame the whole canvas is faded by `1 - exp(-dt / decaySeconds)` before
// the new column is drawn, which is the continuous-time form of the same
// exponential the TUI applies per scrolled column.
export class Waterfall {
  constructor(canvasEl) {
    this.ctx = canvasEl.getContext('2d');
    this.width = canvasEl.width;
    this.height = canvasEl.height;
    const decayCss = getComputedStyle(document.documentElement).getPropertyValue('--modem-phosphor-decay').trim();
    this.decaySeconds = parseFloat(decayCss) || 2.56;
    const groundCss = getComputedStyle(document.documentElement).getPropertyValue('--modem-ground').trim();
    const greenCss = getComputedStyle(document.documentElement).getPropertyValue('--modem-green-bright').trim();
    this.ground = hexToRgb(groundCss);
    this.bright = hexToRgb(greenCss);
    this.lastFrame = null;
    this.floorDb = -70;
    this.ctx.fillStyle = groundCss;
    this.ctx.fillRect(0, 0, this.width, this.height);
  }

  frame(analyser, sampleRate) {
    const now = performance.now();
    const dt = this.lastFrame === null ? 1 / 60 : (now - this.lastFrame) / 1000;
    this.lastFrame = now;

    const fadeAlpha = 1 - Math.exp(-dt / this.decaySeconds);
    this.ctx.fillStyle = `rgba(${this.ground.r}, ${this.ground.g}, ${this.ground.b}, ${fadeAlpha})`;
    this.ctx.fillRect(0, 0, this.width, this.height);

    // Shift everything one column left, then draw the new column at the
    // right edge - the scrolling half of the effect.
    this.ctx.drawImage(this.ctx.canvas, -1, 0);

    const bins = new Float32Array(analyser.frequencyBinCount);
    analyser.getFloatFrequencyData(bins);
    const hzPerBin = sampleRate / analyser.fftSize;

    for (let y = 0; y < this.height; y++) {
      const freq = 3400 - (y / this.height) * (3400 - 300);
      const bin = Math.min(bins.length - 1, Math.max(0, Math.round(freq / hzPerBin)));
      const db = bins[bin];
      const t = Math.min(1, Math.max(0, (db - this.floorDb) / (0 - this.floorDb)));
      const r = lerp(this.ground.r, this.bright.r, t);
      const g = lerp(this.ground.g, this.bright.g, t);
      const b = lerp(this.ground.b, this.bright.b, t);
      this.ctx.fillStyle = `rgb(${r}, ${g}, ${b})`;
      this.ctx.fillRect(this.width - 1, y, 1, 1);
    }
  }

  reset() {
    this.lastFrame = null;
    this.ctx.fillStyle = `rgb(${this.ground.r}, ${this.ground.g}, ${this.ground.b})`;
    this.ctx.fillRect(0, 0, this.width, this.height);
  }
}

function hexToRgb(hex) {
  const h = hex.replace('#', '');
  return { r: parseInt(h.slice(0, 2), 16), g: parseInt(h.slice(2, 4), 16), b: parseInt(h.slice(4, 6), 16) };
}

function lerp(a, b, t) {
  return Math.round(a + (b - a) * t);
}
