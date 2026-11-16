// The values that cross between the engines and the interface. Each type
// mirrors one type of `src/payload.rs`.

export type Mode = "audio" | "video" | "screenshot";
export type Route = "home" | Mode;
export type CaptureKind = "region" | "fullscreen" | "window" | "monitor" | "last";

export type SoundUpdate =
  | { kind: "started"; sampleRate: number; bitrate: number }
  | { kind: "block"; left: [number, number]; right: [number, number]; seconds: number; bands: number[] }
  | { kind: "finalizing" }
  | { kind: "notice"; text: string }
  | { kind: "stopped"; file: string | null; error: string | null };

export type VideoUpdate =
  | { kind: "started"; width: number; height: number; fps: number }
  | { kind: "finalizing" }
  | { kind: "notice"; text: string }
  | { kind: "stopped"; file: string | null; error: string | null };

export type ShotUpdate =
  | { kind: "ready"; hotkeys: [boolean, boolean, boolean] }
  | { kind: "capturing" }
  | {
      kind: "finished";
      file: string | null;
      width: number;
      height: number;
      url: string | null;
      copied: boolean;
      hdrNits: number | null;
      error: string | null;
    }
  | { kind: "cancelled" }
  | { kind: "failed"; error: string }
  | { kind: "auto"; active: boolean };

export interface Monitor {
  index: number;
  width: number;
  height: number;
  primary: boolean;
}

export interface FileEntry {
  name: string;
  path: string;
  ageSeconds: number;
}

export interface Destination {
  id: string;
  name: string;
  note: string;
  keyLabel: string | null;
  keySource: string;
}

export interface Snapshot {
  copy: boolean;
  save: boolean;
  upload: boolean;
  destination: string | null;
  destinations: Destination[];
  folder: string;
  audioFolder: string;
  videoFolder: string;
  delaySeconds: number;
  intervalSeconds: number;
  primaryMonitor: boolean;
  sdrWhiteNits: number | null;
  autostart: boolean;
  hotkeys: [boolean, boolean, boolean];
  autoActive: boolean;
  engineError: string | null;
  version: string;
}

export type Flag = "copy" | "save" | "upload" | "primaryMonitor" | "autostart";
export type Setting = "folder" | "delaySeconds" | "intervalSeconds" | "sdrWhiteNits";

/** One live picture of a video recording. */
export interface Frame {
  sequence: number;
  width: number;
  height: number;
  rgba: Uint8ClampedArray;
}

export interface Events {
  sound: SoundUpdate;
  video: VideoUpdate;
  shots: ShotUpdate;
  closing: null;
  drop: string[];
}

/** Everything the interface asks of the program behind it. */
export interface Bridge {
  /** True when a simulation stands in for the engines. */
  readonly demo: boolean;
  snapshot(): Promise<Snapshot>;
  setFlag(name: Flag, value: boolean): Promise<Snapshot>;
  setSetting(name: Setting, value: string): Promise<Snapshot>;
  selectDestination(id: string | null): Promise<Snapshot>;
  saveKey(id: string, key: string): Promise<Snapshot>;
  importUploader(path: string): Promise<Snapshot>;
  capture(mode: CaptureKind, delayed: boolean): Promise<void>;
  toggleAuto(): Promise<void>;
  monitors(): Promise<Monitor[]>;
  startRecording(mode: Mode, monitor: number, fps: number): Promise<string>;
  stopRecording(): Promise<void>;
  setPaused(paused: boolean): Promise<boolean>;
  videoFrame(after: number): Promise<Frame | null>;
  files(mode: Mode): Promise<FileEntry[]>;
  /** The bytes of one screenshot, or `null` when a stand-in is drawn. */
  picture(path: string): Promise<Blob | null>;
  reveal(path: string): Promise<void>;
  openFile(path: string): Promise<void>;
  openFolder(mode: Mode): Promise<void>;
  copyPicture(path: string): Promise<void>;
  quit(): Promise<void>;
  on<K extends keyof Events>(event: K, handler: (payload: Events[K]) => void): void;
}
