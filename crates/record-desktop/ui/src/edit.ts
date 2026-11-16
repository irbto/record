// The two editors: trim one audio clip, and trim, fade, and crop one video.

import * as draw from "./draw";
import { commandFor, comboOf, keysOf, type Command } from "./keys";
import type { Bridge, ClipShape, EditUpdate, Save, VideoInfo } from "./types";

type Child = Node | string | null | false;

/** What the editors need from the rest of the interface. */
export interface Host {
  bridge: Bridge;
  h<K extends keyof HTMLElementTagNameMap>(tag: K, props?: Partial<HTMLElementTagNameMap[K]> & { dataset?: Record<string, string> }, ...children: Child[]): HTMLElementTagNameMap[K];
  say(text: string, tone?: "" | "good" | "bad"): void;
  fail(error: unknown): void;
  openDialog(title: string, build: (body: HTMLElement) => void, submit?: (() => void) | null): void;
  closeDialog(): void;
  render(): void;
  /** Called when a clip changed its length. */
  clipTrimmed(path: string, seconds: number): void;
  /** Called when a video was written again. */
  videoSaved(path: string): void;
}

let host: Host;
/** Runs when the clip that plays reaches its end. */
let played: (() => void) | null = null;
const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

/** Formats seconds as minutes, seconds, and tenths. */
function stamp(seconds: number): string {
  const safe = Math.max(0, seconds);
  const minutes = Math.floor(safe / 60);
  return `${String(minutes).padStart(2, "0")}:${(safe - minutes * 60).toFixed(1).padStart(4, "0")}`;
}

/**
 * Lets the pointer move the two ends of a range on a canvas. A press near an
 * end moves that end. A press anywhere else reports a position.
 */
function dragRange(
  canvas: HTMLCanvasElement,
  range: () => [number, number],
  move: (end: 0 | 1, position: number) => void,
  point: (position: number) => void,
): void {
  let held: 0 | 1 | null = null;
  const at = (event: PointerEvent) => {
    const box = canvas.getBoundingClientRect();
    return Math.min(1, Math.max(0, (event.clientX - box.left) / Math.max(1, box.width)));
  };
  const act = (event: PointerEvent) => (held === null ? point(at(event)) : move(held, at(event)));
  canvas.addEventListener("pointerdown", (event) => {
    const box = canvas.getBoundingClientRect();
    const position = at(event);
    const [start, end] = range();
    const near = (edge: number) => Math.abs(position - edge) * box.width <= 9;
    // The end wins when both are close, so a short range can still grow.
    held = near(end) ? 1 : near(start) ? 0 : null;
    canvas.setPointerCapture(event.pointerId);
    canvas.dataset.down = "1";
    act(event);
  });
  canvas.addEventListener("pointermove", (event) => {
    if (canvas.dataset.down) act(event);
  });
  const release = () => {
    delete canvas.dataset.down;
    held = null;
  };
  canvas.addEventListener("pointerup", release);
  canvas.addEventListener("pointercancel", release);
}

// ---- Audio clip. ----

