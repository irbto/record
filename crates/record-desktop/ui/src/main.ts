// The interface of the record desktop program: a home screen, one screen for
// each of the three modes, a menu bar, and the dialogs.

import { desktopBridge } from "./bridge";
import { demoBridge } from "./demo";
import * as draw from "./draw";
import { closeMenus, setupMenus, type MenuEntry } from "./menu";
import type { CaptureKind, FileEntry, Flag, Mode, Monitor, Route, Setting, ShotUpdate, Snapshot, SoundUpdate, VideoUpdate } from "./types";

const bridge = desktopBridge() ?? demoBridge();
const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

type Phase = "idle" | "starting" | "rec" | "paused" | "saving";
type Recorder = "audio" | "video";

const app = {
  route: "home" as Route,
  snapshot: null as Snapshot | null,
  monitors: [] as Monitor[],
  files: { audio: [], video: [], screenshot: [] } as Record<Mode, FileEntry[]>,
  rec: { mode: null as Recorder | null, phase: "idle" as Phase, facts: "", file: "", seconds: 0, since: 0, banked: 0 },
  video: { monitor: 0, fps: 60, sequence: 0, picture: null as CanvasImageSource | null, width: 0, height: 0, polling: false },
  shots: { busy: false, picked: 0, bitmap: null as ImageBitmap | null, shown: "" },
  bands: [] as number[],
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

function say(text: string, tone: "" | "good" | "bad" = ""): void {
  const message = $("message");
  message.textContent = text;
  message.className = tone;
}

/** Shows the reason of a failed command in the status bar. */
function fail(error: unknown): void {
  say(typeof error === "string" ? error : error instanceof Error ? error.message : String(error), "bad");
}

function clock(seconds: number): string {
  const whole = Math.max(0, Math.floor(seconds));
  const two = (value: number) => String(value).padStart(2, "0");
  return `${two(Math.floor(whole / 3600))}:${two(Math.floor(whole / 60) % 60)}:${two(whole % 60)}`;
}

/** Formats an age with its largest whole unit, as the terminal screen does. */
function age(seconds: number): string {
  if (seconds < 60) return "now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h`;
  return `${Math.floor(seconds / 86400)}d`;
}

const number = (value: number) => String(Number(value.toFixed(2)));

interface RowOptions {
  id: string;
  name: string;
  key?: string;
  selected?: boolean;
  /** A switch: `true` is on, `false` is off. Leave it out for a plain row. */
  dot?: boolean;
  /** Shown but not usable. */
  off?: boolean;
  title?: string;
  action?: () => void;
}

/** Adds one row: a marker, an optional switch, a name, and a value. */
function row(parent: HTMLElement, options: RowOptions): void {
  const { id, name, key = "", selected = false, dot, off = false, title = "", action } = options;
  const marks = [
    h("span", { className: "mark", textContent: "▌" }),
    dot === undefined ? h("span") : h("span", { className: dot ? "dot on" : "dot", textContent: dot ? "●" : "○" }),
    h("span", { className: "name", textContent: name }),
    h("span", { className: "key", textContent: key }),
  ];
  const className = `row${selected ? " sel" : ""}${off ? " off" : ""}`;
  if (!action) {
    parent.append(h("div", { className: `${className} plain`, title }, ...marks));
    return;
  }
  const button = h("button", { className, type: "button", title, disabled: off, dataset: { id } }, ...marks);
  if (dot !== undefined) button.setAttribute("aria-pressed", String(dot));
  button.addEventListener("click", action);
  parent.append(button);
}

function note(parent: HTMLElement, text: string): void {
  parent.append(h("p", { className: "note", textContent: text }));
}

// ---- Routes. ----

const hashes: Record<string, Route> = { audio: "audio", video: "video", screenshot: "screenshot" };

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
  render();
  if (route === "home") await Promise.all((["audio", "video", "screenshot"] as const).map(refreshFiles));
  else await refreshFiles(route);
  if (route === "video" && app.rec.mode !== "video") {
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
}

// ---- Recording. ----

function resetRecording(): void {
  app.rec = { mode: null, phase: "idle", facts: "", file: "", seconds: 0, since: 0, banked: 0 };
  app.video.picture = null;
  app.bands = [];
}

function elapsed(): number {
  const rec = app.rec;
  if (rec.mode === "audio") return rec.seconds;
  return rec.banked + (rec.phase === "rec" ? (performance.now() - rec.since) / 1000 : 0);
}

async function startRecording(): Promise<void> {
  if (app.rec.phase !== "idle") return;
  if (app.route !== "audio" && app.route !== "video") {
    say("Open System audio or Screen capture to start a recording.");
    return;
  }
  const mode = app.route;
  resetRecording();
  app.rec.mode = mode;
  app.rec.phase = "starting";
  traces.left.clear();
  traces.right.clear();
  say("");
  render();
  try {
    app.rec.file = await bridge.startRecording(mode, app.video.monitor, app.video.fps);
    render();
  } catch (error) {
    resetRecording();
    render();
    fail(error);
  }
}

async function togglePause(): Promise<void> {
  const rec = app.rec;
  if (rec.phase !== "rec" && rec.phase !== "paused") return;
  try {
    const paused = await bridge.setPaused(rec.phase === "rec");
    if (rec.phase !== "rec" && rec.phase !== "paused") return;
    if (paused && rec.phase === "rec") rec.banked += (performance.now() - rec.since) / 1000;
    if (!paused) rec.since = performance.now();
    rec.phase = paused ? "paused" : "rec";
    say(paused ? "Paused. The file stays open." : "");
    render();
  } catch (error) {
    fail(error);
  }
}

async function stopRecording(): Promise<void> {
  const rec = app.rec;
  if (rec.phase === "idle" || rec.phase === "saving") return;
  if (rec.phase === "rec") rec.banked += (performance.now() - rec.since) / 1000;
  rec.phase = "saving";
  render();
  try {
    await bridge.stopRecording();
  } catch (error) {
    fail(error);
  }
}

function recordOrPause(): void {
  if (app.rec.phase === "idle") void startRecording();
  else void togglePause();
}

function onStopped(file: string | null, error: string | null): void {
  const mode = app.rec.mode;
  resetRecording();
  if (error) say(`The recording failed: ${error}`, "bad");
  else if (file) say(`Saved ${file}`, "good");
  render();
  if (mode) void refreshFiles(mode);
  if (app.route === "home") void enter("home");
}

function onSound(update: SoundUpdate): void {
  const rec = app.rec;
  switch (update.kind) {
    case "started":
      traces.left.setRate(update.sampleRate);
      traces.right.setRate(update.sampleRate);
      rec.facts = `MP3, ${Math.round(update.bitrate / 1000)} kbps, ${update.sampleRate / 1000} kHz, stereo`;
      if (rec.phase === "starting") rec.phase = "rec";
      render();
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
    case "notice":
      say(update.text);
      break;
    case "stopped":
      onStopped(update.file, update.error);
      break;
  }
}

function onVideo(update: VideoUpdate): void {
  const rec = app.rec;
  switch (update.kind) {
    case "started":
      rec.facts = `${update.width}x${update.height}, ${update.fps} fps, H.264 and AAC`;
      if (rec.phase === "starting") {
        rec.phase = "rec";
        rec.since = performance.now();
      }
      render();
      break;
    case "finalizing":
      rec.phase = "saving";
      render();
      break;
    case "notice":
      say(update.text);
      break;
    case "stopped":
      onStopped(update.file, update.error);
      break;
  }
}

/** Asks for the newest live picture while a video records on screen. */
async function pollPicture(): Promise<void> {
  const video = app.video;
  if (video.polling || app.rec.mode !== "video" || app.rec.phase !== "rec" || app.route !== "video" || document.hidden) return;
  video.polling = true;
  try {
    const frame = await bridge.videoFrame(video.sequence);
    if (frame && app.rec.mode === "video") {
      video.sequence = frame.sequence;
      video.width = frame.width;
      video.height = frame.height;
      video.picture = draw.keepFrame(frame);
      $("desktop-empty").hidden = true;
    }
  } catch {
    // A missed picture is replaced by the next one.
  }
  video.polling = false;
}

// ---- Screenshots. ----

const captures: { kind: CaptureKind; name: string; key: string }[] = [
  { kind: "region", name: "Region", key: "Ctrl+PrtSc" },
  { kind: "fullscreen", name: "Fullscreen", key: "PrtSc" },
  { kind: "window", name: "Active window", key: "Alt+PrtSc" },
  { kind: "monitor", name: "Active monitor", key: "" },
  { kind: "last", name: "Last region", key: "" },
];

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
    case "ready":
      if (app.snapshot) app.snapshot.hotkeys = update.hotkeys;
      break;
    case "capturing":
      shots.busy = true;
      say("Capturing");
      break;
    case "finished": {
      shots.busy = false;
      const parts = [update.file ? `Saved ${update.file}` : "Copied to the clipboard", `${update.width}x${update.height}`];
      if (update.url) parts.splice(0, 2, `Uploaded ${update.url}`, update.copied ? "link copied" : "");
      else if (update.copied && update.file) parts.push("copied");
      if (update.hdrNits !== null) parts.push(`HDR at ${Math.round(update.hdrNits)} nits`);
      if (update.error) parts.unshift(update.error);
      say(parts.filter(Boolean).join(" · "), update.error ? "bad" : "good");
      shots.picked = 0;
      void refreshFiles("screenshot");
      break;
    }
    case "cancelled":
      shots.busy = false;
      say("Cancelled. No file was written.");
      break;
    case "failed":
      shots.busy = false;
      say(`The capture failed: ${update.error}`, "bad");
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
  if (app.route === "home") {
    say("Open one of the three screens first. Each has its own folder.");
    return;
  }
  try {
    await bridge.openFolder(app.route);
  } catch (error) {
    fail(error);
  }
}

