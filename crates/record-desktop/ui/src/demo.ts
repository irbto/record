// A simulation of the engines for a plain browser. It lets the interface be
// viewed and tested without Windows. Nothing is recorded.

import { standInDesktop, standInSample } from "./draw";
import type { Bridge, CaptureKind, Events, FileEntry, Frame, Mode, Snapshot } from "./types";

const two = (value: number) => String(value).padStart(2, "0");

function stamp(): string {
  const now = new Date();
  return (
    `${now.getFullYear()}${two(now.getMonth() + 1)}${two(now.getDate())}` +
    `-${two(now.getHours())}${two(now.getMinutes())}${two(now.getSeconds())}`
  );
}

const folders: Record<Mode, string> = {
  audio: "C:\\Users\\marcu\\Music\\record",
  video: "C:\\Users\\marcu\\Videos\\record",
  screenshot: "C:\\Users\\marcu\\Pictures\\Screenshots",
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
  let sequence = 0;
  let autoTimer = 0;
  let maximized = false;
  const desktop = document.createElement("canvas");
  desktop.width = 640;
  desktop.height = 360;

  const finishCapture = () => {
    const name = `screenshot-${stamp()}.png`;
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
        { index: 0, width: 2560, height: 1440, primary: false },
        { index: 1, width: 1920, height: 1080, primary: true },
      ];
    },
    async startRecording(mode, _monitor, fps) {
      if (running) throw "A recording runs already. Save it first.";
      const name = `recording-${stamp()}.${mode === "audio" ? "mp3" : "mp4"}`;
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
        files[session.mode].unshift(entry(session.mode, session.name, 0));
        emit(event, { kind: "stopped", file: session.name, error: null });
      }, 300);
    },
    async setPaused(paused) {
      if (!running) return false;
      running.paused = paused;
      return paused;
    },
    async videoFrame(after): Promise<Frame | null> {
      if (!running || running.mode !== "video" || running.paused) return null;
      sequence += 1;
      if (sequence === after) return null;
      const g = desktop.getContext("2d", { willReadFrequently: true })!;
      standInDesktop(g, desktop.width, desktop.height, signal);
      const pixels = g.getImageData(0, 0, desktop.width, desktop.height);
      return { sequence, width: desktop.width, height: desktop.height, rgba: pixels.data };
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
    async quit() {},
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