/** Opens the trim dialog of one clip of the running audio session. */
export async function openClipEditor(save: Save): Promise<void> {
  const { bridge, h } = host;
  let shape: ClipShape;
  try {
    shape = await bridge.clipWaveform(save.path);
  } catch (error) {
    host.fail(error);
    return;
  }
  let start = 0;
  let end = 1;
  let playing = false;
  const canvas = h("canvas", { className: "wave" });
  const range = h("p", { className: "hint" });
  const problem = h("p", { className: "problem" });
  const play = h("button", { className: "button", type: "button", textContent: "Play" });
  const redraw = () => {
    draw.lanes(canvas, [{ bins: shape.left, color: draw.token("--cyan") }, { bins: shape.right, color: draw.token("--green") }], start, end, null);
    range.textContent = `${stamp(start * shape.seconds)} to ${stamp(end * shape.seconds)} · ${stamp((end - start) * shape.seconds)}`;
    play.textContent = playing ? "Stop" : "Play";
  };
  const smallest = Math.min(1, 0.05 / Math.max(0.05, shape.seconds));
  dragRange(
    canvas,
    () => [start, end],
    (which, position) => {
      if (which === 0) start = Math.min(position, end - smallest);
      else end = Math.max(position, start + smallest);
      redraw();
    },
    () => undefined,
  );
  play.addEventListener("click", () => {
    const request = playing ? bridge.stopClip() : bridge.playClip(save.path, start * shape.seconds, end * shape.seconds);
    playing = !playing;
    redraw();
    request.catch((error: unknown) => {
      playing = false;
      problem.textContent = String(error);
      redraw();
    });
  });
  played = () => {
    playing = false;
    if (canvas.isConnected) redraw();
  };
  const reset = h("button", { className: "button", type: "button", textContent: "Reset" });
  reset.addEventListener("click", () => {
    start = 0;
    end = 1;
    redraw();
  });
  const close = h("button", { className: "button", type: "button", textContent: "Cancel" });
  close.addEventListener("click", () => {
    void bridge.stopClip();
    host.closeDialog();
  });
  const trim = async () => {
    if (start === 0 && end === 1) {
      host.closeDialog();
      return;
    }
    try {
      const seconds = await bridge.trimAudioClip(save.path, start * shape.seconds, end * shape.seconds);
      host.clipTrimmed(save.path, seconds);
      host.closeDialog();
      host.say(`Trimmed ${save.name}`, "good");
    } catch (error) {
      problem.textContent = String(error);
    }
  };
  const saveButton = h("button", { className: "button save", type: "submit", textContent: "Save" });
  host.openDialog(
    save.name,
    (body) => {
      body.append(canvas, range, problem, h("div", { className: "buttons" }, play, reset, close, saveButton));
    },
    () => void trim(),
  );
  redraw();
}

// ---- Video. ----

type Tool = "range" | "videoFade" | "audioFade" | "crop";
const icon = (path: string) => `<svg viewBox="0 0 16 16" aria-hidden="true"><path d="${path}"/></svg>`;
/** Each tool with its name, its key, and its picture. */
const tools: { tool: Tool; label: string; command: Command; icon: string }[] = [
  { tool: "range", label: "Range", command: "toolRange", icon: icon("M5 3H3v10h2M11 3h2v10h-2M6 8h4") },
  { tool: "videoFade", label: "Video fade", command: "toolVideoFade", icon: icon("M2 3h12v10H2zM3 12l10-8") },
  { tool: "audioFade", label: "Audio fade", command: "toolAudioFade", icon: icon("M2 9v-1M5 10V7M8 11V6M11 12V5M14 13V4") },
  { tool: "crop", label: "Crop", command: "toolCrop", icon: icon("M4 1v11h11M1 4h11v11") },
];

/** A level that is constant, or that follows keyframes. Values run from 0 to 1. */
interface Level {
  base: number;
  keys: { frame: number; value: number }[];
}
type LevelName = "opacity" | "volume";
const levelNames: [LevelName, string][] = [["opacity", "Opacity (%)"], ["volume", "Volume (%)"]];

function levelAt(level: Level, frame: number): number {
  const { keys } = level;
  if (!keys.length) return level.base;
  const after = keys.findIndex((key) => key.frame > frame);
  if (after < 0) return keys[keys.length - 1]!.value;
  if (after === 0) return keys[0]!.value;
  const [previous, next] = [keys[after - 1]!, keys[after]!];
  return previous.value + ((next.value - previous.value) * (frame - previous.frame)) / (next.frame - previous.frame);
}

/** Sets the level at one frame: a keyframe when the level has keyframes. */
function setLevel(level: Level, frame: number, value: number, force = false): void {
  if (!level.keys.length && !force) {
    level.base = value;
    return;
  }
  const existing = level.keys.find((key) => key.frame === frame);
  if (existing) existing.value = value;
  else level.keys.push({ frame, value });
  level.keys.sort((first, second) => first.frame - second.frame);
}

/** The keyframes of the kept range, in seconds from its start. */
function exported(level: Level, start: number, end: number, fps: number): [number, number][] {
  if (!level.keys.length) return level.base < 1 ? [[0, level.base]] : [];
  const inside = level.keys.filter((key) => key.frame > start && key.frame < end - 1);
  const frames = [start, ...inside.map((key) => key.frame), end - 1];
  return frames.map((frame) => [(frame - start) / fps, levelAt(level, frame)]);
}