// ---- Rendering. ----

const stateLabels: Record<Phase, string> = { idle: "○ Ready", starting: "○ Starting", rec: "● Recording", paused: "● Paused", saving: "○ Saving" };
const modeNames: Record<Recorder, string> = { audio: "System audio", video: "Screen capture" };

function renderTabs(): void {
  const { rec, shots, route, snapshot } = app;
  for (const tab of document.querySelectorAll<HTMLElement>(".tab")) {
    const target = tab.dataset.route as Route;
    tab.setAttribute("aria-selected", String(target === route));
    const busy = target === "screenshot" ? shots.busy || Boolean(snapshot?.autoActive) : target === rec.mode;
    tab.classList.toggle("busy", busy);
  }
  for (const view of document.querySelectorAll<HTMLElement>(".view")) view.classList.toggle("on", view.id === route);
  for (const choice of document.querySelectorAll<HTMLAnchorElement>(".choice")) {
    choice.classList.toggle("busy", rec.mode !== null && choice.hash === `#/${rec.mode}`);
  }
  const now = $<HTMLAnchorElement>("now");
  now.hidden = rec.mode === null || rec.mode === route;
  if (rec.mode) {
    now.href = `#/${rec.mode}`;
    now.classList.toggle("paused", rec.phase !== "rec");
    now.dataset.label = `${stateLabels[rec.phase].toUpperCase()} · ${modeNames[rec.mode]}`;
  }
}

