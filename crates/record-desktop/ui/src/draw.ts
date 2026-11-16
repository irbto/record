// Canvas drawing: the dot scope, the block spectrum, pictures, and the three
// small drawings of the home screen.

const styles = getComputedStyle(document.documentElement);

/** Reads one color or font token from the style sheet. */
export function token(name: string): string {
  return styles.getPropertyValue(name).trim();
}

/** Sizes a canvas to its box at the pixel ratio of the screen and clears it. */
function fit(canvas: HTMLCanvasElement): [CanvasRenderingContext2D, number, number] {
  const ratio = window.devicePixelRatio || 1;
  const box = canvas.getBoundingClientRect();
  const width = Math.max(1, Math.round(box.width * ratio));
  const height = Math.max(1, Math.round(box.height * ratio));
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
  }
  const g = canvas.getContext("2d")!;
  g.setTransform(ratio, 0, 0, ratio, 0, 0);
  g.clearRect(0, 0, box.width, box.height);
  return [g, box.width, box.height];
}

/** The lowest and highest sample of each audio block of the last seconds. */
export class Trace {
  private low: number[] = [];
  private high: number[] = [];
  private limit = 282;

  /** Sets how many blocks six seconds hold at this sample rate. */
  setRate(sampleRate: number, blockFrames = 1024): void {
    this.limit = Math.max(16, Math.round((6 * sampleRate) / blockFrames));
  }

  push([low, high]: [number, number]): void {
    this.low.push(low);
    this.high.push(high);
    const extra = this.low.length - this.limit;
    if (extra > 0) {
      this.low.splice(0, extra);
      this.high.splice(0, extra);
    }
  }

  clear(): void {
    this.low = [];
    this.high = [];
  }

  /** Returns the loudest absolute sample of the newest blocks. */
  peak(blocks: number): number {
    let peak = 0;
    for (let index = Math.max(0, this.low.length - blocks); index < this.low.length; index++) {
      peak = Math.max(peak, -this.low[index]!, this.high[index]!);
    }
    return Math.min(1, peak);
  }

  /** Returns the extent of one screen column. The newest block is at the right. */
  column(column: number, columns: number): [number, number] {
    const perColumn = this.limit / columns;
    const end = this.low.length - Math.round((columns - 1 - column) * perColumn);
    const start = Math.round(end - Math.max(1, perColumn));
    let low = 0;
    let high = 0;
    for (let index = Math.max(0, start); index < end; index++) {
      low = Math.min(low, this.low[index]!);
      high = Math.max(high, this.high[index]!);
    }
    return [low, high];
  }
}

/** Draws six seconds of one channel on the dot grid of the terminal screen. */
export function scope(canvas: HTMLCanvasElement, trace: Trace, color: string): void {
  const [g, width, height] = fit(canvas);
  const pitch = 4;
  const dot = 2;
  const middle = Math.round(height / 2 / pitch) * pitch;
  const columns = Math.floor(width / pitch);
  const rows = Math.floor((height / 2 - 4) / pitch);
  g.fillStyle = color;
  for (let column = 0; column < columns; column++) {
    const x = column * pitch;
    g.globalAlpha = 1;
    g.fillRect(x, middle - dot, dot, dot);
    g.globalAlpha = 0.28;
    g.fillRect(x, middle + pitch - dot, dot, dot);
    const [low, high] = trace.column(column, columns);
    g.globalAlpha = 1;
    for (let row = 1; row <= Math.round(Math.min(1, high) * rows); row++) {
      g.fillRect(x, middle - dot - row * pitch, dot, dot);
    }
    for (let row = 1; row <= Math.round(Math.min(1, -low) * rows); row++) {
      g.fillRect(x, middle - dot + row * pitch, dot, dot);
    }
  }
  g.globalAlpha = 1;
}

/** Draws the bands as bars that rise in whole cells. */
export function spectrum(canvas: HTMLCanvasElement, bands: readonly number[], color: string): void {
  const [g, width, height] = fit(canvas);
  const count = Math.max(1, bands.length);
  const gap = 3;
  const cell = 9;
  const bar = Math.max(3, Math.floor((width - gap * (count - 1)) / count));
  g.fillStyle = color;
  for (let band = 0; band < count; band++) {
    const x = band * (bar + gap);
    g.globalAlpha = 0.3;
    g.fillRect(x, height - 2, bar, 2);
    const cells = Math.round(((bands[band] ?? 0) * (height - 6)) / cell);
    g.globalAlpha = 1;
    g.fillRect(x, height - cells * cell, bar, cells * cell);
  }
  g.globalAlpha = 1;
}

