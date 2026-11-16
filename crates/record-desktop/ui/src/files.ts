// The file browser: one list of files with three views, as in a file
// manager. Home, System audio, and Screen capture each show one.

import { dotIcons, lineIcons } from "./icons";

export type View = "list" | "details" | "thumbs";
export const views: { view: View; label: string; icon: string }[] = [
  { view: "list", label: "List", icon: lineIcons.list },
  { view: "details", label: "Details", icon: lineIcons.details },
  { view: "thumbs", label: "Thumbnails", icon: lineIcons.thumbs },
];

export interface Item {
  path: string;
  name: string;
  modifiedMs: number;
  /** `null` when the size is not known. */
  sizeBytes: number | null;
  /** A length or another fact, shown after the name. */
  extra?: string;
}

export interface BrowserOptions {
  items: Item[];
  selected: string | null;
  view: View;
  /** Pictures of the files. Without this the thumbnail view shows type icons. */
  thumbnails: boolean;
  empty: string;
  thumbnail(path: string, edge: number): Promise<Blob>;
  select(item: Item): void;
  /** A double-click or Enter. */
  open(item: Item): void;
  remove(item: Item): void;
}

export interface Action {
  id: string;
  label: string;
  icon: string;
  enabled: boolean;
  run(): void;
}

const stampFormat = new Intl.DateTimeFormat(undefined, { dateStyle: "short", timeStyle: "short" });

/** The date and time of a file in the locale and the time zone of the user. */
export function stamp(milliseconds: number): string {
  return milliseconds > 0 ? stampFormat.format(milliseconds) : "";
}

export function size(bytes: number | null): string {
  if (bytes === null) return "";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

function extension(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
}

/** The kind of a file, from its extension. */
export function kindOf(name: string): "audio" | "video" | "screenshot" {
  const end = extension(name);
  if (["mp3", "wav", "flac", "m4a", "ogg"].includes(end)) return "audio";
  if (["mp4", "mkv", "webm", "mov"].includes(end)) return "video";
  return "screenshot";
}

const el = <K extends keyof HTMLElementTagNameMap>(tag: K, className = "", text = ""): HTMLElementTagNameMap[K] => {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text) element.textContent = text;
  return element;
};

// Thumbnails: each one loads once, when its tile comes into view.
const pictures = new Map<string, string>();
const failed = new Set<string>();
const waiting = new Map<Element, () => void>();
const watcher = new IntersectionObserver((entries) => {
  for (const entry of entries) {
    if (!entry.isIntersecting) continue;
    waiting.get(entry.target)?.();
    waiting.delete(entry.target);
    watcher.unobserve(entry.target);
  }
});

/** Forgets the picture of one file, after it changed or was deleted. */
export function forgetThumbnail(path: string): void {
  const url = pictures.get(path);
  if (url) URL.revokeObjectURL(url);
  pictures.delete(path);
  failed.delete(path);
}

/** Forgets every picture, when the user turns the thumbnails off. */
export function forgetThumbnails(): void {
  for (const path of [...pictures.keys()]) forgetThumbnail(path);
}

function tilePicture(item: Item, options: BrowserOptions): HTMLElement {
  const frame = el("span", "thumb");
  const icon = () => {
    frame.innerHTML = dotIcons[kindOf(item.name)];
  };
  const show = (url: string) => {
    const image = el("img");
    image.alt = "";
    image.draggable = false;
    image.src = url;
    frame.replaceChildren(image);
  };
  const known = pictures.get(item.path);
  if (!options.thumbnails || failed.has(item.path)) icon();
  else if (known) show(known);
  else {
    waiting.set(frame, () => {
      options.thumbnail(item.path, 256).then(
        (blob) => {
          const url = URL.createObjectURL(blob);
          pictures.set(item.path, url);
          if (frame.isConnected) show(url);
        },
        () => {
          failed.add(item.path);
          if (frame.isConnected) icon();
        },
      );
    });
    watcher.observe(frame);
  }
  return frame;
}

let lastClick = { path: "", time: 0 };