function renderTransport(mode: Recorder): void {
  const rec = app.rec;
  const mine = rec.mode === mode;
  const other = rec.mode !== null && !mine;
  const phase = mine ? rec.phase : "idle";
  const monitor = app.monitors.find((one) => one.index === app.video.monitor);
  const ready =
    mode === "audio"
      ? "MP3, 320 kbps, stereo"
      : monitor
        ? `${monitor.width}x${monitor.height}, ${app.video.fps} fps, H.264 and AAC`
        : `${app.video.fps} fps, H.264 and AAC`;
  const facts = other ? `${modeNames[rec.mode!]} records now. Save it first.` : mine && rec.facts ? rec.facts : ready;
  const live = phase === "rec" || phase === "paused";
  const button = (id: string, className: string, label: string, title: string, enabled: boolean, action: () => void) => {
    const element = h("button", { className: `button ${className}`, type: "button", textContent: label, title, disabled: !enabled, dataset: { id } });
    element.addEventListener("click", action);
    return element;
  };
  $(`transport-${mode}`).replaceChildren(
    h("span", { className: `state ${phase}`, textContent: stateLabels[phase].toUpperCase() }),
    h("span", { className: "clock", id: `clock-${mode}`, textContent: clock(mine ? elapsed() : 0) }),
    h("span", { className: "facts", textContent: facts }),
    h("span", { className: "spacer" }),
    button(`record-${mode}`, "go", "● Record", "Start recording (Ctrl+R)", phase === "idle" && !other, () => void startRecording()),
    button(`pause-${mode}`, "", phase === "paused" ? "Resume" : "Pause", "Pause or resume (Ctrl+P)", live, () => void togglePause()),
    button(`stop-${mode}`, "save", "■ Save and stop", "Save the file and stop (Ctrl+S)", live || phase === "starting", () => void stopRecording()),
  );
}