interface VideoState {
  info: VideoInfo;
  /** What the mouse changes: on the timeline, or on the picture for a crop. */
  tool: Tool;
  start: number;
  end: number;
  playhead: number;
  fades: { videoIn: number; videoOut: number; audioIn: number; audioOut: number };
  crop: { left: number; right: number; top: number; bottom: number };
  levels: Record<LevelName, Level>;
  bins: [number, number][];
  picture: ImageBitmap | null;
  wanted: number | null;
  loading: boolean;
  exporting: string;
}

let video: VideoState | null = null;

/** Reports whether a video is open in the editor of the screen capture screen. */
export function videoEditorOpen(): boolean {
  return video !== null;
}

/** The video that is open in the editor. */
export function videoEditorInfo(): VideoInfo | null {
  return video?.info ?? null;
}

const shown = () => $("video").classList.contains("on");

function fresh(info: VideoInfo): VideoState {
  return {
    info,
    tool: "range",
    start: 0,
    end: info.frames,
    playhead: 0,
    fades: { videoIn: 0, videoOut: 0, audioIn: 0, audioOut: 0 },
    crop: { left: 0, right: 0, top: 0, bottom: 0 },
    levels: { opacity: { base: 1, keys: [] }, volume: { base: 1, keys: [] } },
    bins: [],
    picture: null,
    wanted: 0,
    loading: false,
    exporting: "",
  };
}

/** Asks for the picture at the playhead. One request runs at a time. */
async function loadPicture(): Promise<void> {
  const state = video;
  if (!state || state.loading || state.wanted === null) return;
  state.loading = true;
  const frame = state.wanted;
  state.wanted = null;
  try {
    const picture = await createImageBitmap(await host.bridge.editorFrame(frame / state.info.fps));
    if (video === state) {
      state.picture?.close();
      state.picture = picture;
      $("editor-empty").hidden = true;
    } else picture.close();
  } catch (error) {
    if (video === state && !state.exporting) {
      $("editor-empty").textContent = String(error);
      $("editor-empty").hidden = false;
    }
  }
  state.loading = false;
  paintVideo();
  if (video === state && state.wanted !== null) void loadPicture();
}

function seek(frame: number): void {
  if (!video) return;
  video.playhead = Math.min(video.info.frames - 1, Math.max(0, Math.round(frame)));
  video.wanted = video.playhead;
  void loadPicture();
  paintVideo();
}

function paintVideo(): void {
  const state = video;
  if (!state || !shown()) return;
  const canvas = $<HTMLCanvasElement>("editor-picture");
  draw.contain(canvas, state.picture, state.picture?.width ?? 0, state.picture?.height ?? 0);
  // The picture is smaller than the video, so the crop scales with it.
  const scale = state.picture ? state.picture.width / state.info.width : 0;
  draw.cropShade(
    canvas,
    state.picture?.width ?? 0,
    state.picture?.height ?? 0,
    { left: state.crop.left * scale, right: state.crop.right * scale, top: state.crop.top * scale, bottom: state.crop.bottom * scale },
    state.tool === "crop",
  );
  canvas.style.cursor = state.tool === "crop" ? "move" : "";
  const frames = Math.max(1, state.info.frames);
  const [first, last] = [state.start / frames, state.end / frames];
  const fps = state.info.fps;
  const { fades, levels } = state;
  // The picture shows the fade and the opacity at the playhead.
  const since = (state.playhead - state.start) / fps;
  const until = (state.end - 1 - state.playhead) / fps;
  const faded = Math.min(fades.videoIn ? since / fades.videoIn : 1, fades.videoOut ? until / fades.videoOut : 1, 1);
  draw.dim(canvas, state.picture?.width ?? 0, state.picture?.height ?? 0, Math.max(0, faded) * levelAt(levels.opacity, state.playhead));
  const points = (level: Level): [number, number][] => level.keys.map((key) => [key.frame / frames, key.value]);
  const ramp = (seconds: number, rising: boolean, color: string, top: boolean, tool: Tool) => ({
    from: rising ? first : last,
    to: rising ? first + (seconds * fps) / frames : last - (seconds * fps) / frames,
    color,
    top,
    held: state.tool === tool,
  });
  draw.lanes($<HTMLCanvasElement>("editor-timeline"), [{ bins: state.bins, color: draw.token("--purple") }], first, last, state.playhead / frames, [
    ramp(fades.videoIn, true, draw.token("--green"), true, "videoFade"),
    ramp(fades.videoOut, false, draw.token("--green"), true, "videoFade"),
    ramp(fades.audioIn, true, draw.token("--amber"), false, "audioFade"),
    ramp(fades.audioOut, false, draw.token("--amber"), false, "audioFade"),
  ], [
    { points: points(levels.opacity), color: draw.token("--green"), top: true },
    { points: points(levels.volume), color: draw.token("--amber"), top: false },
  ]);
  syncFields();
  $("editor-range").textContent = `Range · ${stamp(state.start / fps)} to ${stamp(state.end / fps)} · ${stamp((state.end - state.start) / fps)}`;
}

