// The values that cross between the engines and the interface. Each type
// mirrors one type of `src/payload.rs`.

export type Mode = "audio" | "video" | "screenshot";
export type Route = "home" | Mode;
export type CaptureKind = "region" | "fullscreen" | "window" | "monitor" | "last";

export type SoundUpdate =
  | { kind: "started"; sampleRate: number; bitrate: number }
  | { kind: "block"; left: [number, number]; right: [number, number]; seconds: number; bands: number[] }
  | { kind: "finalizing" }
  | ({ kind: "saved" } & Save)
  | { kind: "notice"; text: string }
  | { kind: "stopped"; file: string | null; error: string | null };

/** One finished file of an audio session: a part or a named clip. */
export interface Save {
  name: string;
  path: string;
  clip: boolean;
  seconds: number;
  editable: boolean;
}

/** The shape of one audio clip: the extent of each column. */
export interface ClipShape {
  left: [number, number][];
  right: [number, number][];
  seconds: number;
}

/** One video that is open in the editor. */
export interface VideoInfo {
  name: string;
  path: string;
  width: number;
  height: number;
  fps: number;
  frames: number;
}

/** The edit of a video. Times are in seconds, crops in pixels. */
export interface VideoEdit {
  startFrame: number;
  endFrame: number;
  audioFadeIn: number;
  audioFadeOut: number;
  videoFadeIn: number;
  videoFadeOut: number;
  cropLeft: number;
  cropRight: number;
  cropTop: number;
  cropBottom: number;
  /** Keyframes of the picture level: seconds from the range start, level 0 to 1. */
  opacity: [number, number][];
  /** Keyframes of the audio level, in the same form. */
  volume: [number, number][];
}

export type EditUpdate =
  | { kind: "stage"; name: string }
  | { kind: "progress"; done: number; total: number }
  | { kind: "done"; info: VideoInfo | null; error: string | null };

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
  left: number;
  top: number;
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
  format: string;
  formats: string[];
  closeToTray: boolean;
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

export type Flag = "copy" | "save" | "upload" | "primaryMonitor" | "autostart" | "closeToTray";
export type Setting = "folder" | "format" | "delaySeconds" | "intervalSeconds" | "sdrWhiteNits";

export interface Events {
  sound: SoundUpdate;
  video: VideoUpdate;
  shots: ShotUpdate;
  edit: EditUpdate;
  played: null;
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
  /** A small picture of one monitor as it looks now. */
  monitorPreview(monitor: Monitor): Promise<Blob>;
  files(mode: Mode): Promise<FileEntry[]>;
  /** The bytes of one screenshot, or `null` when a stand-in is drawn. */
  picture(path: string): Promise<Blob | null>;
  reveal(path: string): Promise<void>;
  openFile(path: string): Promise<void>;
  openFolder(mode: Mode): Promise<void>;
  copyPicture(path: string): Promise<void>;
  /** Opens one saved screenshot in the annotation overlay. */
  editScreenshot(path: string): Promise<void>;
  /** Cuts the audio since the last file into a named clip. */
  saveClip(name: string): Promise<void>;
  clipWaveform(path: string): Promise<ClipShape>;
  playClip(path: string, start: number, end: number): Promise<void>;
  stopClip(): Promise<void>;
  /** Keeps one part of a clip and returns the new length in seconds. */
  trimAudioClip(path: string, start: number, end: number): Promise<number>;
  openVideoEditor(path: string): Promise<VideoInfo>;
  closeVideoEditor(): Promise<void>;
  /** The picture of the open video at one time. */
  editorFrame(seconds: number): Promise<Blob>;
  editorWaveform(): Promise<[number, number][]>;
  /** Writes the edit over the file. The `edit` event reports the progress. */
  exportVideo(edit: VideoEdit): Promise<void>;
  /** Ends the program. */
  quit(): Promise<void>;
  /** Closes the window as the close button does: to the tray, or for good. */
  closeWindow(): Promise<void>;
  minimize(): Promise<void>;
  /** Maximizes or restores the window and returns the new state. */
  toggleMaximize(): Promise<boolean>;
  isMaximized(): Promise<boolean>;
  on<K extends keyof Events>(event: K, handler: (payload: Events[K]) => void): void;
}
