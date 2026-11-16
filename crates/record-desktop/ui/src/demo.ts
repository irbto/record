// A simulation of the engines for a plain browser. It lets the interface be
// viewed and tested without Windows. Nothing is recorded.

import { standInDesktop, standInSample } from "./draw";
import type { Bridge, CaptureKind, Events, FileEntry, Mode, Snapshot, VideoInfo } from "./types";

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

function entry(mode: Mode, name: string, ageSeconds: number): FileEntry {
  return { name, path: `${folders[mode]}\\${name}`, ageSeconds };
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
    hotkeys: [true, true, true],
    autoActive: false,
    engineError: null,
    version: "0.3.3",
  };
  const files: Record<Mode, FileEntry[]> = {
    audio: [
      entry("audio", "recording-20261116-162210.mp3", 3 * 3600),
      entry("audio", "recording-20261114-201955.mp3", 2 * 86400),
    ],
    video: [
      entry("video", "recording-20261116-174455.mp4", 3600),
      entry("video", "recording-20261115-203310.mp4", 22 * 3600),
      entry("video", "recording-20261113-110802.mp4", 3 * 86400),
    ],
    screenshot: [
      entry("screenshot", "screenshot-20261116-184210.png", 120),
      entry("screenshot", "screenshot-20261116-181455.png", 29 * 60),
      entry("screenshot", "screenshot-20261116-173002.png", 3700),
      entry("screenshot", "screenshot-20261115-221940.png", 21 * 3600),
      entry("screenshot", "screenshot-20261115-140313.png", 86400 + 500),
      entry("screenshot", "screenshot-20261114-095127.png", 2 * 86400 + 900),
    ],
  };

  let running: { mode: Mode; name: string; timer: number; frames: number; paused: boolean } | null = null;
  let signal = 21.4;
  let autoTimer = 0;
  let maximized = false;
  let playTimer = 0;
  let clipCount = 0;
  let partCount = 0;
  let editing: VideoInfo | null = null;
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
    if (state.save) files.screenshot.unshift(entry("screenshot", name, 0));
    emit("shots", {
      kind: "finished",
      file: state.save ? name : null,
      width: 2560,
      height: 1440,
      url: state.upload ? "https://files.catbox.moe/k3v9qd.png" : null,
      copied: state.copy || state.upload,
      hdrNits: 240,
      error: null,
    });
  };

  const snapshot = async () => structuredClone(state);

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
      return [
        { index: 0, width: 2560, height: 1440, primary: false, left: -2560, top: 0 },
        { index: 1, width: 1920, height: 1080, primary: true, left: 0, top: 0 },
      ];
    },
    async startRecording(mode, _monitor, fps) {
      if (running) throw "A recording runs already. Save it first.";
      const name = mode === "audio" ? `recording-${stamp()}` : `recording-${stamp()}.mp4`;
      partCount = 0;
      const session = { mode, name, timer: 0, frames: 0, paused: false };
      running = session;
      window.setTimeout(() => {
        if (running !== session) return;
        if (mode === "audio") emit("sound", { kind: "started", sampleRate: 48000, bitrate: 320000 });
        else emit("video", { kind: "started", width: 1920, height: 1080, fps });
        session.timer = window.setInterval(() => {
          if (session.paused) return;
          signal += 1024 / 48000;
          if (mode !== "audio") return;
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
      }, 120);
      return `${folders[mode]}\\${name}`;
    },
    async stopRecording() {
      const session = running;
      if (!session) return;
      window.clearInterval(session.timer);
      const event = session.mode === "audio" ? "sound" : "video";
      emit(event, { kind: "finalizing" });
      window.setTimeout(() => {
        running = null;
        if (session.mode === "audio") {
          partCount += 1;
          const part = `part-${String(partCount).padStart(3, "0")}.mp3`;
          files.audio.unshift(entry("audio", `${session.name}\\${part}`, 0));
          emit("sound", { kind: "saved", name: part, path: `${folders.audio}\\${session.name}\\${part}`, clip: false, seconds: session.frames / 48000, editable: false });
        } else files.video.unshift(entry("video", session.name, 0));
        emit(event, { kind: "stopped", file: session.name, error: null });
      }, 300);
    },
    async setPaused(paused) {
      if (!running) return false;
      running.paused = paused;
      return paused;
    },
    async monitorPreview(monitor) {
      // The stand-in desktop moves on its own clock, as a real one does.
      return picture(monitor.width / monitor.height, performance.now() / 1000 + monitor.index * 40);
    },
    async files(mode) {
      return files[mode].map((file) => ({ ...file }));
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
      window.setTimeout(() => emit("shots", { kind: "finished", file: path.split("\\").pop() ?? path, width: 2560, height: 1440, url: null, copied: state.copy, hdrNits: null, error: null }), 400);
    },
    async saveClip(name) {
      const session = running;
      if (!session || session.mode !== "audio") throw "No audio recording runs";
      const stem = name.trim().replace(/\.mp3$/i, "");
      if (!stem || /[\\/:*?"<>|]/.test(stem)) throw "the clip name is not a file name";
      clipCount += 1;
      partCount += 1;
      const seconds = Math.max(1, session.frames / 48000);
      session.frames = 0;
      emit("sound", { kind: "saved", name: `${stem}.mp3`, path: `${folders.audio}\\${session.name}\\${stem}.mp3`, clip: true, seconds, editable: true });
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