/** The fields of the side panel and the value each one shows. */
function fieldValues(state: VideoState): [string, number][] {
  const { fades, crop } = state;
  return [
    ["Video fade in (s)", fades.videoIn],
    ["Video fade out (s)", fades.videoOut],
    ["Audio fade in (s)", fades.audioIn],
    ["Audio fade out (s)", fades.audioOut],
    ["Crop left (px)", crop.left],
    ["Crop right (px)", crop.right],
    ["Crop top (px)", crop.top],
    ["Crop bottom (px)", crop.bottom],
  ];
}

const fieldId = (label: string) => `edit-${label.toLowerCase().replace(/[^a-z]+/g, "-")}`;

/** Shows in the fields what the mouse changed. */
function syncFields(): void {
  if (!video) return;
  for (const [label, value] of fieldValues(video)) {
    const input = document.getElementById(fieldId(label)) as HTMLInputElement | null;
    const text = String(Number(value.toFixed(2)));
    if (input && document.activeElement !== input && input.value !== text) input.value = text;
  }
  for (const [name] of levelNames) {
    const level = video.levels[name];
    const input = document.getElementById(`edit-${name}`) as HTMLInputElement | null;
    const text = String(Math.round(levelAt(level, video.playhead) * 100));
    if (input && document.activeElement !== input && input.value !== text) input.value = text;
    const here = level.keys.some((key) => key.frame === video!.playhead);
    document.getElementById(`key-${name}`)?.classList.toggle("on", here);
    document.getElementById(`key-${name}`)?.classList.toggle("some", level.keys.length > 0);
  }
}