/** Converts a level from 0 to 1 into the text and the width of a meter. */
export function meter(bar: HTMLElement, label: HTMLElement, level: number): void {
  const decibels = level > 0.001 ? Math.max(-60, Math.round(20 * Math.log10(level))) : -60;
  label.textContent = decibels <= -60 ? "-inf dB" : `${decibels} dB`;
  bar.style.width = `${Math.max(0, Math.min(100, ((decibels + 60) / 60) * 100))}%`;
}

/** Draws a picture as large as its panel allows, without distortion. */
export function contain(
  canvas: HTMLCanvasElement,
  picture: CanvasImageSource | null,
  pictureWidth: number,
  pictureHeight: number,
): void {
  const [g, width, height] = fit(canvas);
  if (!picture || !pictureWidth || !pictureHeight) return;
  const scale = Math.min(width / pictureWidth, height / pictureHeight);
  const drawnWidth = pictureWidth * scale;
  const drawnHeight = pictureHeight * scale;
  g.imageSmoothingQuality = "high";
  g.drawImage(picture, (width - drawnWidth) / 2, (height - drawnHeight) / 2, drawnWidth, drawnHeight);
}

function noise(x: number): number {
  const value = Math.sin(x * 127.1) * 43758.5453;
  return value - Math.floor(value);
}

function smooth(x: number): number {
  const whole = Math.floor(x);
  const part = x - whole;
  const eased = part * part * (3 - 2 * part);
  return noise(whole) * (1 - eased) + noise(whole + 1) * eased;
}

/** Draws one cell grid. `paint` gives the opacity of each cell, or a color. */
function grid(
  canvas: HTMLCanvasElement,
  color: string,
  paint: (column: number, row: number, columns: number, rows: number) => number | string,
): void {
  const [g, width, height] = fit(canvas);
  const pitch = 6;
  const dot = 2;
  const columns = Math.floor(width / pitch);
  const rows = Math.floor(height / pitch);
  const left = (width - columns * pitch) / 2;
  for (let row = 0; row < rows; row++) {
    for (let column = 0; column < columns; column++) {
      const value = paint(column, row, columns, rows);
      if (value === 0) continue;
      g.fillStyle = typeof value === "string" ? value : color;
      g.globalAlpha = typeof value === "string" ? 1 : value;
      g.fillRect(left + column * pitch, row * pitch, dot, dot);
    }
  }
  g.globalAlpha = 1;
}

/** Home screen: a burst of sound on the dot grid. */
export function audioArt(canvas: HTMLCanvasElement): void {
  grid(canvas, token("--cyan"), (column, row, columns, rows) => {
    const middle = Math.floor(rows / 2);
    const position = column / columns;
    const gate = Math.max(0, smooth(position * 5.2 + 3.1) * 1.9 - 0.5);
    const reach = Math.round(Math.min(1, gate * (0.4 + 0.6 * noise(column * 1.7))) * (middle - 1));
    const distance = Math.abs(row - middle);
    if (distance === 0) return 1;
    return distance <= reach ? 1 : 0;
  });
}

/** Home screen: a monitor with the recording dot. */
export function videoArt(canvas: HTMLCanvasElement): void {
  const red = token("--red");
  grid(canvas, token("--green"), (column, row, columns, rows) => {
    const screenRows = Math.max(6, rows - 4);
    const screenColumns = Math.min(columns - 2, Math.round(screenRows * 1.9));
    const left = Math.floor((columns - screenColumns) / 2);
    const right = left + screenColumns - 1;
    const bottom = screenRows - 1;
    const inside = column >= left && column <= right && row <= bottom;
    if (inside) {
      const edge = column === left || column === right || row === 0 || row === bottom;
      if (edge) return 1;
      const fromDot = Math.hypot(column - (right - 4), row - 3);
      if (fromDot < 1.6) return red;
      // Window shapes on the recorded desktop.
      const inWindow = column > left + 2 && column < left + screenColumns * 0.55 && row > 2 && row < bottom - 2;
      return inWindow && (row === 3 || column % 2 === 0) ? 0.45 : 0.14;
    }
    const middle = Math.floor(columns / 2);
    if (row === bottom + 1 && Math.abs(column - middle) < 1) return 1;
    if (row === bottom + 2 && Math.abs(column - middle) <= 4) return 1;
    return 0;
  });
}