/** Draws the files into `list`. Nothing changes when the same files are drawn again. */
export function renderBrowser(list: HTMLElement, options: BrowserOptions): void {
  const { items, view, selected } = options;
  const signature = JSON.stringify([view, selected, options.thumbnails, items.map((item) => [item.path, item.name, item.modifiedMs, item.sizeBytes, item.extra])]);
  if (list.dataset.signature === signature) return;
  list.dataset.signature = signature;
  for (const frame of list.querySelectorAll(".thumb")) {
    waiting.delete(frame);
    watcher.unobserve(frame);
  }
  list.className = `body scroll files ${view}`;
  list.replaceChildren();
  if (!items.length) {
    list.append(el("p", "note", options.empty));
    return;
  }
  if (view === "details") {
    const head = el("div", "file head");
    head.append(el("span", "", "Name"), el("span", "", "Date"), el("span", "", "Type"), el("span", "num", "Size"));
    list.append(head);
  }
  for (const item of items) {
    const button = el("button", `file${item.path === selected ? " sel" : ""}`);
    button.type = "button";
    button.dataset.id = `file-${item.path}`;
    button.setAttribute("aria-pressed", String(item.path === selected));
    const name = el("span", "name", item.name);
    const when = el("span", "when", stamp(item.modifiedMs));
    button.title = item.name;
    if (view === "thumbs") {
      button.title = [item.name, stamp(item.modifiedMs), size(item.sizeBytes)].filter(Boolean).join("\n");
      button.append(tilePicture(item, options), name);
    } else if (view === "details") {
      if (item.extra) name.append(el("span", "extra", item.extra));
      button.append(name, when, el("span", "type", extension(item.name).toUpperCase()), el("span", "num", size(item.sizeBytes)));
    } else {
      button.append(name, el("span", "when", item.extra ? `${item.extra} · ${stamp(item.modifiedMs)}` : stamp(item.modifiedMs)));
    }
    button.addEventListener("click", () => {
      const now = performance.now();
      const twice = lastClick.path === item.path && now - lastClick.time < 450;
      lastClick = { path: twice ? "" : item.path, time: now };
      if (twice) options.open(item);
      else options.select(item);
    });
    button.addEventListener("keydown", (event) => {
      if (event.ctrlKey || event.altKey) return;
      const move = (to: Element | null) => {
        if (to instanceof HTMLButtonElement) {
          event.preventDefault();
          to.focus();
        }
      };
      if (event.key === "Enter") {
        event.preventDefault();
        options.open(item);
      } else if (event.key === "Delete") {
        event.preventDefault();
        options.remove(item);
      } else if (event.key === "ArrowDown" || (view === "thumbs" && event.key === "ArrowRight")) move(button.nextElementSibling);
      else if (event.key === "ArrowUp" || (view === "thumbs" && event.key === "ArrowLeft")) move(button.previousElementSibling);
      else return;
      event.stopPropagation();
    });
    list.append(button);
  }
}

/** Draws the buttons in the top edge of a browser: the actions, then the views. */
export function renderTools(tools: HTMLElement, actions: Action[], view: View | null, setView: (view: View) => void): void {
  const button = (id: string, label: string, icon: string, enabled: boolean, pressed: boolean | null, run: () => void) => {
    const element = el("button", "icon");
    element.type = "button";
    element.title = label;
    element.setAttribute("aria-label", label);
    element.dataset.id = id;
    element.disabled = !enabled;
    if (pressed !== null) element.setAttribute("aria-pressed", String(pressed));
    element.innerHTML = icon;
    element.addEventListener("click", run);
    return element;
  };
  const prefix = tools.id;
  const nodes: HTMLElement[] = actions.map((action) => button(`${prefix}-${action.id}`, action.label, action.icon, action.enabled, null, action.run));
  if (view !== null) {
    if (nodes.length) nodes.push(el("span", "split"));
    for (const one of views) nodes.push(button(`${prefix}-${one.view}`, one.label, one.icon, true, one.view === view, () => setView(one.view)));
  }
  tools.replaceChildren(...nodes);
}