function renderVideoEditor(): void {
  const state = video;
  if (!state) return;
  const { h } = host;
  const { info } = state;
  const busy = Boolean(state.exporting);
  const button = (id: string, className: string, label: string, enabled: boolean, action: () => void) => {
    const element = h("button", { className: `button ${className}`, type: "button", textContent: label, disabled: !enabled, dataset: { id } });
    element.addEventListener("click", action);
    return element;
  };
  // The tool list: a button with the picture of the tool, and one row for each tool.
  const current = tools.find((one) => one.tool === state.tool) ?? tools[0]!;
  const face = (one: (typeof tools)[number], key: boolean) => {
    const picture = h("span", { className: "tool-icon" });
    picture.innerHTML = one.icon;
    return [picture, h("span", { textContent: one.label }), h("kbd", { textContent: key ? keysOf(one.command) : "▾" })];
  };
  const toolButton = h("button", { type: "button", id: "editor-tool", disabled: busy, title: "Tool" }, ...face(current, false));
  const toolList = h("div", { className: "items" });
  const tool = h("div", { className: "menu tool" }, toolButton, toolList);
  toolButton.addEventListener("click", () => tool.classList.toggle("open"));
  for (const one of tools) {
    const item = h("button", { type: "button", className: "item", dataset: { tool: one.tool } }, ...face(one, true));
    item.addEventListener("click", () => {
      state.tool = one.tool;
      renderVideoEditor();
    });
    toolList.append(item);
  }
  $("editor-bar").replaceChildren(
    tool,
    h("span", { className: "facts", id: "editor-facts", textContent: state.exporting }),
  );
  $("editor-actions").replaceChildren(
    button("edit-save", "save", "Save", !busy, () => void exportVideo()),
    button("edit-reset", "", "Reset", !busy, () => {
      const bins = state.bins;
      const picture = state.picture;
      video = { ...fresh(info), tool: state.tool, playhead: state.playhead, bins, picture, wanted: null };
      renderVideoEditor();
    }),
    button("edit-close", "", "Close", !busy, () => void closeVideoEditor()),
  );
  const fields = $("editor-fields");
  fields.replaceChildren();
  const field = (label: string, value: number, limit: number, step: number, set: (value: number) => void) => {
    const input = h("input", { type: "text", value: String(value), spellcheck: false, disabled: busy, id: fieldId(label) });
    input.addEventListener("change", () => {
      const typed = Number(input.value);
      const safe = Number.isFinite(typed) ? Math.min(limit, Math.max(0, Math.round(typed / step) * step)) : 0;
      input.value = String(Number(safe.toFixed(2)));
      set(safe);
      paintVideo();
    });
    fields.append(h("div", { className: "field" }, h("label", { htmlFor: input.id, textContent: label }), input));
  };
  fields.append(h("div", { className: "field info" }, h("span", { textContent: "Clip" }), h("span", { textContent: `${info.width}x${info.height} · ${info.fps} fps · ${stamp(info.frames / info.fps)}` })));
  // A level has a value and three keyframe buttons: previous, set, next.
  for (const [name, label] of levelNames) {
    const level = state.levels[name];
    const input = h("input", { type: "text", spellcheck: false, disabled: busy, id: `edit-${name}`, value: String(Math.round(levelAt(level, state.playhead) * 100)) });
    input.addEventListener("change", () => {
      const typed = Number(input.value);
      setLevel(level, state.playhead, Number.isFinite(typed) ? Math.min(100, Math.max(0, typed)) / 100 : 1);
      paintVideo();
    });
    const jump = (direction: 1 | -1) => () => {
      const frames = level.keys.map((key) => key.frame).filter((frame) => (direction === 1 ? frame > state.playhead : frame < state.playhead));
      const target = direction === 1 ? frames[0] : frames[frames.length - 1];
      if (target !== undefined) seek(target);
    };
    const key = (id: string, text: string, title: string, action: () => void) => {
      const element = h("button", { type: "button", className: "key", id, textContent: text, title, disabled: busy });
      element.addEventListener("click", action);
      return element;
    };
    fields.append(
      h(
        "div",
        { className: "field level" },
        h("label", { htmlFor: input.id, textContent: label }),
        input,
        key(`prev-${name}`, "‹", "Previous keyframe", jump(-1)),
        key(`key-${name}`, "◆", "Keyframe at the playhead", () => {
          const at = level.keys.findIndex((one) => one.frame === state.playhead);
          if (at >= 0) {
            // The last keyframe leaves its value as the constant level.
            if (level.keys.length === 1) level.base = level.keys[0]!.value;
            level.keys.splice(at, 1);
          } else setLevel(level, state.playhead, levelAt(level, state.playhead), true);
          paintVideo();
        }),
        key(`next-${name}`, "›", "Next keyframe", jump(1)),
      ),
    );
  }
  const seconds = (state.end - state.start) / info.fps;
  const { fades } = state;
  field("Video fade in (s)", fades.videoIn, seconds, 0.1, (value) => (fades.videoIn = value));
  field("Video fade out (s)", fades.videoOut, seconds, 0.1, (value) => (fades.videoOut = value));
  field("Audio fade in (s)", fades.audioIn, seconds, 0.1, (value) => (fades.audioIn = value));
  field("Audio fade out (s)", fades.audioOut, seconds, 0.1, (value) => (fades.audioOut = value));
  // The encoder takes even numbers of pixels.
  field("Crop left (px)", state.crop.left, info.width - 16, 2, (value) => (state.crop.left = value));
  field("Crop right (px)", state.crop.right, info.width - 16, 2, (value) => (state.crop.right = value));
  field("Crop top (px)", state.crop.top, info.height - 16, 2, (value) => (state.crop.top = value));
  field("Crop bottom (px)", state.crop.bottom, info.height - 16, 2, (value) => (state.crop.bottom = value));
  paintVideo();
}

async function exportVideo(): Promise<void> {
  const state = video;
  if (!state || state.exporting) return;
  const { fades, crop } = state;
  state.exporting = "Preparing";
  renderVideoEditor();
  try {
    await host.bridge.exportVideo({
      startFrame: state.start,
      endFrame: state.end,
      videoFadeIn: fades.videoIn,
      videoFadeOut: fades.videoOut,
      audioFadeIn: fades.audioIn,
      audioFadeOut: fades.audioOut,
      cropLeft: crop.left,
      cropRight: crop.right,
      cropTop: crop.top,
      cropBottom: crop.bottom,
      opacity: exported(state.levels.opacity, state.start, state.end, state.info.fps),
      volume: exported(state.levels.volume, state.start, state.end, state.info.fps),
    });
  } catch (error) {
    state.exporting = "";
    renderVideoEditor();
    host.fail(error);
  }
}