/** Home screen: a selected region on a dimmed screen. */
export function shotArt(canvas: HTMLCanvasElement): void {
  grid(canvas, token("--purple"), (column, row, columns, rows) => {
    const left = Math.floor(columns * 0.22);
    const right = Math.floor(columns * 0.74);
    const top = Math.floor(rows * 0.2);
    const bottom = Math.floor(rows * 0.78);
    const inside = column >= left && column <= right && row >= top && row <= bottom;
    if (!inside) return 0.14;
    const onColumnEdge = column === left || column === right;
    const onRowEdge = row === top || row === bottom;
    if (onColumnEdge && onRowEdge) return "#ffffff";
    if (onColumnEdge || onRowEdge) return 1;
    return (column + row) % 2 === 0 ? 0.5 : 0.3;
  });
}

/** Draws a stand-in desktop for the preview of the simulation. */
export function standInDesktop(g: CanvasRenderingContext2D, width: number, height: number, time: number): void {
  const unit = width / 160;
  g.fillStyle = "#111723";
  g.fillRect(0, 0, width, height);
  const pane = (x: number, y: number, w: number, h: number, tone: string) => {
    g.fillStyle = "#1b2433";
    g.fillRect(x * unit, y * unit, w * unit, h * unit);
    g.fillStyle = tone;
    g.fillRect(x * unit, y * unit, w * unit, 4 * unit);
    g.fillStyle = "#33415a";
    for (let line = 0; line < Math.floor((h - 12) / 5); line++) {
      g.fillRect((x + 4) * unit, (y + 9 + line * 5) * unit, (w - 8) * unit * (0.45 + 0.5 * noise(line * 3.3 + x)), 1.6 * unit);
    }
  };
  pane(8, 8, 86, 62, "#2d3a52");
  pane(70, 26, 80, 52, "#3a2f5a");
  g.fillStyle = "#0d1119";
  g.fillRect(0, height - 7 * unit, width, 7 * unit);
  const x = (80 + 46 * Math.sin(time * 0.7)) * unit;
  const y = (44 + 20 * Math.cos(time * 0.9)) * unit;
  g.fillStyle = "#e2e8f0";
  g.beginPath();
  g.moveTo(x, y);
  g.lineTo(x, y + 5 * unit);
  g.lineTo(x + 3.4 * unit, y + 3.6 * unit);
  g.fill();
}

/** Draws a stand-in screenshot with the marks that the editor makes. */
export function standInShot(canvas: HTMLCanvasElement, seed: number): void {
  const [g, width, height] = fit(canvas);
  const scale = Math.min(width / 16, height / 10);
  const drawnWidth = scale * 16;
  const drawnHeight = scale * 10;
  const left = (width - drawnWidth) / 2;
  const top = (height - drawnHeight) / 2;
  const unit = drawnWidth / 160;
  g.fillStyle = "#f3f4f1";
  g.fillRect(left, top, drawnWidth, drawnHeight);
  g.fillStyle = "#dfe3e8";
  g.fillRect(left, top, drawnWidth, 9 * unit);
  g.fillStyle = "#c3cad4";
  for (let line = 0; line < 12; line++) {
    g.fillRect(left + 10 * unit, top + (18 + line * 6) * unit, (60 + 70 * noise(line * 1.7 + seed * 9)) * unit, 2 * unit);
  }
  g.strokeStyle = token("--red");
  g.lineWidth = 1.4 * unit;
  g.strokeRect(left + 8 * unit, top + (27 + (seed % 3) * 12) * unit, 96 * unit, 15 * unit);
  g.fillStyle = token("--red");
  g.beginPath();
  g.arc(left + 118 * unit, top + 30 * unit, 5 * unit, 0, 7);
  g.fill();
  g.fillStyle = "#ffffff";
  g.font = `700 ${6 * unit}px ${token("--mono")}`;
  g.textAlign = "center";
  g.fillText(String((seed % 3) + 1), left + 118 * unit, top + 32.2 * unit);
}

/** A stand-in signal for the simulation: bursts of sound, different in each ear. */
export function standInSample(time: number, ear: number): number {
  const gate = Math.max(0, smooth(time * 0.6 + ear * 3.1) * 1.9 - 0.55);
  const syllable = 0.35 + 0.65 * smooth(time * 7.3 + ear * 11);
  return Math.min(1, gate * syllable * 1.25) * (noise(time * 913.7 + ear) * 2 - 1);
}

/**
 * Draws one or more waveforms over the full width with a selected range.
 * Everything outside the range is dimmed. `play` marks one position.
 * All positions run from 0 to 1.
 */
