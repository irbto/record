// The bridge to the engines inside the desktop program.

import type { Bridge, Events, Frame } from "./types";

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
    async videoFrame(after): Promise<Frame | null> {
      const bytes = bytesOf(await invoke<unknown>("video_frame", { after }));
      if (!bytes || bytes.byteLength <= 12) return null;
      const head = new DataView(bytes, 0, 12);
      const width = head.getUint32(4, true);
      const height = head.getUint32(8, true);
      if (bytes.byteLength !== 12 + width * height * 4) return null;
      return { sequence: head.getUint32(0, true), width, height, rgba: new Uint8ClampedArray(bytes, 12) };
    },
    files: (mode) => invoke("files", { mode }),
    async picture(path) {
      const bytes = bytesOf(await invoke<unknown>("picture", { path }));
      return bytes ? new Blob([bytes], { type: "image/png" }) : null;
    },
    reveal: (path) => invoke("reveal", { path }),
    openFile: (path) => invoke("open_file", { path }),
    openFolder: (mode) => invoke("open_folder", { mode }),
    copyPicture: (path) => invoke("copy_picture", { path }),
    quit: () => invoke("quit"),
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