function onEdit(update: EditUpdate): void {
  const state = video;
  if (!state) return;
  if (update.kind === "stage") state.exporting = update.name;
  else if (update.kind === "progress") state.exporting = `${Math.round((update.done / Math.max(1, update.total)) * 100)}%`;
  else {
    if (update.error) host.say(`Edit failed: ${update.error}`, "bad");
    else host.say(`Saved ${state.info.name}`, "good");
    if (update.info) {
      state.picture?.close();
      video = fresh(update.info);
      void loadWaveform();
      void loadPicture();
    } else video = null;
    host.videoSaved(state.info.path);
    host.render();
    return;
  }
  renderVideoEditor();
}

async function loadWaveform(): Promise<void> {
  const state = video;
  if (!state) return;
  try {
    const bins = await host.bridge.editorWaveform();
    if (video === state) {
      state.bins = bins;
      paintVideo();
    }
  } catch {
    // A video without readable audio has a flat line.
  }
}

/**
 * Lets the pointer change the crop on the picture: a corner or an edge moves
 * that side, and a press inside the frame moves the whole frame.
 */
function setupCrop(canvas: HTMLCanvasElement): void {
  type Sides = { left: boolean; right: boolean; top: boolean; bottom: boolean };
  let held: { sides: Sides; x: number; y: number; crop: VideoState["crop"] } | null = null;
  const place = (event: PointerEvent, state: VideoState) => {
    const box = canvas.getBoundingClientRect();
    const picture = state.picture;
    const drawn = draw.pictureBox(canvas, picture?.width ?? 1, picture?.height ?? 1);
    // One pixel of the video on the screen.
    const scale = (drawn.scale * (picture?.width ?? 1)) / state.info.width;
    return { x: (event.clientX - box.left - drawn.x) / scale, y: (event.clientY - box.top - drawn.y) / scale, reach: 10 / scale };
  };
  canvas.addEventListener("pointerdown", (event) => {
    const state = video;
    if (!state || state.tool !== "crop" || state.exporting || !state.picture) return;
    const { x, y, reach } = place(event, state);
    const { crop, info } = state;
    const near = (value: number, edge: number) => Math.abs(value - edge) <= reach;
    const sides = { left: near(x, crop.left), right: near(x, info.width - crop.right), top: near(y, crop.top), bottom: near(y, info.height - crop.bottom) };
    const inside = x > crop.left - reach && x < info.width - crop.right + reach && y > crop.top - reach && y < info.height - crop.bottom + reach;
    if (!inside) return;
    held = { sides, x, y, crop: { ...crop } };
    canvas.setPointerCapture(event.pointerId);
  });
  canvas.addEventListener("pointermove", (event) => {
    const state = video;
    if (!held || !state) return;
    const { x, y } = place(event, state);
    const [dx, dy] = [x - held.x, y - held.y];
    const { info } = state;
    const from = held.crop;
    const even = (value: number) => Math.round(value / 2) * 2;
    const limit = (value: number, most: number) => even(Math.min(most, Math.max(0, value)));
    const { sides } = held;
    const next = { ...from };
    if (!sides.left && !sides.right && !sides.top && !sides.bottom) {
      // Move the frame: one margin grows as much as the other shrinks.
      const shiftX = even(Math.min(from.right, Math.max(-from.left, dx)));
      const shiftY = even(Math.min(from.bottom, Math.max(-from.top, dy)));
      Object.assign(next, { left: from.left + shiftX, right: from.right - shiftX, top: from.top + shiftY, bottom: from.bottom - shiftY });
    } else {
      // The picture keeps at least 16 pixels in each direction.
      if (sides.left) next.left = limit(from.left + dx, info.width - from.right - 16);
      else if (sides.right) next.right = limit(from.right - dx, info.width - from.left - 16);
      if (sides.top) next.top = limit(from.top + dy, info.height - from.bottom - 16);
      else if (sides.bottom) next.bottom = limit(from.bottom - dy, info.height - from.top - 16);
    }
    state.crop = next;
    paintVideo();
  });
  const release = () => {
    held = null;
  };
  canvas.addEventListener("pointerup", release);
  canvas.addEventListener("pointercancel", release);
}

