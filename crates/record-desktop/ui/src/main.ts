// The interface of the record desktop program: a home screen, one screen for
// each of the three modes, a menu bar, the activity panel, and the dialogs.

import { desktopBridge } from "./bridge";
import { demoBridge } from "./demo";
import * as draw from "./draw";
import { closeVideoEditor, openClipEditor, openVideoEditor, setupEditors, showVideoEditor, videoEditorInfo, videoEditorOpen } from "./edit";
import { forgetThumbnail, forgetThumbnails, kindOf, renderBrowser, renderTools, stamp, views, type Action, type Item, type View } from "./files";
import { dotIcons, lineIcons } from "./icons";
import { bind, changedBinds, commandFor, commands, comboOf, keysOf, sameKeys, setBinds, type Command } from "./keys";
import { closeMenus, setupMenus, type MenuEntry } from "./menu";
import type { CaptureKind, FileEntry, Flag, HotkeyId, HotkeyInfo, Mode, Monitor, Recorder, Region, Route, Save, Setting, ShotUpdate, Snapshot, SoundUpdate, Stopped, VideoUpdate } from "./types";

const bridge = desktopBridge() ?? demoBridge();
const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

type Phase = "idle" | "starting" | "countdown" | "rec" | "paused" | "saving";
const recorders: Recorder[] = ["audio", "video"];

/** The state of the recording of one mode. */
interface Rec {
  phase: Phase;
  facts: string;
  /** The file or the session folder. */
  file: string;
  seconds: number;
  since: number;
  banked: number;
  /** Seconds until the start, while the program counts down. */
  countdown: number;
  /** The user asked to discard this recording. */
  discarding: boolean;
}
const idleRec = (): Rec => ({ phase: "idle", facts: "", file: "", seconds: 0, since: 0, banked: 0, countdown: 0, discarding: false });

type Browser = "recent" | "audio" | "video";

/** What the interface stores through `ui_settings`. */
interface Ui {
  startAudioAtOnce: boolean;
  startVideoAtOnce: boolean;
  thumbnails: boolean;
  views: Record<Browser, View>;
  binds: Partial<Record<Command, string>>;
}
const defaultUi = (): Ui => ({ startAudioAtOnce: false, startVideoAtOnce: false, thumbnails: false, views: { recent: "details", audio: "list", video: "list" }, binds: {} });

const app = {
  route: "home" as Route,
  snapshot: null as Snapshot | null,
  monitors: [] as Monitor[],
  files: { audio: [], video: [], screenshot: [] } as Record<Mode, FileEntry[]>,
  rec: { audio: idleRec(), video: idleRec() } as Record<Recorder, Rec>,
  video: { monitor: 0, fps: 60, region: null as Region | null, useRegion: false, picture: null as ImageBitmap | null, polling: false, problem: "" },
  shots: { busy: false, picked: 0, bitmap: null as ImageBitmap | null, shown: "" },
  bands: [] as number[],
  /** The parts and clips of the newest audio session, with the time of each. */
  saves: [] as (Save & { at: number })[],
  /** The selected file of each browser that keeps a selection. */
  picked: { recent: null, audio: null } as Record<"recent" | "audio", string | null>,
  ui: defaultUi(),
  /** The mode that starts as soon as its screen is open. */
  pending: null as Recorder | null,
  activityOpen: false,
  /** Why the last change of a global hotkey was refused. */
  hotkeyProblem: null as { id: HotkeyId; text: string } | null,
};
const traces = { left: new draw.Trace(), right: new draw.Trace() };

// ---- Small helpers. ----

type Child = Node | string | null | false;

/** Creates one element with its properties and children. */
function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<HTMLElementTagNameMap[K]> & { dataset?: Record<string, string> } = {},
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const element = document.createElement(tag);
  const { dataset, ...rest } = props;
  Object.assign(element, rest);
  for (const [name, value] of Object.entries(dataset ?? {})) element.dataset[name] = value;
  for (const child of children) if (child !== null && child !== false) element.append(child);
  return element;
}

const reason = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : String(error));

/**
 * Shows one line in the status bar. With a path, the text up to the first
 * separator is a link that shows that file in its folder.
 */
function say(text: string, tone: "" | "good" | "bad" = "", path?: string): void {
  const message = $("message");
  message.className = tone;
  if (!path) {
    message.textContent = text;
    return;
  }
  const cut = text.indexOf(" · ");
  const link = h("button", { className: "link", type: "button", textContent: cut < 0 ? text : text.slice(0, cut), title: "Show in folder", dataset: { id: "message-link" } });
  link.addEventListener("click", () => void bridge.reveal(path).catch(fail));
  message.replaceChildren(link, cut < 0 ? "" : text.slice(cut));
}

/** Shows the reason of a failed command in the status bar. */
function fail(error: unknown): void {
  say(reason(error), "bad");
}

function clock(seconds: number): string {
  const whole = Math.max(0, Math.floor(seconds));
  const two = (value: number) => String(value).padStart(2, "0");
  return `${two(Math.floor(whole / 3600))}:${two(Math.floor(whole / 60) % 60)}:${two(whole % 60)}`;
}

const number = (value: number) => String(Number(value.toFixed(2)));
const baseName = (path: string) => path.split(/[\\/]/).pop() ?? path;

interface RowOptions {
  id: string;
  name: string;
  /** Keys in front of the name. */
  keys?: string;
  value?: string;
  selected?: boolean;
  /** A switch: on, off, or on without effect. Leave it out for a plain row. */
  dot?: boolean | "taken";
  /** Shown but not usable. */
  off?: boolean;
  /** Without the column of the selection mark. */
  bare?: boolean;
  title?: string;
  action?: () => void;
}

/** Adds one row: a marker, an optional switch, a name, and a value. */
function row(parent: HTMLElement, options: RowOptions): void {
  const { id, name, keys, value = "", selected = false, dot, off = false, bare = false, title = "", action } = options;
  const marks = [
    h("span", { className: "mark", textContent: "▌" }),
    dot === undefined ? h("span") : h("span", { className: `dot${dot === true ? " on" : dot === "taken" ? " taken" : ""}`, textContent: dot ? "●" : "○" }),
    h("span", { className: "name" }, keys !== undefined && h("kbd", { textContent: keys }), name),
    h("span", { className: `val${dot === "taken" ? " taken" : ""}`, textContent: value }),
  ];
  const className = `row${selected ? " sel" : ""}${off ? " off" : ""}${bare ? " bare" : ""}`;
  if (!action) {
    parent.append(h("div", { className: `${className} plain`, title }, ...marks));
    return;
  }
  const button = h("button", { className, type: "button", title, disabled: off, dataset: { id } }, ...marks);
  if (dot !== undefined) {
    button.setAttribute("role", "switch");
    button.setAttribute("aria-checked", String(dot !== false));
  }
  button.addEventListener("click", action);
  parent.append(button);
}

function note(parent: HTMLElement, text: string): void {
  parent.append(h("p", { className: "note", textContent: text }));
}

function textButton(id: string, className: string, label: string, title: string, enabled: boolean, action: () => void): HTMLButtonElement {
  const element = h("button", { className: `button ${className}`, type: "button", textContent: label, title, disabled: !enabled, dataset: { id } });
  element.addEventListener("click", action);
  return element;
}

// ---- Interface settings. ----

function readUi(text: string): Ui {
  const ui = defaultUi();
  try {
    const stored = JSON.parse(text) as Record<string, unknown> | null;
    if (!stored || typeof stored !== "object") return ui;
    for (const name of ["startAudioAtOnce", "startVideoAtOnce", "thumbnails"] as const) {
      if (typeof stored[name] === "boolean") ui[name] = stored[name] as boolean;
    }
    const stored_views = stored.views as Record<string, unknown> | undefined;
    for (const name of ["recent", "audio", "video"] as const) {
      const view = stored_views?.[name];
      if (views.some((one) => one.view === view)) ui.views[name] = view as View;
    }
    setBinds(stored.binds);
    ui.binds = changedBinds();
  } catch {
    // Text that is not JSON counts as no settings.
  }
  return ui;
}

function saveUi(): void {
  app.ui.binds = changedBinds();
  void bridge.setUiSettings(JSON.stringify(app.ui)).catch(fail);
}

function setView(browser: Browser, view: View): void {
  app.ui.views[browser] = view;
  // The thumbnail view is the place where the user turns the pictures on.
  if (view === "thumbs") app.ui.thumbnails = true;
  saveUi();
  render();
}

// ---- Routes. ----

const hashes: Record<string, Route> = { audio: "audio", video: "video", screenshot: "screenshot" };
const isRecorder = (route: Route): route is Recorder => route === "audio" || route === "video";

