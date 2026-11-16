// The values that cross between the engines and the interface. Each type
// mirrors one type of `src/payload.rs`.

export type Mode = "audio" | "video" | "screenshot";
export type Recorder = "audio" | "video";

/** A part of the desktop, in desktop pixels. */
export interface Region {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** The end of one recording. `discarded` is true when nothing was kept. */
export interface Stopped {
  kind: "stopped";
  file: string | null;
  path: string | null;
  error: string | null;
  discarded: boolean;
}
export type Route = "home" | Mode;
export type CaptureKind = "region" | "fullscreen" | "window" | "monitor" | "last";

export type SoundUpdate =
  | { kind: "starting"; path: string; countdown: number }
  | { kind: "countdown"; seconds: number }
  | { kind: "started"; sampleRate: number; bitrate: number }
  | { kind: "block"; left: [number, number]; right: [number, number]; seconds: number; bands: number[] }
  | { kind: "finalizing" }
  | ({ kind: "saved" } & Save)
  | { kind: "notice"; text: string }
  | Stopped;

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
  | { kind: "starting"; path: string; countdown: number; monitor: number; region: Region | null }
  | { kind: "countdown"; seconds: number }
  | { kind: "started"; width: number; height: number; fps: number }
  | { kind: "finalizing" }
  | { kind: "notice"; text: string }
  | Stopped;

export type HotkeyId = "region" | "fullscreen" | "window" | "videoRegion" | "videoFullscreen";

/** One global hotkey: what the user wants and what Windows gave. */
export interface HotkeyInfo {
  id: HotkeyId;
  label: string;
  keys: string;
  enabled: boolean;
  registered: boolean;
}

export type ShotUpdate =
  | { kind: "capturing" }
  | {
      kind: "finished";
      file: string | null;
      path: string | null;
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
  /** Unix time in milliseconds. */
  modifiedMs: number;
  sizeBytes: number;
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
  hotkeys: HotkeyInfo[];
  /** Seconds that a recording waits before it starts. 0 starts at once. */
  countdownSeconds: number;
  /** The modes that record now. */
  running: Recorder[];
  autoActive: boolean;
  engineError: string | null;
  version: string;
}

export type Flag = "copy" | "save" | "upload" | "primaryMonitor" | "autostart" | "closeToTray";
export type Setting = "folder" | "format" | "delaySeconds" | "intervalSeconds" | "sdrWhiteNits" | "countdownSeconds";

export interface Events {
  sound: SoundUpdate;
  video: VideoUpdate;
  shots: ShotUpdate;
  hotkeys: HotkeyInfo[];
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
  /** Starts one mode. The answer is the path of the file or the session folder. */
  startRecording(mode: Recorder, options?: { monitor?: number; fps?: number; region?: Region | null }): Promise<string>;
  /** Saves and stops. During a countdown this cancels the start. */
  stopRecording(mode: Recorder): Promise<void>;
  setPaused(mode: Recorder, paused: boolean): Promise<boolean>;
  /** Stops and deletes everything that this recording wrote. */
  discardRecording(mode: Recorder): Promise<void>;
  /** The region selector of the desktop. `null` means cancelled. */
  selectRegion(): Promise<Region | null>;
  setHotkey(id: HotkeyId, keys: string): Promise<Snapshot>;
  setHotkeyEnabled(id: HotkeyId, enabled: boolean): Promise<Snapshot>;
  /** The JSON text that the interface stored. */
  uiSettings(): Promise<string>;
  setUiSettings(json: string): Promise<void>;
  /** A small picture of one listed file. */
  thumbnail(path: string, edge: number): Promise<Blob>;
  /** Moves one listed file to the Recycle Bin. */
  deleteFile(path: string): Promise<void>;
  /** A small picture of one monitor as it looks now. */
  monitorPreview(monitor: Region): Promise<Blob>;
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