/** Opens one saved video in the editor. */
export async function openVideoEditor(path: string): Promise<void> {
  try {
    const info = await host.bridge.openVideoEditor(path);
    video?.picture?.close();
    video = fresh(info);
    host.render();
    void loadWaveform();
    void loadPicture();
  } catch (error) {
    host.fail(error);
  }
}

export async function closeVideoEditor(): Promise<void> {
  if (!video || video.exporting) return;
  video.picture?.close();
  video = null;
  // An empty timeline and an empty viewer stay behind.
  for (const id of ["editor-timeline", "editor-picture"]) {
    const canvas = $<HTMLCanvasElement>(id);
    canvas.getContext("2d")?.clearRect(0, 0, canvas.width, canvas.height);
  }
  $("editor-empty").hidden = true;
  host.render();
  await host.bridge.closeVideoEditor().catch(host.fail);
}

/** Draws the editor. The screen capture screen calls this while it is open. */
export function showVideoEditor(): void {
  renderVideoEditor();
}

export function setupEditors(context: Host): void {
  host = context;
  host.bridge.on("edit", onEdit);
  host.bridge.on("played", () => played?.());
  // On the timeline the tool selects what the two handles are: the ends of
  // the range, or the ends of the two fades.
  const fadeOf = (state: VideoState): [keyof VideoState["fades"], keyof VideoState["fades"]] | null =>
    state.tool === "videoFade" ? ["videoIn", "videoOut"] : state.tool === "audioFade" ? ["audioIn", "audioOut"] : null;
  dragRange(
    $<HTMLCanvasElement>("editor-timeline"),
    () => {
      const state = video;
      if (!state) return [0, 1];
      const frames = state.info.frames;
      const fade = fadeOf(state);
      if (!fade) return [state.start / frames, state.end / frames];
      const fps = state.info.fps;
      return [(state.start + state.fades[fade[0]] * fps) / frames, (state.end - state.fades[fade[1]] * fps) / frames];
    },
    (which, position) => {
      const state = video;
      if (!state || state.exporting) return;
      const frame = Math.round(position * state.info.frames);
      const fade = fadeOf(state);
      if (fade) {
        // The two fades of one kind share the length of the range.
        const fps = state.info.fps;
        const length = (state.end - state.start) / fps;
        const other = state.fades[fade[1 - which]!];
        const wanted = which === 0 ? (frame - state.start) / fps : (state.end - frame) / fps;
        state.fades[fade[which]] = Math.round(Math.min(length - other, Math.max(0, wanted)) * 10) / 10;
        paintVideo();
        return;
      }
      if (which === 0) state.start = Math.max(0, Math.min(frame, state.end - 1));
      else state.end = Math.min(state.info.frames, Math.max(frame, state.start + 1));
      // The picture follows the end that moves.
      seek(which === 0 ? state.start : state.end - 1);
    },
    (position) => {
      if (video && !video.exporting) seek(position * video.info.frames);
    },
  );
  setupCrop($<HTMLCanvasElement>("editor-picture"));
  document.addEventListener("pointerdown", (event) => {
    const list = document.querySelector(".menu.tool.open");
    if (list && !list.contains(event.target as Node)) list.classList.remove("open");
  });
  window.addEventListener("resize", paintVideo);
  document.addEventListener("keydown", (event) => {
    const state = video;
    const typing = event.target instanceof HTMLInputElement;
    if (!state || typing || state.exporting || !shown() || document.querySelector("dialog[open], .menu.open, .popover:not([hidden])")) return;
    // Shift makes a frame step one second long.
    const command = commandFor(comboOf(event)) ?? (event.shiftKey ? commandFor(comboOf(event, false)) : null);
    const step = event.shiftKey ? state.info.fps : 1;
    const picked = tools.find((one) => one.command === command);
    if (picked) {
      state.tool = picked.tool;
      renderVideoEditor();
    } else if (command === "frameBack") seek(state.playhead - step);
    else if (command === "frameForward") seek(state.playhead + step);
    else if (event.key === "Escape") void closeVideoEditor();
    else return;
    event.preventDefault();
  });
}