function readRoute(): Route {
  return hashes[location.hash.replace(/^#\/?/, "")] ?? "home";
}

function go(route: Route): void {
  location.hash = route === "home" ? "/" : `/${route}`;
}

async function refreshFiles(mode: Mode): Promise<void> {
  try {
    app.files[mode] = await bridge.files(mode);
  } catch (error) {
    fail(error);
  }
  if (mode === "screenshot") app.shots.picked = Math.min(app.shots.picked, Math.max(0, app.files.screenshot.length - 1));
  render();
}

async function enter(route: Route): Promise<void> {
  app.route = route;
  closeActivity();
  render();
  if (route === "home") await Promise.all((["audio", "video", "screenshot"] as const).map(refreshFiles));
  else await refreshFiles(route);
  if (route === "video") {
    try {
      app.monitors = await bridge.monitors();
      if (!app.monitors.some((monitor) => monitor.index === app.video.monitor)) {
        app.video.monitor = (app.monitors.find((monitor) => monitor.primary) ?? app.monitors[0])?.index ?? 0;
      }
    } catch (error) {
      fail(error);
    }
    render();
  }
  if (app.pending && app.pending === app.route) {
    const mode = app.pending;
    app.pending = null;
    void startRecording(mode);
  } else app.pending = null;
}

// ---- Recording. Audio and video run independently. ----

const active = (mode: Recorder) => app.rec[mode].phase !== "idle";
const live = (mode: Recorder) => app.rec[mode].phase === "rec" || app.rec[mode].phase === "paused";
/** A recording that can be saved, cancelled, or discarded. */
const stoppable = (mode: Recorder) => live(mode) || app.rec[mode].phase === "starting" || app.rec[mode].phase === "countdown";

function elapsed(mode: Recorder): number {
  const rec = app.rec[mode];
  if (mode === "audio") return rec.seconds;
  return rec.banked + (rec.phase === "rec" ? (performance.now() - rec.since) / 1000 : 0);
}

/** The mode that a recording command acts on: this screen, or the one recording. */
function target(): Recorder | null {
  if (isRecorder(app.route)) return app.route;
  const running = recorders.filter(active);
  return running.length === 1 ? running[0]! : null;
}

/** Empties the state of one mode before a new recording. */
function resetRecording(mode: Recorder): void {
  app.rec[mode] = idleRec();
  if (mode !== "audio") return;
  app.bands = [];
  traces.left.clear();
  traces.right.clear();
}

async function startRecording(mode: Recorder): Promise<void> {
  if (active(mode)) return;
  resetRecording(mode);
  app.rec[mode].phase = "starting";
  if (mode === "audio") app.saves = [];
  say("");
  render();
  const { monitor, fps, region, useRegion } = app.video;
  try {
    const path = await bridge.startRecording(mode, mode === "video" ? { monitor, fps, region: useRegion ? region : null } : {});
    if (active(mode) && !app.rec[mode].file) {
      app.rec[mode].file = path;
      render();
    }
  } catch (error) {
    const phase = app.rec[mode].phase;
    if (phase === "starting" || phase === "countdown") resetRecording(mode);
    render();
    fail(error);
  }
}

async function togglePause(mode: Recorder | null = target()): Promise<void> {
  if (!mode || !live(mode)) return;
  try {
    const paused = await bridge.setPaused(mode, app.rec[mode].phase === "rec");
    const rec = app.rec[mode];
    if (rec.phase !== "rec" && rec.phase !== "paused") return;
    if (paused && rec.phase === "rec") rec.banked += (performance.now() - rec.since) / 1000;
    if (!paused) rec.since = performance.now();
    rec.phase = paused ? "paused" : "rec";
    render();
  } catch (error) {
    fail(error);
  }
}

async function stopRecording(mode: Recorder | null = target()): Promise<void> {
  if (!mode || !stoppable(mode)) return;
  const rec = app.rec[mode];
  if (rec.phase === "rec") rec.banked += (performance.now() - rec.since) / 1000;
  // A start that still waits is cancelled. There is nothing to save then.
  if (live(mode)) rec.phase = "saving";
  render();
  try {
    await bridge.stopRecording(mode);
  } catch (error) {
    fail(error);
  }
}

/** Asks once, then stops the recording and deletes what it wrote. */
function askDiscard(mode: Recorder | null = target()): void {
  if (!mode || !stoppable(mode)) return;
  const discard = async () => {
    dialog.close();
    app.rec[mode].discarding = true;
    try {
      await bridge.discardRecording(mode);
    } catch (error) {
      app.rec[mode].discarding = false;
      fail(error);
    }
  };
  openDialog(
    "Discard",
    (body) => {
      const sure = h("button", { className: "button go", type: "submit", textContent: "Discard", id: "discard-yes" });
      body.append(
        h("p", { textContent: `${modeNames[mode]} · ${clock(elapsed(mode))}` }),
        h("div", { className: "buttons" }, closeButton("Cancel"), sure),
      );
    },
    () => void discard(),
  );
  $("discard-yes").focus();
}

function recordOrPause(): void {
  if (!isRecorder(app.route)) return;
  if (!active(app.route)) void startRecording(app.route);
  else void togglePause(app.route);
}

/** The backend announces every start, also one from a hotkey or the tray. */
function onStarting(mode: Recorder, path: string, countdown: number): void {
  resetRecording(mode);
  const rec = app.rec[mode];
  rec.file = path;
  rec.countdown = countdown;
  rec.phase = countdown > 0 ? "countdown" : "starting";
  if (mode === "audio") app.saves = [];
  render();
}

function onStarted(mode: Recorder, facts: string): void {
  const rec = app.rec[mode];
  rec.facts = facts;
  if (rec.phase !== "paused" && rec.phase !== "saving") {
    rec.phase = "rec";
    rec.since = performance.now();
  }
  render();
}

function onStopped(mode: Recorder, update: Stopped): void {
  const before = app.rec[mode];
  // A start from a hotkey or the tray that failed: there is no recording to end.
  if (before.phase === "idle" && update.error) {
    say(`Recording failed: ${update.error}`, "bad");
    return;
  }
  const path = update.path ?? before.file;
  app.rec[mode] = idleRec();
  if (mode === "audio") {
    app.bands = [];
    if (update.discarded) app.saves = [];
  }
  // A discard that could not delete leaves its files, so the path stays a link.
  if (update.error && before.discarding) say(`Discard failed: ${update.error}`, "bad");
  else if (update.error) say(`Recording failed: ${update.error}`, "bad");
  else if (update.discarded) say("Discarded");
  else if (update.file) say(`Saved ${update.file}`, "good", path || undefined);
  render();
  // A finished video opens in the editor, unless another one is open there.
  if (mode === "video" && !update.error && !update.discarded && path && app.route === "video" && !videoEditorOpen()) void openVideoEditor(path);
  void refreshFiles(mode);
}

function onSound(update: SoundUpdate): void {
  const rec = app.rec.audio;
  switch (update.kind) {
    case "starting":
      onStarting("audio", update.path, update.countdown);
      break;
    case "countdown":
      rec.phase = "countdown";
      rec.countdown = update.seconds;
      render();
      break;
    case "started":
      traces.left.setRate(update.sampleRate);
      traces.right.setRate(update.sampleRate);
      onStarted("audio", `MP3 ${Math.round(update.bitrate / 1000)} kbps ${update.sampleRate / 1000} kHz`);
      break;
    case "block":
      traces.left.push(update.left);
      traces.right.push(update.right);
      app.bands = update.bands;
      rec.seconds = update.seconds;
      break;
    case "finalizing":
      rec.phase = "saving";
      render();
      break;
    case "saved":
      app.saves.push({ name: update.name, path: update.path, clip: update.clip, seconds: update.seconds, editable: update.editable, at: Date.now() });
      if (update.clip) say(`Clip ${update.name}`, "good", update.path);
      render();
      break;
    case "notice":
      say(update.text);
      break;
    case "stopped":
      onStopped("audio", update);
      break;
  }
}

/** Asks for a name and cuts the audio since the last file into a clip. */
function openClipName(): void {
  if (!live("audio")) return;
  const count = app.saves.filter((save) => save.clip).length + 1;
  const input = h("input", { type: "text", id: "clip-name", value: `clip-${count}`, spellcheck: false });
  const problem = h("p", { className: "problem" });
  const send = async () => {
    try {
      await bridge.saveClip(input.value);
      dialog.close();
    } catch (error) {
      problem.textContent = reason(error);
    }
  };
  openDialog(
    "Clip",
    (body) => {
      body.append(
        h("div", { className: "field" }, h("label", { htmlFor: input.id, textContent: "Name" }), input),
        problem,
        h("div", { className: "buttons" }, closeButton("Cancel"), h("button", { className: "button save", type: "submit", textContent: "Create clip" })),
      );
    },
    () => void send(),
  );
  input.select();
}

function onVideo(update: VideoUpdate): void {
  const rec = app.rec.video;
  switch (update.kind) {
    case "starting":
      // The preview follows what the recording captures.
      if (app.monitors.some((monitor) => monitor.index === update.monitor)) app.video.monitor = update.monitor;
      app.video.useRegion = update.region !== null;
      if (update.region) app.video.region = update.region;
      onStarting("video", update.path, update.countdown);
      break;
    case "countdown":
      rec.phase = "countdown";
      rec.countdown = update.seconds;
      render();
      break;
    case "started":
      onStarted("video", `${update.width}x${update.height} ${update.fps} fps H.264 AAC`);
      break;
    case "finalizing":
      rec.phase = "saving";
      render();
      break;
    case "notice":
      say(update.text);
      break;
    case "stopped":
      onStopped("video", update);
      break;
  }
}

/** The part of the desktop that the next video records. */
function sourceBox(): Region | null {
  const video = app.video;
  if (video.useRegion && video.region) return video.region;
  return app.monitors.find((one) => one.index === video.monitor) ?? null;
}

/** Opens the region selector of the desktop and takes its answer as the source. */
async function pickRegion(): Promise<void> {
  if (active("video")) return;
  try {
    const region = await bridge.selectRegion();
    if (!region) return;
    app.video.region = region;
    app.video.useRegion = true;
    app.video.picture?.close();
    app.video.picture = null;
    render();
  } catch (error) {
    fail(error);
  }
}

/**
 * Shows the source as it looks now. This runs while the screen capture
 * screen shows the live picture, with or without a recording.
 */
async function pollPicture(): Promise<void> {
  const video = app.video;
  const box = sourceBox();
  if (video.polling || !box || app.route !== "video" || videoEditorOpen() || document.hidden) return;
  video.polling = true;
  let problem = "";
  try {
    const { left, top, width, height } = box;
    const picture = await createImageBitmap(await bridge.monitorPreview({ left, top, width, height }));
    if (sourceBox() === box) {
      video.picture?.close();
      video.picture = picture;
    } else picture.close();
  } catch (error) {
    problem = reason(error);
  }
  if (problem !== video.problem) {
    video.problem = problem;
    const empty = $("desktop-empty");
    empty.textContent = problem;
    empty.hidden = !problem || videoEditorOpen();
  }
  video.polling = false;
}

// ---- Screenshots. ----

const captures: { kind: CaptureKind; name: string; hotkey?: HotkeyId }[] = [
  { kind: "region", name: "Region", hotkey: "region" },
  { kind: "fullscreen", name: "Fullscreen", hotkey: "fullscreen" },
  { kind: "window", name: "Active window", hotkey: "window" },
  { kind: "monitor", name: "Active monitor" },
  { kind: "last", name: "Last region" },
];

/** The keys of one global hotkey, while it is on. */
function hotkeyKeys(id: HotkeyId | undefined): string {
  const hotkey = app.snapshot?.hotkeys.find((one) => one.id === id);
  return hotkey?.enabled ? hotkey.keys : "";
}

async function capture(kind: CaptureKind, delayed = false): Promise<void> {
  if (app.shots.busy) return;
  try {
    await bridge.capture(kind, delayed);
  } catch (error) {
    fail(error);
  }
}

async function toggleAuto(): Promise<void> {
  try {
    await bridge.toggleAuto();
  } catch (error) {
    fail(error);
  }
}

function onShot(update: ShotUpdate): void {
  const shots = app.shots;
  switch (update.kind) {
    case "capturing":
      shots.busy = true;
      say("Capturing");
      break;
    case "finished": {
      shots.busy = false;
      const parts = [update.file ? `Saved ${update.file}` : "Copied", `${update.width}x${update.height}`];
      if (update.url) parts.splice(0, 2, `Uploaded ${update.url}`, update.copied ? "link copied" : "");
      else if (update.copied && update.file) parts.push("copied");
      if (update.hdrNits !== null) parts.push(`HDR at ${Math.round(update.hdrNits)} nits`);
      const linked = !update.error && !update.url && update.file && update.path;
      if (update.error) parts.unshift(update.error);
      say(parts.filter(Boolean).join(" · "), update.error ? "bad" : "good", linked ? (update.path ?? undefined) : undefined);
      shots.picked = 0;
      // An edited file keeps its name, so the picture must load again.
      shots.shown = "";
      if (update.path) forgetThumbnail(update.path);
      void refreshFiles("screenshot");
      break;
    }
    case "cancelled":
      shots.busy = false;
      say("Cancelled");
      break;
    case "failed":
      shots.busy = false;
      say(`Capture failed: ${update.error}`, "bad");
      break;
    case "auto":
      if (app.snapshot) app.snapshot.autoActive = update.active;
      say(update.active ? `Auto capture every ${number(app.snapshot?.intervalSeconds ?? 0)} s` : "Auto capture stopped");
      break;
  }
  render();
}

function pickedShot(): FileEntry | undefined {
  return app.files.screenshot[app.shots.picked];
}

/** Loads the selected screenshot and draws it, or a stand-in in the simulation. */
async function showShot(): Promise<void> {
  const shots = app.shots;
  const file = pickedShot();
  const canvas = $<HTMLCanvasElement>("shot");
  $("shot-empty").hidden = Boolean(file);
  if (!file) {
    shots.shown = "";
    draw.contain(canvas, null, 0, 0);
    return;
  }
  if (bridge.demo) {
    draw.standInShot(canvas, app.files.screenshot.length - shots.picked);
    return;
  }
  if (shots.shown !== file.path) {
    shots.shown = file.path;
    shots.bitmap = null;
    try {
      const blob = await bridge.picture(file.path);
      const bitmap = blob ? await createImageBitmap(blob) : null;
      if (shots.shown !== file.path) return;
      shots.bitmap = bitmap;
    } catch (error) {
      if (shots.shown === file.path) fail(error);
    }
  }
  draw.contain(canvas, shots.bitmap, shots.bitmap?.width ?? 0, shots.bitmap?.height ?? 0);
}

async function withShot(action: (path: string) => Promise<void>, done: string): Promise<void> {
  const file = pickedShot();
  if (!file) return;
  try {
    await action(file.path);
    if (done) say(done.replace("{}", file.name), "good");
  } catch (error) {
    fail(error);
  }
}

// ---- Settings that change through the bridge. ----

async function change(request: Promise<Snapshot>): Promise<boolean> {
  try {
    app.snapshot = await request;
    render();
    return true;
  } catch (error) {
    fail(error);
    return false;
  }
}

function toggleFlag(name: Flag): void {
  const snapshot = app.snapshot;
  if (!snapshot) return;
  if (name === "upload" && !snapshot.upload && !snapshot.destination) {
    openDestinations();
    return;
  }
  void change(bridge.setFlag(name, !snapshot[name]));
}

function folderOf(route: Route): string {
  const snapshot = app.snapshot;
  if (!snapshot || route === "home") return "";
  return route === "audio" ? snapshot.audioFolder : route === "video" ? snapshot.videoFolder : snapshot.folder;
}

async function openFolder(): Promise<void> {
  if (app.route === "home") return;
  try {
    await bridge.openFolder(app.route);
  } catch (error) {
    fail(error);
  }
}

/** The click on the path of the status bar: the recording, or the folder. */
function openOutput(): void {
  const route = app.route;
  if (isRecorder(route) && active(route) && app.rec[route].file) void bridge.reveal(app.rec[route].file).catch(fail);
  else void openFolder();
}

// ---- Files. ----

const toItem = (file: FileEntry): Item => ({ path: file.path, name: file.name, modifiedMs: file.modifiedMs, sizeBytes: file.sizeBytes });

const reveal = (path: string) => void bridge.reveal(path).catch(fail);

/** Moves one listed file to the Recycle Bin. */
async function removeFile(path: string): Promise<void> {
  try {
    if (videoEditorInfo()?.path === path) await closeVideoEditor();
    if (videoEditorInfo()?.path === path) return;
    await bridge.deleteFile(path);
  } catch (error) {
    fail(error);
    return;
  }
  forgetThumbnail(path);
  app.saves = app.saves.filter((save) => save.path !== path);
  for (const name of ["recent", "audio"] as const) if (app.picked[name] === path) app.picked[name] = null;
  say(`Deleted ${baseName(path)}`);
  void refreshFiles(kindOf(path));
}

/** The buttons that every browser has for its selected file. */
function fileActions(path: string | null, more: Action[] = []): Action[] {
  return [
    ...more,
    { id: "reveal", label: "Show in folder", icon: lineIcons.folder, enabled: path !== null, run: () => path && reveal(path) },
    { id: "delete", label: "Delete", icon: lineIcons.trash, enabled: path !== null, run: () => path && void removeFile(path) },
  ];
}

function browser(name: Browser, items: Item[], selected: string | null, empty: string, select: (item: Item) => void, open: (item: Item) => void): void {
  renderBrowser($(name === "recent" ? "recent" : `${name}-files`), {
    items,
    selected,
    view: app.ui.views[name],
    thumbnails: app.ui.thumbnails,
    empty,
    thumbnail: bridge.thumbnail,
    select,
    open,
    remove: (item) => void removeFile(item.path),
  });
}

// ---- Rendering. ----

const stateLabels: Record<Phase, string> = { idle: "○ Ready", starting: "○ Starting", countdown: "○ Starts in", rec: "● Recording", paused: "● Paused", saving: "○ Saving" };
const modeNames: Record<Recorder, string> = { audio: "System audio", video: "Screen capture" };
const routeNames: Record<Route, string> = { home: "Home", audio: "System audio", video: "Screen capture", screenshot: "Screenshot" };

function stateText(mode: Recorder): string {
  const rec = app.rec[mode];
  const label = stateLabels[rec.phase].toUpperCase();
  return rec.phase === "countdown" ? `${label} ${rec.countdown}` : label;
}

function renderTabs(): void {
  const { shots, route, snapshot } = app;
  for (const tab of document.querySelectorAll<HTMLElement>(".tab")) {
    const target = tab.dataset.route as Route;
    tab.setAttribute("aria-selected", String(target === route));
    const busy = target === "screenshot" ? shots.busy || Boolean(snapshot?.autoActive) : isRecorder(target) && active(target);
    tab.classList.toggle("busy", busy);
    tab.title = `${routeNames[target]} · ${keysOf(target)}`;
  }
  for (const view of document.querySelectorAll<HTMLElement>(".view")) view.classList.toggle("on", view.id === route);
  for (const choice of document.querySelectorAll<HTMLAnchorElement>(".choice")) {
    const mode = choice.dataset.mode as Mode;
    choice.classList.toggle("busy", isRecorder(mode) && active(mode));
    const keys = choice.querySelector("kbd");
    if (keys) keys.textContent = keysOf(mode);
  }
  renderActivity();
}

/** The short text of the activity button: the count, and the state of a single recording. */
function activityLabel(): string {
  const running = recorders.filter(active);
  if (!running.length) return "";
  if (running.length > 1) return String(running.length);
  const mode = running[0]!;
  const rec = app.rec[mode];
  const state = rec.phase === "countdown" ? `${rec.countdown} s` : live(mode) ? clock(elapsed(mode)) : stateLabels[rec.phase].slice(2).toLowerCase();
  return `1 · ${state}`;
}

/** The activity button and its panel: every recording that runs, with its controls. */
function renderActivity(): void {
  const running = recorders.filter(active);
  const button = $("activity");
  button.classList.toggle("on", running.length > 0);
  button.classList.toggle("rec", running.some((mode) => app.rec[mode].phase === "rec"));
  button.setAttribute("aria-expanded", String(app.activityOpen));
  button.title = `Activity · ${keysOf("activity")}`;
  button.setAttribute("aria-label", running.length ? `Activity, ${running.length} recording` : "Activity, idle");
  $("activity-count").textContent = activityLabel();
  const panel = $("activity-panel");
  panel.hidden = !app.activityOpen;
  if (!app.activityOpen) return;
  const list = $("activity-list");
  list.replaceChildren();
  for (const mode of running) {
    const rec = app.rec[mode];
    const jump = h("button", { className: "jump", type: "button", title: "Open", dataset: { id: `act-go-${mode}` } }, modeNames[mode]);
    jump.insertAdjacentHTML("beforeend", lineIcons.jump);
    jump.addEventListener("click", () => {
      closeActivity();
      go(mode);
    });
    list.append(
      h(
        "div",
        { className: "act" },
        jump,
        h("span", { className: `state ${rec.phase}`, textContent: stateText(mode) }),
        h("span", { className: "clock", id: `act-clock-${mode}`, textContent: clock(elapsed(mode)) }),
        h(
          "div",
          { className: "act-buttons" },
          textButton(`act-pause-${mode}`, "", rec.phase === "paused" ? "Resume" : "Pause", "", live(mode), () => void togglePause(mode)),
          textButton(`act-stop-${mode}`, "save", "■ Save and stop", "", stoppable(mode), () => void stopRecording(mode)),
          textButton(`act-discard-${mode}`, "", "Discard", "", stoppable(mode), () => askDiscard(mode)),
        ),
      ),
    );
  }
  if (!running.length) note(list, "Idle");
}

function closeActivity(): void {
  if (!app.activityOpen) return;
  app.activityOpen = false;
  renderActivity();
}

function toggleActivity(): void {
  app.activityOpen = !app.activityOpen;
  closeMenus();
  renderActivity();
  if (app.activityOpen) $("activity").focus();
}

function renderTransport(mode: Recorder): void {
  const rec = app.rec[mode];
  const phase = rec.phase;
  const box = sourceBox();
  const ready = mode === "audio" ? "MP3 320 kbps" : box ? `${box.width}x${box.height} ${app.video.fps} fps H.264 AAC` : `${app.video.fps} fps H.264 AAC`;
  const parts: (Node | false)[] = [
    h("span", { className: `state ${phase}`, textContent: stateText(mode) }),
    h("span", { className: "clock", id: `clock-${mode}`, textContent: clock(elapsed(mode)) }),
    h("span", { className: "facts", textContent: rec.facts || ready }),
    textButton(`record-${mode}`, "go", "● Record", keysOf("record"), phase === "idle", () => void startRecording(mode)),
    textButton(`pause-${mode}`, "", phase === "paused" ? "Resume" : "Pause", keysOf("pause"), live(mode), () => void togglePause(mode)),
    mode === "audio" && textButton("clip-audio", "", "Clip", keysOf("clip"), live(mode), openClipName),
    textButton(`stop-${mode}`, "save", "■ Save and stop", keysOf("stop"), stoppable(mode), () => void stopRecording(mode)),
    textButton(`discard-${mode}`, "", "Discard", keysOf("discard"), stoppable(mode), () => askDiscard(mode)),
  ];
  $(`transport-${mode}`).replaceChildren(...parts.filter((part): part is Node => part !== false));
}

function renderHome(): void {
  const all = (["audio", "video", "screenshot"] as const)
    .flatMap((mode) => app.files[mode])
    .sort((first, second) => second.modifiedMs - first.modifiedMs)
    .slice(0, 60);
  const picked = all.some((file) => file.path === app.picked.recent) ? app.picked.recent : null;
  app.picked.recent = picked;
  const open = (path: string) => void bridge.openFile(path).catch(fail);
  browser(
    "recent",
    all.map(toItem),
    picked,
    "No files",
    (item) => {
      app.picked.recent = item.path;
      render();
    },
    (item) => open(item.path),
  );
  renderTools(
    $("recent-tools"),
    fileActions(picked, [{ id: "open", label: "Open", icon: lineIcons.open, enabled: picked !== null, run: () => picked && open(picked) }]),
    app.ui.views.recent,
    (view) => setView("recent", view),
  );
  $("card-format").textContent = app.snapshot?.format ?? "PNG";

  const hotkeys = $("hotkeys");
  hotkeys.replaceChildren();
  renderTools($("hotkeys-tools"), [{ id: "binds", label: "Key binds", icon: lineIcons.keyboard, enabled: true, run: openKeyBinds }], null, () => undefined);
  const snapshot = app.snapshot;
  if (snapshot?.engineError) note(hotkeys, `Screenshot engine failed: ${snapshot.engineError}`);
  for (const hotkey of snapshot?.hotkeys ?? []) hotkeyRow(hotkeys, hotkey);
}

/** Turns one global hotkey on or off. A refusal is kept for its row. */
async function toggleHotkey(hotkey: HotkeyInfo): Promise<string> {
  let problem = "";
  try {
    app.snapshot = await bridge.setHotkeyEnabled(hotkey.id, !hotkey.enabled);
  } catch (error) {
    problem = reason(error);
  }
  app.hotkeyProblem = problem ? { id: hotkey.id, text: problem } : null;
  render();
  return problem;
}

/** One global hotkey as a switch: green when it works, amber when another program holds the keys. */
function hotkeyRow(parent: HTMLElement, hotkey: HotkeyInfo): void {
  const taken = hotkey.enabled && !hotkey.registered;
  const problem = app.hotkeyProblem?.id === hotkey.id ? app.hotkeyProblem.text : "";
  row(parent, {
    id: `hotkey-${hotkey.id}`,
    keys: hotkey.keys,
    name: hotkey.label,
    dot: taken ? "taken" : hotkey.enabled,
    value: taken ? "in use" : "",
    bare: true,
    action: () => void toggleHotkey(hotkey),
  });
  // The reason of a refused change stays under the row.
  if (problem) parent.append(h("p", { className: "note bad", textContent: problem }));
}

/** The session first, newest on top, then the older recordings. */
function audioItems(): Item[] {
  const listed = new Map(app.files.audio.map((file) => [file.path, file]));
  const saves = [...app.saves].reverse().map((save): Item => {
    const file = listed.get(save.path);
    return { path: save.path, name: save.name, modifiedMs: file?.modifiedMs ?? save.at, sizeBytes: file?.sizeBytes ?? null, extra: clock(save.seconds) };
  });
  const session = new Set(saves.map((save) => save.path));
  return [...saves, ...app.files.audio.filter((file) => !session.has(file.path)).map(toItem)];
}

function renderAudio(): void {
  renderTransport("audio");
  const items = audioItems();
  const picked = items.some((item) => item.path === app.picked.audio) ? app.picked.audio : null;
  app.picked.audio = picked;
  const save = app.saves.find((one) => one.path === picked);
  $("audio-files-title").textContent = `Recordings · ${items.length}`;
  browser(
    "audio",
    items,
    picked,
    "No recordings",
    (item) => {
      app.picked.audio = item.path;
      render();
    },
    (item) => {
      const clip = app.saves.find((one) => one.path === item.path);
      if (clip?.editable) void openClipEditor(clip);
      else void bridge.openFile(item.path).catch(fail);
    },
  );
  renderTools(
    $("audio-tools"),
    fileActions(picked, [{ id: "edit", label: "Trim", icon: lineIcons.edit, enabled: Boolean(save?.editable), run: () => save && void openClipEditor(save) }]),
    app.ui.views.audio,
    (view) => setView("audio", view),
  );
}

function renderVideo(): void {
  renderTransport("video");
  const { video } = app;
  const idle = !active("video");
  const editing = videoEditorInfo();
  $("video").classList.toggle("editing", editing !== null);
  const sources = $("sources");
  sources.replaceChildren();
  const fromMonitor = (index: number) => !video.useRegion && index === video.monitor;
  for (const monitor of app.monitors) {
    row(sources, {
      id: `monitor-${monitor.index}`,
      name: `Display ${monitor.index + 1} · ${monitor.width}x${monitor.height}`,
      value: monitor.primary ? "primary" : "",
      selected: fromMonitor(monitor.index),
      off: !idle && !fromMonitor(monitor.index),
      action: () => {
        if (active("video")) return;
        video.monitor = monitor.index;
        video.useRegion = false;
        video.picture?.close();
        video.picture = null;
        render();
      },
    });
  }
  row(sources, {
    id: "source-region",
    name: "Region",
    value: video.region ? `${video.region.width}x${video.region.height}` : "select",
    selected: video.useRegion,
    off: !idle && !video.useRegion,
    action: () => void pickRegion(),
  });
  const settings = $("video-settings");
  settings.replaceChildren();
  row(settings, {
    id: "fps",
    name: "Frame rate",
    value: `${video.fps} fps`,
    off: !idle,
    action: () => {
      video.fps = video.fps === 60 ? 30 : 60;
      render();
    },
  });
  row(settings, { id: "video-codec", name: "Video", value: "H.264 · 20 Mbps" });
  row(settings, { id: "audio-codec", name: "Audio", value: "AAC · 192 kbps" });
  const source = video.useRegion && video.region ? `Region ${video.region.width}x${video.region.height}` : `Display ${video.monitor + 1}`;
  $("picture-title").textContent = `Viewer · ${editing ? editing.name : source}`;
  $("desktop-empty").hidden = !video.problem || editing !== null;
  $("timeline-empty").hidden = editing !== null;
  if (!editing) $("editor-range").textContent = "Timeline";

  // The media pool: a click opens a recording in the viewer and the timeline.
  const picked = editing?.path ?? null;
  $("video-files-title").textContent = `Media pool · ${app.files.video.length}`;
  browser(
    "video",
    app.files.video.map(toItem),
    picked,
    "No recordings",
    (item) => {
      if (item.path !== picked) void openVideoEditor(item.path);
    },
    () => undefined,
  );
  renderTools($("video-tools"), fileActions(picked), app.ui.views.video, (view) => setView("video", view));
  if (editing) showVideoEditor();
}

function renderScreenshot(): void {
  const snapshot = app.snapshot;
  const { shots } = app;
  const menu = $("capture-menu");
  menu.replaceChildren();
  for (const item of captures) {
    row(menu, { id: `capture-${item.kind}`, name: item.name, value: hotkeyKeys(item.hotkey), off: shots.busy, action: () => void capture(item.kind) });
  }
  menu.append(h("div", { className: "gap" }));
  const delay = number(snapshot?.delaySeconds ?? 3);
  row(menu, { id: "delay-region", name: `Region after ${delay} s`, off: shots.busy, action: () => void capture("region", true) });
  row(menu, { id: "delay-full", name: `Fullscreen after ${delay} s`, off: shots.busy, action: () => void capture("fullscreen", true) });
  menu.append(h("div", { className: "gap" }));
  row(menu, {
    id: "auto",
    name: `Auto capture every ${number(snapshot?.intervalSeconds ?? 5)} s`,
    dot: Boolean(snapshot?.autoActive),
    action: () => void toggleAuto(),
  });
  if (snapshot?.engineError) note(menu, `Screenshot engine failed: ${snapshot.engineError}`);

  const after = $("after-menu");
  after.replaceChildren();
  if (snapshot) {
    row(after, { id: "flag-copy", name: "Copy", dot: snapshot.copy, action: () => toggleFlag("copy") });
    row(after, { id: "flag-save", name: "Save", value: snapshot.format, dot: snapshot.save, action: () => toggleFlag("save") });
    row(after, { id: "flag-upload", name: "Upload", dot: snapshot.upload, action: () => toggleFlag("upload") });
    row(after, { id: "destination", name: "Destination", value: snapshot.destination ?? "none", title: keysOf("destination"), action: openDestinations });
  }

  const history = $("history");
  history.replaceChildren();
  app.files.screenshot.forEach((file, index) => {
    row(history, {
      id: `shot-${file.path}`,
      name: file.name,
      value: stamp(file.modifiedMs),
      selected: index === shots.picked,
      action: () => {
        shots.picked = index;
        render();
      },
    });
  });
  $("history-title").textContent = `History · ${app.files.screenshot.length}`;
  const file = pickedShot();
  $("preview-title").textContent = file?.name ?? "Preview";
  $("shot-empty").textContent = "No screenshots";
  const action = (id: string, label: string, run: () => void) => textButton(id, "", label, "", Boolean(file), run);
  $("shot-actions").replaceChildren(
    action("shot-edit", "Edit", () => void withShot(bridge.editScreenshot, "")),
    action("shot-copy", "Copy image", () => void withShot(bridge.copyPicture, "Copied {}")),
    action("shot-reveal", "Show in folder", () => void withShot(bridge.reveal, "")),
    action("shot-open", "Open", () => void withShot(bridge.openFile, "")),
    action("shot-delete", "Delete", () => {
      const picked = pickedShot();
      if (picked) void removeFile(picked.path);
    }),
  );
  void showShot();
}

function renderStatus(): void {
  const output = $<HTMLButtonElement>("output");
  const { route, snapshot } = app;
  const recording = isRecorder(route) && active(route) ? app.rec[route].file : "";
  const text = recording || (route === "home" ? `record ${snapshot?.version ?? ""}` : folderOf(route));
  output.disabled = route === "home";
  output.title = route === "home" ? "" : recording ? "Show in folder" : `Open folder · ${keysOf("openFolder")}`;
  // The text runs right to left so a long path keeps its end. This mark
  // keeps the punctuation of the path in place.
  $("output-text").textContent = `\u200e${text}`;
}

function render(): void {
  const focused = (document.activeElement as HTMLElement | null)?.dataset?.id;
  renderTabs();
  if (app.route === "home") renderHome();
  else if (app.route === "audio") renderAudio();
  else if (app.route === "video") renderVideo();
  else renderScreenshot();
  renderStatus();
  if (app.route === "home") drawArt();
  if (focused && (document.activeElement as HTMLElement | null)?.dataset?.id !== focused) {
    const again = document.querySelector<HTMLElement>(`[data-id="${CSS.escape(focused)}"]`);
    if (again && !(again as HTMLButtonElement).disabled) again.focus();
  }
}

// The drawings of the home cards. The one under the pointer or with the
// keyboard focus moves.
const arts: { id: string; paint: (canvas: HTMLCanvasElement, time?: number) => void }[] = [
  { id: "art-audio", paint: draw.audioArt },
  { id: "art-video", paint: draw.videoArt },
  { id: "art-shot", paint: draw.shotArt },
];
const moving = new Map<string, number>();
const stillness = window.matchMedia("(prefers-reduced-motion: reduce)");

function drawArt(): void {
  for (const art of arts) if (!moving.has(art.id)) art.paint($<HTMLCanvasElement>(art.id));
}

/** Draws what moves: the scopes, the spectrum, the live picture, the clocks. */
function paint(): void {
  requestAnimationFrame(paint);
  if (document.hidden) return;
  const { route } = app;
  if (route === "audio") {
    const running = app.rec.audio.phase === "rec";
    if (!running) app.bands = app.bands.map((band) => band * 0.86);
    draw.scope($<HTMLCanvasElement>("scope-left"), traces.left, draw.token("--cyan"));
    draw.scope($<HTMLCanvasElement>("scope-right"), traces.right, draw.token("--green"));
    draw.spectrum($<HTMLCanvasElement>("spectrum"), app.bands.length ? app.bands : new Array<number>(24).fill(0), draw.token("--purple"));
    draw.meter($("bar-left"), $("db-left"), running ? traces.left.peak(10) : 0);
    draw.meter($("bar-right"), $("db-right"), running ? traces.right.peak(10) : 0);
  } else if (route === "video" && !videoEditorOpen()) {
    const picture = app.video.picture;
    draw.contain($<HTMLCanvasElement>("desktop"), picture, picture?.width ?? 0, picture?.height ?? 0);
  } else if (route === "home" && moving.size && !stillness.matches) {
    const now = performance.now();
    for (const art of arts) {
      const since = moving.get(art.id);
      if (since !== undefined) art.paint($<HTMLCanvasElement>(art.id), (now - since) / 1000 + 0.01);
    }
  }
  const set = (id: string, text: string) => {
    const element = document.getElementById(id);
    if (element && element.textContent !== text) element.textContent = text;
  };
  for (const mode of recorders) {
    if (!active(mode)) continue;
    const text = clock(elapsed(mode));
    set(`clock-${mode}`, text);
    if (app.activityOpen) set(`act-clock-${mode}`, text);
  }
  set("activity-count", activityLabel());
}

// ---- Dialogs. ----

const dialog = $<HTMLDialogElement>("dialog");
let onSubmit: (() => void) | null = null;

function openDialog(title: string, build: (body: HTMLElement) => void, submit: (() => void) | null = null, wide = false): void {
  closeMenus();
  closeActivity();
  $("dialog-title").textContent = title;
  dialog.classList.toggle("wide", wide);
  const body = $("dialog-body");
  body.replaceChildren();
  onSubmit = submit;
  build(body);
  if (!dialog.open) dialog.showModal();
}

function closeButton(label = "Close"): HTMLButtonElement {
  const button = h("button", { className: "button", type: "button", textContent: label });
  button.addEventListener("click", () => dialog.close());
  return button;
}

function openSettings(): void {
  const snapshot = app.snapshot;
  if (!snapshot) return;
  const problem = h("p", { className: "problem" });
  const plain: { name: Setting; label: string; value: string; placeholder?: string }[] = [
    { name: "folder", label: "Screenshot folder", value: snapshot.folder },
    { name: "delaySeconds", label: "Delay (s)", value: number(snapshot.delaySeconds) },
    { name: "intervalSeconds", label: "Auto capture (s)", value: number(snapshot.intervalSeconds) },
    { name: "countdownSeconds", label: "Countdown (s)", value: number(snapshot.countdownSeconds ?? 0) },
    { name: "sdrWhiteNits", label: "HDR white (nits)", value: snapshot.sdrWhiteNits === null ? "" : number(snapshot.sdrWhiteNits), placeholder: "auto" },
  ];
  const fields = plain.map((field) => ({
    ...field,
    input: h("input", { type: "text", value: field.value, placeholder: field.placeholder ?? "", spellcheck: false, id: `setting-${field.name}` }),
  }));
  const format = h("select", { id: "setting-format" }, ...snapshot.formats.map((name) => h("option", { value: name, textContent: name, selected: name === snapshot.format })));
  const save = async () => {
    problem.textContent = "";
    const changes: { name: Setting; label: string; value: string; focus: HTMLElement }[] = fields
      .filter((field) => field.input.value.trim() !== field.value)
      .map((field) => ({ name: field.name, label: field.label, value: field.input.value, focus: field.input }));
    if (format.value !== snapshot.format) changes.push({ name: "format", label: "Format", value: format.value, focus: format });
    for (const one of changes) {
      try {
        app.snapshot = await bridge.setSetting(one.name, one.value);
      } catch (error) {
        problem.textContent = `${one.label}: ${reason(error)}`;
        one.focus.focus();
        render();
        return;
      }
    }
    render();
    dialog.close();
    say("Settings saved", "good");
  };
  const switches = h("div", { className: "rows" });
  const drawSwitches = () => {
    const focused = (document.activeElement as HTMLElement | null)?.dataset?.id;
    switches.replaceChildren();
    const current = app.snapshot;
    if (!current) return;
    const flip = (name: Flag) => async () => {
      try {
        app.snapshot = await bridge.setFlag(name, !current[name]);
        problem.textContent = "";
      } catch (error) {
        problem.textContent = reason(error);
      }
      drawSwitches();
      render();
    };
    const mine = (id: string, name: string, key: "startAudioAtOnce" | "startVideoAtOnce" | "thumbnails") =>
      row(switches, {
        id,
        name,
        dot: app.ui[key],
        action: () => {
          app.ui[key] = !app.ui[key];
          if (key === "thumbnails" && !app.ui.thumbnails) forgetThumbnails();
          saveUi();
          drawSwitches();
          render();
        },
      });
    mine("set-audio-at-once", "Record audio starts at once", "startAudioAtOnce");
    mine("set-video-at-once", "Record video starts at once", "startVideoAtOnce");
    mine("set-thumbnails", "Thumbnails", "thumbnails");
    row(switches, { id: "set-primary", name: "Capture primary monitor", dot: current.primaryMonitor, action: () => void flip("primaryMonitor")() });
    row(switches, { id: "set-tray", name: "Close to tray", dot: current.closeToTray, action: () => void flip("closeToTray")() });
    row(switches, { id: "set-autostart", name: "Start at sign-in", dot: current.autostart, action: () => void flip("autostart")() });
    if (focused) switches.querySelector<HTMLElement>(`[data-id="${CSS.escape(focused)}"]`)?.focus();
  };
  drawSwitches();
  openDialog(
    "Settings",
    (body) => {
      const [folder, ...rest] = fields;
      const line = (label: string, control: HTMLElement) => h("div", { className: "field" }, h("label", { htmlFor: control.id, textContent: label }), control);
      if (folder) body.append(line(folder.label, folder.input));
      body.append(line("Format", format));
      for (const field of rest) body.append(line(field.label, field.input));
      body.append(switches);
      const binds = h("button", { className: "button", type: "button", textContent: "Key binds", id: "settings-binds" });
      binds.addEventListener("click", openKeyBinds);
      const saveButton = h("button", { className: "button save", type: "submit", textContent: "Save" });
      body.append(problem, h("div", { className: "buttons" }, binds, h("span", { className: "spacer" }), closeButton("Cancel"), saveButton));
    },
    () => void save(),
  );
}

function openDestinations(): void {
  let keyFor: string | null = null;
  const problem = h("p", { className: "problem" });
  const apply = async (request: Promise<Snapshot>, done: (snapshot: Snapshot) => string) => {
    try {
      app.snapshot = await request;
      render();
      dialog.close();
      say(done(app.snapshot), "good");
    } catch (error) {
      problem.textContent = reason(error);
    }
  };
  const uploadsTo = (snapshot: Snapshot) => `Destination: ${snapshot.destination ?? ""}`;
  const build = (body: HTMLElement) => {
    const snapshot = app.snapshot;
    if (!snapshot) return;
    const list = h("div", { className: "rows" });
    row(list, { id: "dest-off", name: "Off", value: "no upload", selected: !snapshot.upload, action: () => void apply(bridge.selectDestination(null), () => "Upload off") });
    let submit: (() => void) | null = null;
    for (const destination of snapshot.destinations) {
      row(list, {
        id: `dest-${destination.id}`,
        name: destination.name,
        value: destination.note,
        selected: snapshot.upload && snapshot.destination === destination.name,
        action: () => {
          if (destination.keyLabel) {
            keyFor = destination.id;
            problem.textContent = "";
            openDialog("Upload destination", build);
            document.getElementById("dest-key")?.focus();
          } else void apply(bridge.selectDestination(destination.id), uploadsTo);
        },
      });
      if (destination.keyLabel && keyFor === destination.id) {
        const input = h("input", { type: "text", id: "dest-key", spellcheck: false, placeholder: `${destination.name} ${destination.keyLabel}` });
        const send = () => void apply(bridge.saveKey(destination.id, input.value), uploadsTo);
        const save = h("button", { className: "button save", type: "button", textContent: "Save and use" });
        save.addEventListener("click", send);
        submit = send;
        list.append(h("div", { className: "note" }, h("div", { className: "inline" }, input, save), h("p", { textContent: destination.keySource })));
      }
    }
    const path = h("input", { type: "text", id: "dest-import", spellcheck: false, placeholder: ".sxcu path" });
    const importIt = () => void apply(bridge.importUploader(path.value), uploadsTo);
    const importButton = h("button", { className: "button", type: "button", textContent: "Import" });
    importButton.addEventListener("click", importIt);
    onSubmit = () => (document.activeElement === path ? importIt() : submit?.());
    body.append(
      list,
      h("div", { className: "inline" }, path, importButton),
      problem,
      h("div", { className: "buttons" }, closeButton()),
    );
  };
  openDialog("Upload destination", build);
}

/** Ends the wait for new keys in the Key binds dialog. */
let stopListening: (() => void) | null = null;

/**
 * Every global hotkey and every key of the window. A click on the keys
 * waits for the new ones. Escape leaves them as they are.
 */
function openKeyBinds(): void {
  let problem = "";
  /** The bind that waits for keys: `global:` or `window:` and its name. */
  let waiting = "";
  const listen = (id: string, take: (keys: string) => void) => {
    stopListening?.();
    waiting = id;
    problem = "";
    const onDown = (event: KeyboardEvent) => {
      event.preventDefault();
      event.stopPropagation();
      const keys = comboOf(event);
      if (!keys) return;
      stopListening?.();
      if (keys === "Esc") build();
      else take(keys);
    };
    // Windows sends no key-down for Print Screen, only the key-up.
    const onUp = (event: KeyboardEvent) => {
      if (event.code === "PrintScreen" || event.key === "PrintScreen") onDown(event);
    };
    const onPointer = (event: PointerEvent) => {
      if ((event.target as HTMLElement | null)?.dataset?.id === `bind-${id}`) return;
      stopListening?.();
      build();
    };
    document.addEventListener("keydown", onDown, true);
    document.addEventListener("keyup", onUp, true);
    document.addEventListener("pointerdown", onPointer, true);
    stopListening = () => {
      document.removeEventListener("keydown", onDown, true);
      document.removeEventListener("keyup", onUp, true);
      document.removeEventListener("pointerdown", onPointer, true);
      stopListening = null;
      waiting = "";
    };
    build();
  };
  const keysButton = (id: string, keys: string, take: (keys: string) => void) => {
    const on = waiting === id;
    const button = h("button", { className: `bind${on ? " on" : ""}`, type: "button", textContent: on ? "Press keys" : keys, title: "Change", dataset: { id: `bind-${id}` } });
    button.addEventListener("click", () => {
      if (waiting === id) {
        stopListening?.();
        build();
      } else listen(id, take);
    });
    return button;
  };
  const setGlobal = async (hotkey: HotkeyInfo, keys: string) => {
    const holder = commandFor(keys);
    if (holder) problem = `${keys}: in use by ${commands.find((one) => one.id === holder)?.label ?? holder}`;
    else {
      try {
        app.snapshot = await bridge.setHotkey(hotkey.id, keys);
        app.hotkeyProblem = null;
      } catch (error) {
        problem = `${keys}: ${reason(error)}`;
      }
    }
    render();
    build();
  };
  const setWindow = (command: Command, keys: string) => {
    const global = app.snapshot?.hotkeys.find((one) => one.enabled && sameKeys(one.keys) === sameKeys(keys));
    const holder = global ? null : bind(command, keys);
    if (global) problem = `${keys}: in use by ${global.label}`;
    else if (holder) problem = `${keys}: in use by ${commands.find((one) => one.id === holder)?.label ?? holder}`;
    else saveUi();
    render();
    build();
  };
  const build = () => {
    const focused = (document.activeElement as HTMLElement | null)?.dataset?.id;
    const column = (...children: Child[]) => h("div", { className: "binds" }, ...children);
    const head = (text: string) => h("h3", { textContent: text });
    const global = column(head("Global"));
    for (const hotkey of app.snapshot?.hotkeys ?? []) {
      const taken = hotkey.enabled && !hotkey.registered;
      const toggle = h("button", { className: `dot${taken ? " taken" : hotkey.enabled ? " on" : ""}`, type: "button", textContent: hotkey.enabled ? "●" : "○", title: taken ? "In use" : hotkey.enabled ? "On" : "Off", dataset: { id: `bind-switch-${hotkey.id}` } });
      toggle.setAttribute("role", "switch");
      toggle.setAttribute("aria-checked", String(hotkey.enabled));
      toggle.setAttribute("aria-label", hotkey.label);
      toggle.addEventListener("click", () => {
        void toggleHotkey(hotkey).then((refused) => {
          problem = refused ? `${hotkey.label}: ${refused}` : "";
          build();
        });
      });
      global.append(toggle, h("span", { textContent: hotkey.label }), keysButton(`global:${hotkey.id}`, hotkey.keys, (keys) => void setGlobal(hotkey, keys)));
    }
    const groups = (["Window", "Recording", "Editor"] as const).map((group) => {
      const list = column(head(group));
      for (const command of commands.filter((one) => one.group === group)) {
        list.append(h("span"), h("span", { textContent: command.label }), keysButton(`window:${command.id}`, keysOf(command.id), (keys) => setWindow(command.id, keys)));
      }
      return list;
    });
    const reset = h("button", { className: "button", type: "button", textContent: "Reset to defaults", id: "binds-reset", disabled: !Object.keys(changedBinds()).length });
    reset.addEventListener("click", () => {
      setBinds({});
      problem = "";
      saveUi();
      render();
      build();
    });
    openDialog(
      "Key binds",
      (body) => {
        body.append(
          h("div", { className: "binds-columns" }, h("div", {}, global, groups[0]!), h("div", {}, groups[1]!, groups[2]!)),
          h("p", { className: "problem", textContent: problem }),
          h("div", { className: "buttons" }, reset, h("span", { className: "spacer" }), closeButton()),
        );
      },
      null,
      true,
    );
    if (focused) document.querySelector<HTMLElement>(`#dialog [data-id="${CSS.escape(focused)}"]`)?.focus();
  };
  build();
}

function openAbout(): void {
  openDialog("About record", (body) => {
    body.append(
      h("p", { textContent: `record ${app.snapshot?.version ?? ""}` }),
      h("p", { className: "hint", textContent: "MIT" }),
      h("p", { className: "hint", textContent: "github.com/irbto/record" }),
      h("div", { className: "buttons" }, closeButton()),
    );
  });
}

// ---- Menus and keys. ----

function menuEntries(menu: string): MenuEntry[] {
  const { shots, snapshot } = app;
  const mode = target();
  const shot = (kind: CaptureKind, label: string, key = "", delayed = false): MenuEntry => ({
    label,
    key,
    enabled: !shots.busy && !snapshot?.engineError,
    run: () => void capture(kind, delayed),
  });
  switch (menu) {
    case "File":
      return [
        { label: "Record", key: keysOf("record"), enabled: isRecorder(app.route) && !active(app.route), run: () => isRecorder(app.route) && void startRecording(app.route) },
        { label: mode && app.rec[mode].phase === "paused" ? "Resume" : "Pause", key: keysOf("pause"), enabled: mode !== null && live(mode), run: () => void togglePause(mode) },
        { label: "Clip", key: keysOf("clip"), enabled: live("audio"), run: openClipName },
        { label: "Save and stop", key: keysOf("stop"), enabled: mode !== null && stoppable(mode), run: () => void stopRecording(mode) },
        { label: "Discard", key: keysOf("discard"), enabled: mode !== null && stoppable(mode), run: () => askDiscard(mode) },
        "separator",
        { label: "Open folder", key: keysOf("openFolder"), enabled: app.route !== "home", run: () => void openFolder() },
        "separator",
        { label: "Settings", key: keysOf("settings"), run: openSettings },
        { label: "Key binds", key: keysOf("keyBinds"), run: openKeyBinds },
        "separator",
        { label: "Close window", key: "Alt+F4", run: () => void bridge.closeWindow().catch(fail) },
        { label: "Quit", run: () => void bridge.quit().catch(fail) },
      ];
    case "View":
      return [
        ...(["home", "audio", "video", "screenshot"] as const).map((route): MenuEntry => ({ label: routeNames[route], key: keysOf(route), checked: app.route === route, run: () => go(route) })),
        "separator",
        { label: "Activity", key: keysOf("activity"), checked: app.activityOpen, run: toggleActivity },
      ];
    case "Capture": {
      const delay = number(snapshot?.delaySeconds ?? 3);
      return [
        ...captures.map((item) => shot(item.kind, item.name, hotkeyKeys(item.hotkey))),
        "separator",
        shot("region", `Region after ${delay} s`, "", true),
        shot("fullscreen", `Fullscreen after ${delay} s`, "", true),
        "separator",
        { label: "Auto capture", checked: Boolean(snapshot?.autoActive), enabled: !snapshot?.engineError, run: () => void toggleAuto() },
        "separator",
        { label: "Copy", checked: Boolean(snapshot?.copy), run: () => toggleFlag("copy") },
        { label: "Save", checked: Boolean(snapshot?.save), run: () => toggleFlag("save") },
        { label: "Upload", checked: Boolean(snapshot?.upload), run: () => toggleFlag("upload") },
        { label: "Destination", key: keysOf("destination"), run: openDestinations },
      ];
    }
    default:
      return [
        { label: "Key binds", key: keysOf("keyBinds"), run: openKeyBinds },
        { label: "About record", run: openAbout },
      ];
  }
}

/** What each command of the window does. The editor runs its own. */
const run: Partial<Record<Command, () => void>> = {
  home: () => go("home"),
  audio: () => go("audio"),
  video: () => go("video"),
  screenshot: () => go("screenshot"),
  activity: toggleActivity,
  record: () => isRecorder(app.route) && void startRecording(app.route),
  recordPause: recordOrPause,
  pause: () => void togglePause(),
  stop: () => void stopRecording(),
  discard: () => askDiscard(),
  clip: openClipName,
  openFolder: () => void openFolder(),
  destination: openDestinations,
  settings: openSettings,
  keyBinds: openKeyBinds,
};

/** Keys that the browser underneath would take: print, save, reload, find. */
const browserKeys = new Set(["F5", "F3", "F7", "Ctrl+Shift+R", "Ctrl+R", "Ctrl+P", "Ctrl+S", "Ctrl+O", "Ctrl+D", "Ctrl+J", "Ctrl+F", "Ctrl+G", "Ctrl+U", "Ctrl+H"]);

function onKey(event: KeyboardEvent): void {
  if (event.defaultPrevented) return;
  const keys = comboOf(event);
  if (!keys) return;
  if (browserKeys.has(keys)) event.preventDefault();
  if (keys === "Esc" && app.activityOpen && !dialog.open) {
    closeActivity();
    return;
  }
  const command = commandFor(keys);
  const action = command && run[command];
  if (!action) return;
  const target = event.target instanceof Element ? event.target : null;
  // A key without Ctrl or Alt belongs to the field or the button that has the focus.
  const bare = !event.ctrlKey && !event.altKey && !event.metaKey && !/^(Shift\+)?F\d+$/.test(keys);
  if (bare && target?.closest("input, select, textarea, button, a, dialog, .menubar")) return;
  if (target?.closest("input, select, textarea") && /^(Ctrl\+)?(Shift\+)?(Left|Right|Up|Down|Home|End|Backspace|Delete|[ACVXZY])$/.test(keys)) return;
  event.preventDefault();
  if (!dialog.open) action();
}

// ---- Start. ----

async function start(): Promise<void> {
  for (const tab of document.querySelectorAll<HTMLElement>(".tab")) {
    const route = tab.dataset.route as Route;
    tab.innerHTML = dotIcons[route];
    tab.setAttribute("aria-label", routeNames[route]);
  }
  $("activity").insertAdjacentHTML("afterbegin", dotIcons.activity);
  $("activity").addEventListener("click", toggleActivity);
  setupMenus($("menubar"), ["File", "View", "Capture", "Help"], menuEntries);
  setupEditors({
    bridge,
    h,
    say,
    fail,
    openDialog,
    closeDialog: () => dialog.close(),
    render,
    clipTrimmed: (path, seconds) => {
      const save = app.saves.find((one) => one.path === path);
      if (save) save.seconds = seconds;
      forgetThumbnail(path);
      render();
    },
    videoSaved: (path) => {
      forgetThumbnail(path);
      void refreshFiles("video");
    },
  });
  document.addEventListener("keydown", onKey);
  // A focus outline is for the keyboard. A click leaves none behind.
  document.addEventListener("pointerdown", () => document.documentElement.classList.add("pointer"), true);
  document.addEventListener("keydown", (event) => event.key === "Tab" && document.documentElement.classList.remove("pointer"), true);
  document.addEventListener("pointerdown", (event) => {
    if (app.activityOpen && !(event.target as Element).closest(".activity-wrap")) closeActivity();
  });
  document.addEventListener("contextmenu", (event) => {
    if (!(event.target instanceof HTMLInputElement)) event.preventDefault();
  });
  $("sheet").addEventListener("submit", (event) => {
    event.preventDefault();
    onSubmit?.();
  });
  // A form with several fields does not submit on Enter by itself.
  $("sheet").addEventListener("keydown", (event) => {
    if (event.key === "Enter" && event.target instanceof HTMLInputElement) {
      event.preventDefault();
      onSubmit?.();
    }
  });
  dialog.addEventListener("click", (event) => {
    if (event.target === dialog) dialog.close();
  });
  dialog.addEventListener("close", () => stopListening?.());
  $("output").insertAdjacentHTML("afterbegin", lineIcons.folder);
  $("output").addEventListener("click", openOutput);
  // The home cards: the drawing moves under the pointer and with the focus,
  // and a card of a recorder can start its recording at once.
  for (const choice of document.querySelectorAll<HTMLAnchorElement>(".choice")) {
    const mode = choice.dataset.mode as Mode;
    const id = choice.querySelector("canvas")?.id ?? "";
    const move = () => moving.has(id) || moving.set(id, performance.now());
    const rest = () => {
      if (choice.matches(":hover") || document.activeElement === choice) return;
      moving.delete(id);
      if (app.route === "home") drawArt();
    };
    choice.addEventListener("pointerenter", move);
    choice.addEventListener("focus", move);
    choice.addEventListener("pointerleave", () => window.setTimeout(rest));
    choice.addEventListener("blur", () => window.setTimeout(rest));
    choice.addEventListener("click", () => {
      const atOnce = mode === "audio" ? app.ui.startAudioAtOnce : mode === "video" && app.ui.startVideoAtOnce;
      app.pending = atOnce && isRecorder(mode) && !active(mode) ? mode : null;
    });
  }
  // The window buttons of the title bar.
  const showMaximized = (maximized: boolean) => {
    document.body.classList.toggle("maximized", maximized);
    const label = maximized ? "Restore" : "Maximize";
    $("win-max").title = label;
    $("win-max").setAttribute("aria-label", label);
  };
  $("win-min").addEventListener("click", () => void bridge.minimize().catch(fail));
  $("win-max").addEventListener("click", () => void bridge.toggleMaximize().then(showMaximized, fail));
  $("win-close").addEventListener("click", () => void bridge.closeWindow().catch(fail));
  window.addEventListener("hashchange", () => void enter(readRoute()));
  window.addEventListener("resize", () => {
    // A double-click on the title bar or a snap also changes this state.
    void bridge.isMaximized().then(showMaximized, () => undefined);
    if (app.route === "home") drawArt();
    if (app.route === "screenshot") void showShot();
  });

  bridge.on("sound", onSound);
  bridge.on("video", onVideo);
  bridge.on("shots", onShot);
  bridge.on("hotkeys", (hotkeys) => {
    if (app.snapshot) app.snapshot.hotkeys = hotkeys;
    app.hotkeyProblem = null;
    render();
  });
  bridge.on("closing", () => say("Saving recording"));
  bridge.on("drop", (paths) => {
    const uploader = paths.find((path) => path.toLowerCase().endsWith(".sxcu"));
    if (!uploader) return;
    void change(bridge.importUploader(uploader)).then((done) => {
      if (!done) return;
      if (dialog.open) dialog.close();
      say(`Destination: ${app.snapshot?.destination ?? ""}`, "good");
    });
  });

  try {
    app.ui = readUi(await bridge.uiSettings());
  } catch (error) {
    fail(error);
  }
  try {
    const snapshot = await bridge.snapshot();
    app.snapshot = snapshot;
    // A recording that runs already: the window was opened again, or reloaded.
    for (const mode of snapshot.running ?? []) {
      if (active(mode)) continue;
      app.rec[mode].phase = "rec";
      app.rec[mode].since = performance.now();
    }
  } catch (error) {
    fail(error);
  }
  window.setInterval(() => void pollPicture(), 150);
  requestAnimationFrame(paint);
  await enter(readRoute());
  if (bridge.demo && isRecorder(app.route) && location.search.includes("rec")) await startRecording(app.route);
}

void start();