export function lanes(
  canvas: HTMLCanvasElement,
  waves: { bins: readonly (readonly [number, number])[]; color: string }[],
  start: number,
  end: number,
  play: number | null,
  ramps: { from: number; to: number; color: string; top: boolean; held: boolean }[] = [],
): void {
  const [g, width, height] = fit(canvas);
  const band = height / Math.max(1, waves.length);
  waves.forEach((wave, lane) => {
    const middle = band * lane + band / 2;
    const reach = band / 2 - 3;
    g.fillStyle = wave.color;
    g.globalAlpha = 0.35;
    g.fillRect(0, Math.round(middle), width, 1);
    g.globalAlpha = 1;
    const count = wave.bins.length;
    for (let x = 0; x < width && count; x += 2) {
      const first = Math.floor((x / width) * count);
      const last = Math.max(first + 1, Math.floor(((x + 2) / width) * count));
      let low = 0;
      let high = 0;
      for (let index = first; index < last && index < count; index++) {
        low = Math.min(low, wave.bins[index]![0]);
        high = Math.max(high, wave.bins[index]![1]);
      }
      const top = middle - Math.min(1, high) * reach;
      const bottom = middle - Math.max(-1, low) * reach;
      g.fillRect(x, top, 1.5, Math.max(1, bottom - top));
    }
  });
  const left = start * width;
  const right = end * width;
  g.fillStyle = "rgb(12 12 12 / 0.72)";
  g.fillRect(0, 0, left, height);
  g.fillRect(right, 0, width - right, height);
  g.fillStyle = token("--text");
  for (const [x, direction] of [[left, 1], [right, -1]] as const) {
    g.fillRect(Math.round(x) - 1, 0, 2, height);
    g.fillRect(direction === 1 ? Math.round(x) : Math.round(x) - 7, 0, 7, 12);
  }
  // A fade is a slope from one end of the range to its handle.
  for (const ramp of ramps) {
    const [low, high] = ramp.top ? [height / 2 - 2, 3] : [height / 2 + 2, height - 3];
    g.strokeStyle = ramp.color;
    g.fillStyle = ramp.color;
    g.lineWidth = 1.5;
    g.globalAlpha = ramp.held ? 1 : 0.55;
    g.beginPath();
    g.moveTo(ramp.from * width, low);
    g.lineTo(ramp.to * width, high);
    g.stroke();
    if (ramp.held) g.fillRect(ramp.to * width - 4, high - (ramp.top ? 0 : 8), 8, 8);
    g.globalAlpha = 1;
  }
  if (play !== null) {
    g.fillStyle = token("--red");
    g.fillRect(Math.round(play * width), 0, 1, height);
  }
}

/** Returns where `contain` draws a picture of this size on a canvas. */
export function pictureBox(canvas: HTMLCanvasElement, pictureWidth: number, pictureHeight: number): { x: number; y: number; scale: number } {
  const box = canvas.getBoundingClientRect();
  const scale = Math.min(box.width / Math.max(1, pictureWidth), box.height / Math.max(1, pictureHeight));
  return { x: (box.width - pictureWidth * scale) / 2, y: (box.height - pictureHeight * scale) / 2, scale };
}

/** Dims the margins that a crop removes from the picture drawn by `contain`. */
export function cropShade(
  canvas: HTMLCanvasElement,
  pictureWidth: number,
  pictureHeight: number,
  crop: { left: number; right: number; top: number; bottom: number },
  handles = false,
): void {
  if (!pictureWidth || !pictureHeight) return;
  const box = canvas.getBoundingClientRect();
  const g = canvas.getContext("2d")!;
  const scale = Math.min(box.width / pictureWidth, box.height / pictureHeight);
  const width = pictureWidth * scale;
  const height = pictureHeight * scale;
  const x = (box.width - width) / 2;
  const y = (box.height - height) / 2;
  const [left, right, top, bottom] = [crop.left * scale, crop.right * scale, crop.top * scale, crop.bottom * scale];
  g.fillStyle = "rgb(12 12 12 / 0.72)";
  g.fillRect(x, y, left, height);
  g.fillRect(x + width - right, y, right, height);
  g.fillRect(x + left, y, Math.max(0, width - left - right), top);
  g.fillRect(x + left, y + height - bottom, Math.max(0, width - left - right), bottom);
  const [boxWidth, boxHeight] = [Math.max(0, width - left - right), Math.max(0, height - top - bottom)];
  if (left + right + top + bottom > 0 || handles) {
    g.strokeStyle = token("--text");
    g.lineWidth = 1;
    g.strokeRect(x + left + 0.5, y + top + 0.5, Math.max(0, boxWidth - 1), Math.max(0, boxHeight - 1));
  }
  if (!handles) return;
  // Eight handles: the corners and the middle of each edge.
  g.fillStyle = token("--text");
  for (const column of [0, 0.5, 1]) {
    for (const line of [0, 0.5, 1]) {
      if (column === 0.5 && line === 0.5) continue;
      g.fillRect(x + left + boxWidth * column - 4, y + top + boxHeight * line - 4, 8, 8);
    }
  }
}