function fileRows(parent: HTMLElement, mode: Mode, empty: string): void {
  parent.replaceChildren();
  const files = app.files[mode];
  for (const file of files) {
    row(parent, {
      id: `file-${file.path}`,
      name: file.name,
      key: age(file.ageSeconds),
      title: "Show in the folder",
      action: () => void bridge.reveal(file.path).catch(fail),
    });
  }
  if (!files.length) note(parent, empty);
}

function renderHome(): void {
  const recent = $("recent");
  recent.replaceChildren();
  const all = (["audio", "video", "screenshot"] as const)
    .flatMap((mode) => app.files[mode])
    .sort((first, second) => first.ageSeconds - second.ageSeconds)
    .slice(0, 30);
  for (const file of all) {
    row(recent, { id: `recent-${file.path}`, name: file.name, key: age(file.ageSeconds), title: "Open the file", action: () => void bridge.openFile(file.path).catch(fail) });
  }
  if (!all.length) note(recent, "Nothing yet. Recordings and screenshots appear here.");

  const hotkeys = $("hotkeys");
  hotkeys.replaceChildren();
  const snapshot = app.snapshot;
  if (snapshot?.engineError) {
    note(hotkeys, `The screenshot engine did not start: ${snapshot.engineError}`);
    return;
  }
  captures.slice(0, 3).forEach((item, index) => {
    const registered = snapshot?.hotkeys[index] ?? false;
    row(hotkeys, {
      id: `hotkey-${item.kind}`,
      name: `${item.key}  ${item.name}`,
      dot: registered,
      key: registered ? "ready" : "used by another program",
    });
  });
  note(hotkeys, "They work in every program while this window is open, also when it is minimized.");
}

function renderAudio(): void {
  renderTransport("audio");
  $("audio-files-title").textContent = `Recordings · ${app.files.audio.length}`;
  fileRows($("audio-files"), "audio", "No recordings yet. Record saves an MP3 file here.");
}

function renderVideo(): void {
  renderTransport("video");
  const { video, rec } = app;
  const idle = rec.phase === "idle";
  const sources = $("sources");
  sources.replaceChildren();
  for (const monitor of app.monitors) {
    row(sources, {
      id: `monitor-${monitor.index}`,
      name: `Display ${monitor.index + 1} · ${monitor.width}x${monitor.height}`,
      key: monitor.primary ? "primary" : "",
      selected: monitor.index === video.monitor,
      off: !idle && monitor.index !== video.monitor,
      action: () => {
        if (app.rec.phase !== "idle") return;
        video.monitor = monitor.index;
        render();
      },
    });
  }
  if (!app.monitors.length) note(sources, "Looking for monitors");
  const settings = $("video-settings");
  settings.replaceChildren();
  row(settings, {
    id: "fps",
    name: "Frame rate",
    key: `${video.fps} fps`,
    off: !idle,
    title: "Switch between 30 and 60 frames per second",
    action: () => {
      video.fps = video.fps === 60 ? 30 : 60;
      render();
    },
  });
  row(settings, { id: "video-codec", name: "Video", key: "H.264 · 20 Mbps" });
  row(settings, { id: "audio-codec", name: "Audio", key: "AAC · 192 kbps" });
  $("picture-title").textContent = `Preview · Display ${video.monitor + 1}`;
  $("video-files-title").textContent = `Recordings · ${app.files.video.length}`;
  fileRows($("video-files"), "video", "No recordings yet. Record saves an MP4 file here.");
  const empty = $("desktop-empty");
  const mine = rec.mode === "video";
  empty.hidden = mine && video.picture !== null;
  empty.textContent = mine ? (rec.phase === "saving" ? "Saving the file" : "Starting the capture") : "Record starts the capture. The live picture appears here.";
}

