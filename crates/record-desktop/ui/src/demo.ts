// A simulation of the engines for a plain browser. It lets the interface be
// viewed and tested without Windows. Nothing is recorded.

import { standInDesktop, standInSample } from "./draw";
import { comboOf } from "./keys";
import type { Bridge, CaptureKind, Events, FileEntry, HotkeyId, HotkeyInfo, Mode, Recorder, Region, Snapshot, VideoInfo } from "./types";

declare global {
  interface Window {
    /** Presses one global hotkey of the simulation. */
    demoHotkey?: (id: HotkeyId) => void;
    /** Makes the next hotkey start, or the next discard, fail. */
    demoFail?: "start" | "discard" | null;
  }
}

const two = (value: number) => String(value).padStart(2, "0");

function stamp(): string {
  const now = new Date();
  return (
    `${now.getFullYear()}${two(now.getMonth() + 1)}${two(now.getDate())}` +
    `-${two(now.getHours())}${two(now.getMinutes())}${two(now.getSeconds())}`
  );
}

const folders: Record<Mode, string> = {
  audio: "C:\\Users\\you\\Music\\record",
  video: "C:\\Users\\you\\Videos\\record",
  screenshot: "C:\\Users\\you\\Pictures\\Screenshots",
};

/** One listed file. The time comes from the stamp in the name, or it is now. */
function entry(mode: Mode, name: string): FileEntry {
  const at = /(\d{4})(\d\d)(\d\d)-(\d\d)(\d\d)(\d\d)\.\w+$/.exec(name);
  const [year, month, day, hour, minute, second] = (at?.slice(1) ?? []).map(Number);
  const modifiedMs = at ? new Date(year!, month! - 1, day, hour, minute, second).getTime() : Date.now();
  // A size that looks like one of this kind of file and stays the same.
  const seed = [...name].reduce((sum, letter) => (sum * 31 + letter.charCodeAt(0)) % 9973, 7) / 9973;
  const sizeBytes = Math.round((mode === "video" ? 18e6 + seed * 140e6 : mode === "audio" ? 2e6 + seed * 30e6 : 3e5 + seed * 2.4e6));
  return { name, path: `${folders[mode]}\\${name}`, ageSeconds: 0, modifiedMs, sizeBytes };
}

/** One recording of the simulation. */
interface Session {
  mode: Recorder;
  name: string;
  path: string;
  timer: number;
  wait: number;
  frames: number;
  paused: boolean;
  started: boolean;
  parts: number;
  /** The files of the session that are listed already. */
  written: string[];
}

