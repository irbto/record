// The bridge to the engines inside the desktop program.

import type { Bridge, Events } from "./types";

interface Tauri {
  core: { invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> };
  event: { listen(event: string, handler: (event: { payload: unknown }) => void): Promise<unknown> };
}

declare global {
  interface Window {
    __TAURI__?: Tauri;
  }
}

/**
 * Returns the bytes of a binary answer. The usual form is an `ArrayBuffer`.
 * The fallback channel of the window shell sends a plain array of numbers.
 */
function bytesOf(value: unknown): ArrayBuffer | null {
  if (value instanceof ArrayBuffer) return value;
  if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength).slice().buffer;
  if (Array.isArray(value)) return new Uint8Array(value as number[]).buffer;
  return null;
}

/** Returns the bridge of the desktop program, or `null` in a plain browser. */
export function desktopBridge(): Bridge | null {
  const tauri = window.__TAURI__;
  if (!tauri) return null;
  const { invoke } = tauri.core;
  return {
    demo: false,
    snapshot: () => invoke("snapshot"),
    setFlag: (name, value) => invoke("set_flag", { name, value }),
    setSetting: (name, value) => invoke("set_setting", { name, value }),
    selectDestination: (id) => invoke("select_destination", { id }),
    saveKey: (id, key) => invoke("save_key", { id, key }),
    importUploader: (path) => invoke("import_uploader", { path }),
    capture: (mode, delayed) => invoke("capture", { mode, delayed }),
    toggleAuto: () => invoke("toggle_auto"),
    monitors: () => invoke("monitors"),
    startRecording: (mode, monitor, fps) => invoke("start_recording", { mode, monitor, fps }),
    stopRecording: () => invoke("stop_recording"),
    setPaused: (paused) => invoke("set_paused", { paused }),
    async monitorPreview({ left, top, width, height }) {
      const bytes = bytesOf(await invoke<unknown>("monitor_preview", { left, top, width, height }));
      if (!bytes || !bytes.byteLength) throw "The preview is empty";
      return new Blob([bytes], { type: "image/jpeg" });
    },
    files: (mode) => invoke("files", { mode }),
    async picture(path) {
      const bytes = bytesOf(await invoke<unknown>("picture", { path }));
      // No type: the window finds the format from the bytes.
      return bytes ? new Blob([bytes]) : null;
    },
    reveal: (path) => invoke("reveal", { path }),
    openFile: (path) => invoke("open_file", { path }),
    openFolder: (mode) => invoke("open_folder", { mode }),
    copyPicture: (path) => invoke("copy_picture", { path }),
    editScreenshot: (path) => invoke("edit_screenshot", { path }),
    saveClip: (name) => invoke("save_clip", { name }),
    clipWaveform: (path) => invoke("clip_waveform", { path }),
    playClip: (path, start, end) => invoke("play_clip", { path, start, end }),
    stopClip: () => invoke("stop_clip"),
    trimAudioClip: (path, start, end) => invoke("trim_audio_clip", { path, start, end }),
    openVideoEditor: (path) => invoke("open_video_editor", { path }),
    closeVideoEditor: () => invoke("close_video_editor"),
    async editorFrame(seconds) {
      const bytes = bytesOf(await invoke<unknown>("editor_frame", { seconds }));
      if (!bytes || !bytes.byteLength) throw "The picture is empty";
      return new Blob([bytes], { type: "image/jpeg" });
    },
    editorWaveform: () => invoke("editor_waveform"),
    exportVideo: (edit) => invoke("export_video", { edit }),
    quit: () => invoke("quit"),
    closeWindow: () => invoke("close_window"),
    minimize: () => invoke("minimize"),
    toggleMaximize: () => invoke("toggle_maximize"),
    isMaximized: () => invoke("is_maximized"),
    on<K extends keyof Events>(event: K, handler: (payload: Events[K]) => void) {
      if (event === "drop") {
        void tauri.event.listen("tauri://drag-drop", (dropped) => {
          const paths = (dropped.payload as { paths?: string[] }).paths ?? [];
          (handler as (payload: string[]) => void)(paths);
        });
      } else {
        void tauri.event.listen(event, (sent) => handler(sent.payload as Events[K]));
      }
    },
  };
}