function renderScreenshot(): void {
  const snapshot = app.snapshot;
  const { shots } = app;
  const menu = $("capture-menu");
  menu.replaceChildren();
  for (const item of captures) {
    row(menu, { id: `capture-${item.kind}`, name: item.name, key: item.key, off: shots.busy, action: () => void capture(item.kind) });
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
  if (snapshot?.engineError) note(menu, `The screenshot engine did not start: ${snapshot.engineError}`);

  const after = $("after-menu");
  after.replaceChildren();
  if (snapshot) {
    row(after, { id: "flag-copy", name: "Copy to the clipboard", dot: snapshot.copy, action: () => toggleFlag("copy") });
    row(after, { id: "flag-save", name: "Save a PNG file", dot: snapshot.save, action: () => toggleFlag("save") });
    row(after, { id: "flag-upload", name: "Upload and copy the link", dot: snapshot.upload, action: () => toggleFlag("upload") });
    row(after, { id: "destination", name: "Upload destination", key: snapshot.destination ?? "none", title: "Choose where uploads go (Ctrl+D)", action: openDestinations });
  }

  const history = $("history");
  history.replaceChildren();
  app.files.screenshot.forEach((file, index) => {
    row(history, {
      id: `shot-${file.path}`,
      name: file.name,
      key: age(file.ageSeconds),
      selected: index === shots.picked,
      action: () => {
        shots.picked = index;
        render();
      },
    });
  });
  if (!app.files.screenshot.length) note(history, "No screenshots in this folder yet.");
  $("history-title").textContent = `History · ${app.files.screenshot.length}`;
  const file = pickedShot();
  $("preview-title").textContent = file?.name ?? "Preview";
  $("shot-empty").textContent = "Take a screenshot. The newest one appears here.";
  const action = (id: string, label: string, run: () => void) => {
    const element = h("button", { className: "button", type: "button", textContent: label, disabled: !file, dataset: { id } });
    element.addEventListener("click", run);
    return element;
  };
  $("shot-actions").replaceChildren(
    action("shot-copy", "Copy image", () => void withShot(bridge.copyPicture, "Copied {} to the clipboard")),
    action("shot-reveal", "Show in folder", () => void withShot(bridge.reveal, "")),
    action("shot-open", "Open", () => void withShot(bridge.openFile, "")),
  );
  void showShot();
}

function renderStatus(): void {
  const output = $<HTMLButtonElement>("output");
  const { rec, route, snapshot } = app;
  const recording = rec.mode !== null && rec.mode === route && rec.file;
  output.textContent = recording ? rec.file : route === "home" ? `record ${snapshot?.version ?? ""}` : folderOf(route);
  output.disabled = route === "home";
  // The text runs right to left so a long path keeps its end. This mark
  // keeps the punctuation of the path in place.
  output.textContent = `\u200e${output.textContent}`;
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
  if (focused) {
    const again = document.querySelector<HTMLElement>(`[data-id="${CSS.escape(focused)}"]`);
    if (again && !(again as HTMLButtonElement).disabled) again.focus();
  }
}

function drawArt(): void {
  draw.audioArt($<HTMLCanvasElement>("art-audio"));
  draw.videoArt($<HTMLCanvasElement>("art-video"));
  draw.shotArt($<HTMLCanvasElement>("art-shot"));
}

/** Draws what moves: the scopes, the spectrum, the live picture, the clock. */
function paint(): void {
  requestAnimationFrame(paint);
  if (document.hidden) return;
  const { rec, route } = app;
  if (route === "audio") {
    const live = rec.mode === "audio" && rec.phase === "rec";
    if (!live) app.bands = app.bands.map((band) => band * 0.86);
    draw.scope($<HTMLCanvasElement>("scope-left"), traces.left, draw.token("--cyan"));
    draw.scope($<HTMLCanvasElement>("scope-right"), traces.right, draw.token("--green"));
    draw.spectrum($<HTMLCanvasElement>("spectrum"), app.bands.length ? app.bands : new Array<number>(24).fill(0), draw.token("--purple"));
    draw.meter($("bar-left"), $("db-left"), live ? traces.left.peak(10) : 0);
    draw.meter($("bar-right"), $("db-right"), live ? traces.right.peak(10) : 0);
  } else if (route === "video") {
    const video = app.video;
    draw.contain($<HTMLCanvasElement>("desktop"), rec.mode === "video" ? video.picture : null, video.width, video.height);
  }
  if (rec.mode) {
    const text = clock(elapsed());
    const own = document.getElementById(`clock-${rec.mode}`);
    if (own && own.textContent !== text) own.textContent = text;
    const now = $("now");
    const label = `${now.dataset.label ?? ""} · ${text}`;
    if (!now.hidden && now.textContent !== label) now.textContent = label;
  }
}

// ---- Dialogs. ----

const dialog = $<HTMLDialogElement>("dialog");
let onSubmit: (() => void) | null = null;

function openDialog(title: string, build: (body: HTMLElement) => void, submit: (() => void) | null = null): void {
  closeMenus();
  $("dialog-title").textContent = title;
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
  const plain: { name: Setting; label: string; hint: string; value: string }[] = [
    { name: "folder", label: "Screenshot folder", hint: "PNG files go here.", value: snapshot.folder },
    { name: "delaySeconds", label: "Delay, seconds", hint: "The wait of a delayed capture.", value: number(snapshot.delaySeconds) },
    { name: "intervalSeconds", label: "Auto capture, seconds", hint: "The time between two auto captures.", value: number(snapshot.intervalSeconds) },
    { name: "sdrWhiteNits", label: "HDR white level, nits", hint: "Leave it empty to use the value from Windows.", value: snapshot.sdrWhiteNits === null ? "" : number(snapshot.sdrWhiteNits) },
  ];
  const fields = plain.map((field) => ({
    ...field,
    input: h("input", { type: "text", value: field.value, spellcheck: false, id: `setting-${field.name}` }),
  }));
  const save = async () => {
    problem.textContent = "";
    for (const field of fields) {
      if (field.input.value.trim() === field.value) continue;
      try {
        app.snapshot = await bridge.setSetting(field.name, field.input.value);
      } catch (error) {
        problem.textContent = `${field.label}: ${String(error)}`;
        field.input.focus();
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
    switches.replaceChildren();
    const current = app.snapshot;
    if (!current) return;
    const flip = (name: Flag) => async () => {
      try {
        app.snapshot = await bridge.setFlag(name, !current[name]);
        problem.textContent = "";
      } catch (error) {
        problem.textContent = String(error);
      }
      drawSwitches();
      render();
    };
    row(switches, { id: "set-primary", name: "Always capture the primary monitor", dot: current.primaryMonitor, title: "Off captures the monitor under the pointer", action: () => void flip("primaryMonitor")() });
    row(switches, { id: "set-autostart", name: "Start the screenshot tray at sign-in", dot: current.autostart, title: "Runs record screenshot --tray when you sign in", action: () => void flip("autostart")() });
  };
  drawSwitches();
  openDialog(
    "Settings",
    (body) => {
      for (const field of fields) {
        body.append(
          h("div", { className: "field" }, h("label", { htmlFor: field.input.id, textContent: field.label }), field.input, h("small", { textContent: field.hint })),
        );
      }
      body.append(switches);
      body.append(h("p", { className: "hint", textContent: `Audio goes to ${snapshot.audioFolder}. Video goes to ${snapshot.videoFolder}.` }));
      const saveButton = h("button", { className: "button save", type: "submit", textContent: "Save" });
      body.append(problem, h("div", { className: "buttons" }, closeButton("Cancel"), saveButton));
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
      problem.textContent = String(error);
    }
  };
  const uploadsTo = (snapshot: Snapshot) => `Uploads go to ${snapshot.destination ?? "the new destination"}`;
  const build = (body: HTMLElement) => {
    const snapshot = app.snapshot;
    if (!snapshot) return;
    const list = h("div", { className: "rows" });
    row(list, { id: "dest-off", name: "Off", key: "no upload", selected: !snapshot.upload, action: () => void apply(bridge.selectDestination(null), () => "Uploads are off") });
    let submit: (() => void) | null = null;
    for (const destination of snapshot.destinations) {
      row(list, {
        id: `dest-${destination.id}`,
        name: destination.name,
        key: destination.note,
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
        list.append(h("div", { className: "note" }, h("div", { className: "inline" }, input, save), h("p", { textContent: `Get the ${destination.keyLabel} at ${destination.keySource}. It stays on this PC.` })));
      }
    }
    const path = h("input", { type: "text", id: "dest-import", spellcheck: false, placeholder: "C:\\path\\to\\uploader.sxcu" });
    const importIt = () => void apply(bridge.importUploader(path.value), uploadsTo);
    const importButton = h("button", { className: "button", type: "button", textContent: "Import" });
    importButton.addEventListener("click", importIt);
    onSubmit = () => (document.activeElement === path ? importIt() : submit?.());
    body.append(
      list,
      h("p", { className: "hint", textContent: "Any ShareX custom uploader works. Enter the path of a .sxcu file, or drop the file on this window." }),
      h("div", { className: "inline" }, path, importButton),
      problem,
      h("div", { className: "buttons" }, closeButton()),
    );
  };
  openDialog("Upload destination", build);
}

function openShortcuts(): void {
  const groups: [string, [string, string][]][] = [
    ["In this window", [
      ["Ctrl+0 to Ctrl+3", "Home, System audio, Screen capture, Screenshot"],
      ["Ctrl+R", "Start recording"],
      ["Space", "Start, pause, or resume"],
      ["Ctrl+P", "Pause or resume"],
      ["Ctrl+S", "Save and stop"],
      ["Ctrl+O", "Open the output folder"],
      ["Ctrl+D", "Upload destination"],
      ["Ctrl+,", "Settings"],
      ["Alt+F, V, C, H", "Open a menu"],
    ]],
    ["In every program", [
      ["Ctrl+PrtSc", "Capture a region"],
      ["PrtSc", "Capture all screens"],
      ["Alt+PrtSc", "Capture the active window"],
    ]],
  ];
  openDialog("Keyboard shortcuts", (body) => {
    const list = h("div", { className: "keys-list" });
    for (const [title, keys] of groups) {
      list.append(h("h3", { textContent: title }));
      for (const [key, what] of keys) list.append(h("kbd", { textContent: key }), h("span", { textContent: what }));
    }
    body.append(list, h("div", { className: "buttons" }, closeButton()));
  });
}

function openAbout(): void {
  openDialog("About record", (body) => {
    body.append(
      h("p", { textContent: `record ${app.snapshot?.version ?? ""}` }),
      h("p", { className: "hint", textContent: "System audio to MP3, the screen to MP4, and screenshots that stay correct on HDR monitors. One small program, MIT license." }),
      h("p", { className: "hint", textContent: "github.com/irbto/record" }),
      h("div", { className: "buttons" }, closeButton()),
    );
  });
}

// ---- Menus and keys. ----

function menuEntries(menu: string): MenuEntry[] {
  const { rec, shots, snapshot } = app;
  const live = rec.phase === "rec" || rec.phase === "paused";
  const onRecorder = app.route === "audio" || app.route === "video";
  const shot = (kind: CaptureKind, label: string, key = "", delayed = false): MenuEntry => ({
    label,
    key,
    enabled: !shots.busy && !snapshot?.engineError,
    run: () => void capture(kind, delayed),
  });
  switch (menu) {
    case "File":
      return [
        { label: "Start recording", key: "Ctrl+R", enabled: rec.phase === "idle" && onRecorder, run: () => void startRecording() },
        { label: rec.phase === "paused" ? "Resume" : "Pause", key: "Ctrl+P", enabled: live, run: () => void togglePause() },
        { label: "Save and stop", key: "Ctrl+S", enabled: live || rec.phase === "starting", run: () => void stopRecording() },
        "separator",
        { label: "Open the output folder", key: "Ctrl+O", enabled: app.route !== "home", run: () => void openFolder() },
        "separator",
        { label: "Settings", key: "Ctrl+,", run: openSettings },
        "separator",
        { label: "Exit", key: "Alt+F4", run: () => void bridge.quit() },
      ];
    case "View":
      return [
        { label: "Home", key: "Ctrl+0", checked: app.route === "home", run: () => go("home") },
        { label: "System audio", key: "Ctrl+1", checked: app.route === "audio", run: () => go("audio") },
        { label: "Screen capture", key: "Ctrl+2", checked: app.route === "video", run: () => go("video") },
        { label: "Screenshot", key: "Ctrl+3", checked: app.route === "screenshot", run: () => go("screenshot") },
      ];
    case "Capture": {
      const delay = number(snapshot?.delaySeconds ?? 3);
      return [
        ...captures.map((item) => shot(item.kind, item.name, item.key)),
        "separator",
        shot("region", `Region after ${delay} s`, "", true),
        shot("fullscreen", `Fullscreen after ${delay} s`, "", true),
        "separator",
        { label: "Auto capture", checked: Boolean(snapshot?.autoActive), enabled: !snapshot?.engineError, run: () => void toggleAuto() },
        "separator",
        { label: "Copy to the clipboard", checked: Boolean(snapshot?.copy), run: () => toggleFlag("copy") },
        { label: "Save a PNG file", checked: Boolean(snapshot?.save), run: () => toggleFlag("save") },
        { label: "Upload and copy the link", checked: Boolean(snapshot?.upload), run: () => toggleFlag("upload") },
        { label: "Upload destination", key: "Ctrl+D", run: openDestinations },
      ];
    }
    default:
      return [
        { label: "Keyboard shortcuts", key: "F1", run: openShortcuts },
        { label: "About record", run: openAbout },
      ];
  }
}

const shortcuts: Record<string, () => void> = {
  "0": () => go("home"),
  "1": () => go("audio"),
  "2": () => go("video"),
  "3": () => go("screenshot"),
  r: () => void startRecording(),
  p: () => void togglePause(),
  s: () => void stopRecording(),
  o: () => void openFolder(),
  d: openDestinations,
  ",": openSettings,
};

function onKey(event: KeyboardEvent): void {
  const key = event.key.toLowerCase();
  if (key === "f5" || (event.ctrlKey && event.shiftKey && key === "r")) {
    event.preventDefault();
    return;
  }
  if (key === "f1") {
    event.preventDefault();
    if (!dialog.open) openShortcuts();
    return;
  }
  if (event.ctrlKey && !event.altKey && !event.shiftKey) {
    const command = shortcuts[key];
    if (command) {
      // The browser underneath would print, save, or reload otherwise.
      event.preventDefault();
      if (!dialog.open) command();
    }
    return;
  }
  const target = event.target as HTMLElement | null;
  const typing = target instanceof Element && target.closest("input, button, a, dialog, .menubar") !== null;
  if (key === " " && !typing && (app.route === "audio" || app.route === "video")) {
    event.preventDefault();
    recordOrPause();
  }
}

// ---- Start. ----

async function start(): Promise<void> {
  setupMenus($("menubar"), ["File", "View", "Capture", "Help"], menuEntries);
  document.addEventListener("keydown", onKey);
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
  $("output").addEventListener("click", () => void openFolder());
  // The window buttons of the title bar.
  const showMaximized = (maximized: boolean) => {
    document.body.classList.toggle("maximized", maximized);
    const label = maximized ? "Restore" : "Maximize";
    $("win-max").title = label;
    $("win-max").setAttribute("aria-label", label);
  };
  $("win-min").addEventListener("click", () => void bridge.minimize().catch(fail));
  $("win-max").addEventListener("click", () => void bridge.toggleMaximize().then(showMaximized, fail));
  $("win-close").addEventListener("click", () => void bridge.quit().catch(fail));
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
  bridge.on("closing", () => say("Saving the recording. The window closes when the file is ready."));
  bridge.on("drop", (paths) => {
    const uploader = paths.find((path) => path.toLowerCase().endsWith(".sxcu"));
    if (!uploader) {
      say("Drop a .sxcu file to add an upload destination.");
      return;
    }
    void change(bridge.importUploader(uploader)).then((done) => {
      if (!done) return;
      if (dialog.open) dialog.close();
      say(`Uploads go to ${app.snapshot?.destination ?? "the new destination"}`, "good");
    });
  });

  try {
    app.snapshot = await bridge.snapshot();
  } catch (error) {
    fail(error);
  }
  window.setInterval(() => void pollPicture(), 125);
  requestAnimationFrame(paint);
  await enter(readRoute());
  if (bridge.demo) {
    say("Preview with a simulated signal. Nothing is recorded.");
    if (location.search.includes("rec")) await startRecording();
  }
}

void start();
