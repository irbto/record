// The two editors: trim one audio clip, and trim, fade, and crop one video.

import * as draw from "./draw";
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
  videoSaved(): void;
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

interface VideoState {
  info: VideoInfo;
  start: number;
  end: number;
  playhead: number;
  fades: { videoIn: number; videoOut: number; audioIn: number; audioOut: number };
  crop: { left: number; right: number; top: number; bottom: number };
  bins: [number, number][];
  picture: ImageBitmap | null;
  wanted: number | null;
  loading: boolean;
  exporting: string;
}

let video: VideoState | null = null;

/** Reports whether the video editor covers the screen capture screen. */
export function videoEditorOpen(): boolean {
  return video !== null;
}

function fresh(info: VideoInfo): VideoState {
  return {
    info,
    start: 0,
    end: info.frames,
    playhead: 0,
    fades: { videoIn: 0, videoOut: 0, audioIn: 0, audioOut: 0 },
    crop: { left: 0, right: 0, top: 0, bottom: 0 },
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
  if (!state || !$("editor").classList.contains("on")) return;
  const canvas = $<HTMLCanvasElement>("editor-picture");
  draw.contain(canvas, state.picture, state.picture?.width ?? 0, state.picture?.height ?? 0);
  // The picture is smaller than the video, so the crop scales with it.
  const scale = state.picture ? state.picture.width / state.info.width : 0;
  draw.cropShade(canvas, state.picture?.width ?? 0, state.picture?.height ?? 0, {
    left: state.crop.left * scale,
    right: state.crop.right * scale,
    top: state.crop.top * scale,
    bottom: state.crop.bottom * scale,
  });
  const frames = Math.max(1, state.info.frames);
  draw.lanes($<HTMLCanvasElement>("editor-timeline"), [{ bins: state.bins, color: draw.token("--purple") }], state.start / frames, state.end / frames, state.playhead / frames);
  const fps = state.info.fps;
  $("editor-range").textContent = `Range · ${stamp(state.start / fps)} to ${stamp(state.end / fps)} · ${stamp((state.end - state.start) / fps)}`;
}

function renderVideoEditor(): void {
  const state = video;
  if (!state) return;
  const { h, bridge } = host;
  const { info } = state;
  const busy = Boolean(state.exporting);
  const button = (id: string, className: string, label: string, enabled: boolean, action: () => void) => {
    const element = h("button", { className: `button ${className}`, type: "button", textContent: label, disabled: !enabled, dataset: { id } });
    element.addEventListener("click", action);
    return element;
  };
  $("editor-bar").replaceChildren(
    h("span", { className: "clock", textContent: info.name }),
    h("span", { className: "facts", textContent: busy ? state.exporting : `${info.width}x${info.height} ${info.fps} fps ${stamp(info.frames / info.fps)}` }),
    h("span", { className: "spacer" }),
    button("edit-save", "save", "Save", !busy, () => void exportVideo()),
    button("edit-reset", "", "Reset", !busy, () => {
      const bins = state.bins;
      const picture = state.picture;
      video = { ...fresh(info), bins, picture, wanted: null };
      renderVideoEditor();
    }),
    button("edit-reveal", "", "Show in folder", true, () => void bridge.reveal(info.path).catch(host.fail)),
    button("edit-close", "", "Close", !busy, () => void closeVideoEditor()),
  );
  $("editor-title").textContent = `Edit · ${info.name}`;
  const fields = $("editor-fields");
  fields.replaceChildren();
  const field = (label: string, value: number, limit: number, step: number, set: (value: number) => void) => {
    const input = h("input", { type: "text", value: String(value), spellcheck: false, disabled: busy, id: `edit-${label.toLowerCase().replace(/[^a-z]+/g, "-")}` });
    input.addEventListener("change", () => {
      const typed = Number(input.value);
      const safe = Number.isFinite(typed) ? Math.min(limit, Math.max(0, Math.round(typed / step) * step)) : 0;
      input.value = String(Number(safe.toFixed(2)));
      set(safe);
      paintVideo();
    });
    fields.append(h("div", { className: "field" }, h("label", { htmlFor: input.id, textContent: label }), input));
  };
  const seconds = (state.end - state.start) / info.fps;
  const { fades, crop } = state;
  field("Video fade in (s)", fades.videoIn, seconds, 0.1, (value) => (fades.videoIn = value));
  field("Video fade out (s)", fades.videoOut, seconds, 0.1, (value) => (fades.videoOut = value));
  field("Audio fade in (s)", fades.audioIn, seconds, 0.1, (value) => (fades.audioIn = value));
  field("Audio fade out (s)", fades.audioOut, seconds, 0.1, (value) => (fades.audioOut = value));
  // The encoder takes even numbers of pixels.
  field("Crop left (px)", crop.left, info.width - 16, 2, (value) => (crop.left = value));
  field("Crop right (px)", crop.right, info.width - 16, 2, (value) => (crop.right = value));
  field("Crop top (px)", crop.top, info.height - 16, 2, (value) => (crop.top = value));
  field("Crop bottom (px)", crop.bottom, info.height - 16, 2, (value) => (crop.bottom = value));
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
    host.videoSaved();
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
  dragRange(
    $<HTMLCanvasElement>("editor-timeline"),
    () => (video ? [video.start / video.info.frames, video.end / video.info.frames] : [0, 1]),
    (which, position) => {
      const state = video;
      if (!state || state.exporting) return;
      const frame = Math.round(position * state.info.frames);
      if (which === 0) state.start = Math.max(0, Math.min(frame, state.end - 1));
      else state.end = Math.min(state.info.frames, Math.max(frame, state.start + 1));
      // The picture follows the end that moves.
      seek(which === 0 ? state.start : state.end - 1);
    },
    (position) => {
      if (video && !video.exporting) seek(position * video.info.frames);
    },
  );
  window.addEventListener("resize", paintVideo);
  document.addEventListener("keydown", (event) => {
    const state = video;
    const typing = event.target instanceof HTMLInputElement;
    if (!state || typing || state.exporting || !$("editor").classList.contains("on") || document.querySelector("dialog[open]")) return;
    const step = event.shiftKey ? state.info.fps : 1;
    if (event.key === "ArrowLeft") seek(state.playhead - step);
    else if (event.key === "ArrowRight") seek(state.playhead + step);
    else if (event.key === "Escape") void closeVideoEditor();
    else return;
    event.preventDefault();
  });
}