export function demoBridge(): Bridge {
  const handlers: { [K in keyof Events]?: ((payload: Events[K]) => void)[] } = {};
  const emit = <K extends keyof Events>(event: K, payload: Events[K]) => {
    for (const handler of handlers[event] ?? []) handler(payload);
  };

  const state: Snapshot = {
    copy: true,
    save: true,
    upload: false,
    destination: "Catbox",
    destinations: [
      { id: "imgur", name: "Imgur", note: "needs a client ID", keyLabel: "client ID", keySource: "api.imgur.com/oauth2/addclient" },
      { id: "imgbb", name: "ImgBB", note: "needs an API key", keyLabel: "API key", keySource: "api.imgbb.com" },
      { id: "catbox", name: "Catbox", note: "no account", keyLabel: null, keySource: "" },
      { id: "litterbox", name: "Litterbox", note: "no account, deleted after 72 hours", keyLabel: null, keySource: "" },
    ],
    folder: folders.screenshot,
    format: "PNG",
    formats: ["PNG", "JPEG", "WebP", "BMP", "GIF", "TIFF"],
    closeToTray: true,
    audioFolder: folders.audio,
    videoFolder: folders.video,
    delaySeconds: 3,
    intervalSeconds: 5,
    primaryMonitor: false,
    sdrWhiteNits: null,
    autostart: false,
    hotkeys: [
      { id: "region", label: "Screenshot region", keys: "Ctrl+PrtSc", enabled: true, registered: true },
      { id: "fullscreen", label: "Screenshot fullscreen", keys: "PrtSc", enabled: true, registered: true },
      { id: "window", label: "Screenshot window", keys: "Alt+PrtSc", enabled: true, registered: true },
      { id: "videoRegion", label: "Record region", keys: "Ctrl+Shift+F8", enabled: true, registered: true },
      { id: "videoFullscreen", label: "Record fullscreen", keys: "Ctrl+Shift+F9", enabled: true, registered: true },
    ],
    countdownSeconds: 0,
    running: [],
    autoActive: false,
    engineError: null,
    version: "0.3.3",
  };
  const files: Record<Mode, FileEntry[]> = {
    audio: [
      entry("audio", "recording-20261116-162210.mp3"),
      entry("audio", "recording-20261114-201955.mp3"),
    ],
    video: [
      entry("video", "recording-20261116-174455.mp4"),
      entry("video", "recording-20261115-203310.mp4"),
      entry("video", "recording-20261113-110802.mp4"),
    ],
    screenshot: [
      entry("screenshot", "screenshot-20261116-184210.png"),
      entry("screenshot", "screenshot-20261116-181455.png"),
      entry("screenshot", "screenshot-20261116-173002.png"),
      entry("screenshot", "screenshot-20261115-221940.png"),
      entry("screenshot", "screenshot-20261115-140313.png"),
      entry("screenshot", "screenshot-20261114-095127.png"),
    ],
  };

  const running: Partial<Record<Recorder, Session>> = {};
  let uiSettings = "{}";
  let signal = 21.4;
  let autoTimer = 0;
  let maximized = false;
  let playTimer = 0;
  let clipCount = 0;
  let editing: VideoInfo | null = null;
  const monitors = [
    { index: 0, width: 2560, height: 1440, primary: false, left: -2560, top: 0 },
    { index: 1, width: 1920, height: 1080, primary: true, left: 0, top: 0 },
  ];
  const wave = (bins: number, seed: number): [number, number][] =>
    Array.from({ length: bins }, (_, index) => {
      const level = Math.abs(standInSample(index / 14 + seed, seed)) * 0.8;
      return [-level, level];
    });
  const picture = (shape: number, time: number) => {
    desktop.height = Math.round(desktop.width / shape);
    standInDesktop(desktop.getContext("2d")!, desktop.width, desktop.height, time);
    return new Promise<Blob>((resolve, reject) => {
      desktop.toBlob((blob) => (blob ? resolve(blob) : reject("The preview is empty")), "image/jpeg", 0.8);
    });
  };
  const desktop = document.createElement("canvas");
  desktop.width = 640;
  desktop.height = 360;

  const finishCapture = () => {
    const extension = state.format === "JPEG" ? "jpg" : state.format.toLowerCase();
    const name = `screenshot-${stamp()}.${extension}`;
    if (state.save) files.screenshot.unshift(entry("screenshot", name));
    emit("shots", {
      kind: "finished",
      file: state.save ? name : null,
      path: state.save ? `${folders.screenshot}\\${name}` : null,
      width: 2560,
      height: 1440,
      url: state.upload ? "https://files.catbox.moe/k3v9qd.png" : null,
      copied: state.copy || state.upload,
      hdrNits: 240,
      error: null,
    });
  };

  const snapshot = async () => structuredClone({ ...state, running: (["audio", "video"] as const).filter((mode) => running[mode]?.started) });

  /** Starts one recording, as a command or a hotkey does. */
  const begin = (mode: Recorder, monitor = 1, fps = 60, region: Region | null = null): string => {
    if (running[mode]) throw mode === "audio" ? "An audio recording runs already" : "A video recording runs already";
    const name = mode === "audio" ? `recording-${stamp()}` : `recording-${stamp()}.mp4`;
    const session: Session = { mode, name, path: `${folders[mode]}\\${name}`, timer: 0, wait: 0, frames: 0, paused: false, started: false, parts: 0, written: [] };
    running[mode] = session;
    let left = Math.round(state.countdownSeconds);
    if (mode === "audio") emit("sound", { kind: "starting", path: session.path, countdown: left });
    else emit("video", { kind: "starting", path: session.path, countdown: left, monitor, region });
    const launch = () => {
      session.started = true;
      const screen = region ?? monitors.find((one) => one.index === monitor) ?? monitors[1]!;
      if (mode === "audio") emit("sound", { kind: "started", sampleRate: 48000, bitrate: 320000 });
      else emit("video", { kind: "started", width: screen.width, height: screen.height, fps });
      session.timer = window.setInterval(() => {
        if (session.paused) return;
        if (mode !== "audio") return;
        signal += 1024 / 48000;
        session.frames += 1024;
        const extent = (ear: number): [number, number] => {
          let low = 0;
          let high = 0;
          for (let step = 0; step < 8; step++) {
            const value = standInSample(signal + step * 0.0026, ear) * 0.62;
            low = Math.min(low, value);
            high = Math.max(high, value);
          }
          return [low, high];
        };
        const left = extent(0);
        const right = extent(1);
        const level = Math.max(-left[0], left[1], -right[0], right[1]);
        const bands = Array.from({ length: 24 }, (_, band) => {
          const tilt = Math.exp(-(((band - 6) / 7) ** 2)) * 0.9 + Math.exp(-(((band - 16) / 3.2) ** 2)) * 0.5;
          return Math.min(1, level * tilt * (0.6 + 0.4 * Math.abs(standInSample(signal * 9 + band, band))) * 1.2);
        });
        emit("sound", { kind: "block", left, right, seconds: session.frames / 48000, bands });
      }, 1000 / 47);
    };
    if (left > 0) {
      session.wait = window.setInterval(() => {
        left -= 1;
        if (left > 0) {
          emit(mode === "audio" ? "sound" : "video", { kind: "countdown", seconds: left });
          return;
        }
        window.clearInterval(session.wait);
        launch();
      }, 1000);
    } else session.wait = window.setTimeout(launch, 120);
    return session.path;
  };

  /** Ends one recording: saved, or with everything it wrote deleted. */
  const end = (mode: Recorder, discard: boolean) => {
    const session = running[mode];
    if (!session) return;
    const event = mode === "audio" ? "sound" : "video";
    window.clearInterval(session.timer);
    window.clearInterval(session.wait);
    window.clearTimeout(session.wait);
    if (discard && session.started && window.demoFail === "discard") {
      // The files stay when the delete fails.
      window.demoFail = null;
      delete running[mode];
      if (mode === "video") files.video.unshift(entry("video", session.name));
      emit(event, { kind: "stopped", file: session.name, path: session.path, error: "the file is open in another program", discarded: false });
      return;
    }
    if (discard || !session.started) {
      delete running[mode];
      files[mode] = files[mode].filter((file) => !session.written.includes(file.path));
      emit(event, { kind: "stopped", file: null, path: null, error: null, discarded: true });
      return;
    }
    emit(event, { kind: "finalizing" });
    window.setTimeout(() => {
      if (running[mode] !== session) return;
      delete running[mode];
      if (mode === "audio") {
        session.parts += 1;
        const part = `part-${String(session.parts).padStart(3, "0")}.mp3`;
        files.audio.unshift(entry("audio", `${session.name}\\${part}`));
        emit("sound", { kind: "saved", name: part, path: `${session.path}\\${part}`, clip: false, seconds: session.frames / 48000, editable: false });
      } else files.video.unshift(entry("video", session.name));
      emit(event, { kind: "stopped", file: session.name, path: session.path, error: null, discarded: false });
    }, 300);
  };

  const demoRegion: Region = { left: 320, top: 180, width: 1280, height: 720 };
  /** What a global hotkey does. The region selector answers after a moment. */
  const press = (id: HotkeyId) => {
    if (id === "videoRegion" || id === "videoFullscreen") {
      if (running.video) end("video", false);
      else if (window.demoFail === "start") {
        // A failed start from a hotkey comes without a `starting` event.
        window.demoFail = null;
        emit("video", { kind: "stopped", file: null, path: null, error: "the screen cannot be captured", discarded: false });
      }
      else if (id === "videoFullscreen") begin("video", 1, 60, null);
      else window.setTimeout(() => running.video || begin("video", 1, 60, demoRegion), 300);
      return;
    }
    emit("shots", { kind: "capturing" });
    window.setTimeout(finishCapture, 450);
  };
  window.demoHotkey = press;
  window.addEventListener("keydown", (event) => {
    if (document.querySelector("dialog[open]")) return;
    const keys = comboOf(event)?.toLowerCase();
    const hotkey = state.hotkeys.find((one) => one.enabled && one.registered && one.keys.toLowerCase() === keys);
    if (!hotkey) return;
    event.preventDefault();
    press(hotkey.id);
  });
  /** Keys that another program holds in the simulation. */
  const held = ["ctrl+shift+s"];
  const register = () => {
    for (const hotkey of state.hotkeys) hotkey.registered = hotkey.enabled && !held.includes(hotkey.keys.toLowerCase());
    emit("hotkeys", structuredClone(state.hotkeys) as HotkeyInfo[]);
  };

  /** A generated picture of one listed file. */
  const thumbnail = (path: string, edge: number): Promise<Blob> => {
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(16, edge);
    canvas.height = Math.round((canvas.width * 9) / 16);
    const g = canvas.getContext("2d")!;
    const { width, height } = canvas;
    const seed = [...path].reduce((sum, letter) => (sum * 31 + letter.charCodeAt(0)) % 997, 3);
    if (/\.mp4$/i.test(path)) standInDesktop(g, width, height, seed);
    else if (/\.mp3$/i.test(path)) {
      g.fillStyle = "#0c0c0c";
      g.fillRect(0, 0, width, height);
      g.fillStyle = "#41d6e0";
      for (let x = 2; x < width - 2; x += 3) {
        const reach = Math.abs(standInSample(x / 9 + seed, seed)) * height * 0.42 + 1;
        g.fillRect(x, height / 2 - reach, 2, reach * 2);
      }
    } else {
      const unit = width / 160;
      g.fillStyle = "#f3f4f1";
      g.fillRect(0, 0, width, height);
      g.fillStyle = "#dfe3e8";
      g.fillRect(0, 0, width, 9 * unit);
      g.fillStyle = "#c3cad4";
      for (let line = 0; line < 11; line++) g.fillRect(10 * unit, (18 + line * 6) * unit, (50 + ((seed * (line + 3)) % 80)) * unit, 2 * unit);
      g.strokeStyle = "#f74b67";
      g.lineWidth = 1.4 * unit;
      g.strokeRect(8 * unit, (27 + (seed % 3) * 12) * unit, 96 * unit, 15 * unit);
    }
    return new Promise((resolve, reject) => canvas.toBlob((blob) => (blob ? resolve(blob) : reject("The thumbnail is empty")), "image/png"));
  };

  return {
    demo: true,
    snapshot,
    async setFlag(name, value) {
      if (name === "upload" && value && !state.destination) throw "Choose an upload destination first";
      state[name] = value;
      if (!state.copy && !state.save) state[name === "copy" ? "save" : "copy"] = true;
      return snapshot();
    },
    async setSetting(name, value) {
      const text = value.trim();
      if (name === "folder") {
        if (!text) throw "Enter a folder path";
        state.folder = text;
      } else if (name === "format") {
        const format = state.formats.find((one) => one.toLowerCase() === text.toLowerCase());
        if (!format) throw `${text} is not an image format`;
        state.format = format;
      } else if (name === "sdrWhiteNits") {
        const nits = Number(text);
        if (text && text.toLowerCase() !== "auto" && !(nits >= 80 && nits <= 1000)) throw "enter a white level from 80 to 1000 nits, or leave it empty";
        state.sdrWhiteNits = text && text.toLowerCase() !== "auto" ? nits : null;
      } else if (name === "countdownSeconds") {
        const seconds = Number(text);
        if (!text || !(seconds >= 0 && seconds <= 60)) throw "enter a number of seconds from 0 to 60";
        state.countdownSeconds = Math.round(seconds);
      } else {
        const seconds = Number(text);
        if (!(seconds >= 0.25 && seconds <= 3600)) throw "enter a number of seconds from 0.25 to 3600";
        state[name] = seconds;
      }
      return snapshot();
    },
    async selectDestination(id) {
      if (id === null) {
        state.upload = false;
        return snapshot();
      }
      const destination = state.destinations.find((one) => one.id === id);
      if (!destination) throw `no upload destination named ${id}`;
      if (destination.keyLabel) throw `${destination.name} needs its ${destination.keyLabel}`;
      state.destination = destination.name;
      state.upload = true;
      return snapshot();
    },
    async saveKey(id, key) {
      const destination = state.destinations.find((one) => one.id === id);
      if (!destination) throw `${id} is not a built-in destination`;
      if (!/^[A-Za-z0-9_-]{6,128}$/.test(key.trim())) {
        throw `the ${destination.name} ${destination.keyLabel ?? "key"} has 6 to 128 letters, digits, and dashes`;
      }
      destination.keyLabel = null;
      destination.note = "ready";
      return this.selectDestination(id);
    },
    async importUploader(path) {
      const name = path.trim().replace(/^"|"$/g, "").split(/[\\/]/).pop()?.replace(/\.sxcu$/i, "") ?? "";
      if (!name || !/\.sxcu"?$/i.test(path.trim())) throw "Enter the path of a .sxcu file";
      if (!state.destinations.some((one) => one.id === name)) {
        state.destinations.push({ id: name, name, note: "custom uploader", keyLabel: null, keySource: "" });
      }
      return this.selectDestination(name);
    },
    async capture(_mode: CaptureKind, delayed) {
      emit("shots", { kind: "capturing" });
      window.setTimeout(finishCapture, delayed ? state.delaySeconds * 1000 : 450);
    },
    async toggleAuto() {
      state.autoActive = !state.autoActive;
      window.clearInterval(autoTimer);
      if (state.autoActive) autoTimer = window.setInterval(finishCapture, state.intervalSeconds * 1000);
      emit("shots", { kind: "auto", active: state.autoActive });
    },
    async monitors() {
      return monitors.map((monitor) => ({ ...monitor }));
    },
    async startRecording(mode, options = {}) {
      const region = options.region ?? null;
      if (region && (region.width < 32 || region.height < 32)) throw "the region is smaller than 32 x 32";
      return begin(mode, options.monitor, options.fps, region);
    },
    async stopRecording(mode) {
      end(mode, false);
    },
    async setPaused(mode, paused) {
      const session = running[mode];
      if (!session?.started) return false;
      session.paused = paused;
      return paused;
    },
    async discardRecording(mode) {
      end(mode, true);
    },
    async selectRegion() {
      await new Promise((resolve) => window.setTimeout(resolve, 300));
      return { ...demoRegion };
    },
    async setHotkey(id, keys) {
      const hotkey = state.hotkeys.find((one) => one.id === id);
      if (!hotkey) throw `no hotkey named ${id}`;
      const parts = keys.split("+");
      const key = parts[parts.length - 1] ?? "";
      if (keys.toLowerCase() === "ctrl+alt+delete") throw "Windows keeps these keys";
      const typing = /^([A-Z0-9]|Space|Enter|Tab|Left|Right|Up|Down|Delete|Backspace)$/i.test(key);
      if (typing && !parts.some((part) => ["ctrl", "alt", "win"].includes(part.toLowerCase()))) throw `${key} types text: add Ctrl, Alt, or Win`;
      const other = state.hotkeys.find((one) => one.id !== id && one.enabled && one.keys.toLowerCase() === keys.toLowerCase());
      if (other) throw `${other.label} uses ${keys}`;
      hotkey.keys = keys;
      register();
      return snapshot();
    },
    async setHotkeyEnabled(id, enabled) {
      const hotkey = state.hotkeys.find((one) => one.id === id);
      if (!hotkey) throw `no hotkey named ${id}`;
      const other = state.hotkeys.find((one) => one.id !== id && one.enabled && one.keys.toLowerCase() === hotkey.keys.toLowerCase());
      if (enabled && other) throw `${other.label} uses ${hotkey.keys}`;
      hotkey.enabled = enabled;
      register();
      return snapshot();
    },
    async uiSettings() {
      return uiSettings;
    },
    async setUiSettings(json) {
      if (json.length > 65536) throw "the settings are larger than 64 KB";
      uiSettings = json;
    },
    async thumbnail(path, edge) {
      if (!Number.isInteger(edge) || edge < 16 || edge > 1024) throw "the edge is from 16 to 1024 pixels";
      if (!Object.values(files).some((list) => list.some((file) => file.path === path))) {
        // A part or a clip of the running session is not listed yet.
        if (!path.startsWith(folders.audio)) throw "not a listed file";
      }
      return thumbnail(path, edge);
    },
    async deleteFile(path) {
      let found = false;
      for (const mode of ["audio", "video", "screenshot"] as const) {
        const before = files[mode].length;
        files[mode] = files[mode].filter((file) => file.path !== path);
        found ||= files[mode].length !== before;
      }
      if (!found && !path.startsWith(folders.audio)) throw "not a listed file";
    },
    async monitorPreview(box) {
      // The stand-in desktop moves on its own clock, as a real one does.
      return picture(box.width / box.height, performance.now() / 1000 + box.left / 64);
    },
    async files(mode) {
      return files[mode].map((file) => ({ ...file, ageSeconds: Math.max(0, Math.round((Date.now() - file.modifiedMs) / 1000)) }));
    },
    async picture() {
      return null;
    },
    async reveal() {},
    async openFile() {},
    async openFolder() {},
    async copyPicture() {},
    async editScreenshot(path) {
      emit("shots", { kind: "capturing" });
      window.setTimeout(() => emit("shots", { kind: "finished", file: path.split("\\").pop() ?? path, path, width: 2560, height: 1440, url: null, copied: state.copy, hdrNits: null, error: null }), 400);
    },
    async saveClip(name) {
      const session = running.audio;
      if (!session?.started) throw "No audio recording runs";
      const stem = name.trim().replace(/\.mp3$/i, "");
      if (!stem || /[\\/:*?"<>|]/.test(stem)) throw "the clip name is not a file name";
      clipCount += 1;
      session.parts += 1;
      const seconds = Math.max(1, session.frames / 48000);
      session.frames = 0;
      const listed = entry("audio", `${session.name}\\${stem}.mp3`);
      files.audio.unshift(listed);
      session.written.push(listed.path);
      emit("sound", { kind: "saved", name: `${stem}.mp3`, path: listed.path, clip: true, seconds, editable: true });
    },
    async clipWaveform() {
      return { left: wave(600, 1 + clipCount), right: wave(600, 7 + clipCount), seconds: 12 };
    },
    async playClip(_path, start, end) {
      window.clearTimeout(playTimer);
      playTimer = window.setTimeout(() => emit("played", null), (end - start) * 1000);
    },
    async stopClip() {
      window.clearTimeout(playTimer);
      emit("played", null);
    },
    async trimAudioClip(_path, start, end) {
      return end - start;
    },
    async openVideoEditor(path) {
      editing = { name: path.split("\\").pop() ?? path, path, width: 1920, height: 1080, fps: 60, frames: 1500 };
      return { ...editing };
    },
    async closeVideoEditor() {
      editing = null;
    },
    async editorFrame(seconds) {
      return picture(16 / 9, seconds);
    },
    async editorWaveform() {
      return wave(900, 3);
    },
    async exportVideo(edit) {
      const video = editing;
      if (!video) throw "No video is open";
      const total = edit.endFrame - edit.startFrame;
      emit("edit", { kind: "stage", name: "Preparing" });
      let done = 0;
      const timer = window.setInterval(() => {
        done = Math.min(total, done + Math.ceil(total / 12));
        emit("edit", { kind: "progress", done, total });
        if (done < total) return;
        window.clearInterval(timer);
        editing = { ...video, frames: total };
        emit("edit", { kind: "done", info: { ...editing }, error: null });
      }, 60);
    },
    async quit() {},
    async closeWindow() {},
    async minimize() {},
    async toggleMaximize() {
      maximized = !maximized;
      return maximized;
    },
    async isMaximized() {
      return maximized;
    },
    on(event, handler) {
      (handlers[event] ??= [] as never[]).push(handler as never);
    },
  };
}
