// The key binds of the window: every command with its keys. The user can
// change each one. The changes are stored with the interface settings.

export type Command =
  | "home"
  | "audio"
  | "video"
  | "screenshot"
  | "record"
  | "recordPause"
  | "pause"
  | "clip"
  | "stop"
  | "discard"
  | "activity"
  | "openFolder"
  | "destination"
  | "settings"
  | "keyBinds"
  | "toolRange"
  | "toolVideoFade"
  | "toolAudioFade"
  | "toolCrop"
  | "frameBack"
  | "frameForward";

/** Every command, in the order of the Key binds dialog. */
export const commands: { id: Command; label: string; keys: string; group: "Window" | "Recording" | "Editor" }[] = [
  { id: "home", label: "Home", keys: "Ctrl+0", group: "Window" },
  { id: "audio", label: "System audio", keys: "Ctrl+1", group: "Window" },
  { id: "video", label: "Screen capture", keys: "Ctrl+2", group: "Window" },
  { id: "screenshot", label: "Screenshot", keys: "Ctrl+3", group: "Window" },
  { id: "activity", label: "Activity", keys: "Ctrl+J", group: "Window" },
  { id: "openFolder", label: "Open folder", keys: "Ctrl+O", group: "Window" },
  { id: "destination", label: "Destination", keys: "Ctrl+D", group: "Window" },
  { id: "settings", label: "Settings", keys: "Ctrl+,", group: "Window" },
  { id: "keyBinds", label: "Key binds", keys: "F1", group: "Window" },
  { id: "record", label: "Record", keys: "Ctrl+R", group: "Recording" },
  { id: "recordPause", label: "Record, pause, resume", keys: "Space", group: "Recording" },
  { id: "pause", label: "Pause, resume", keys: "Ctrl+P", group: "Recording" },
  { id: "clip", label: "Clip", keys: "Ctrl+K", group: "Recording" },
  { id: "stop", label: "Save and stop", keys: "Ctrl+S", group: "Recording" },
  { id: "discard", label: "Discard", keys: "Ctrl+Delete", group: "Recording" },
  { id: "toolRange", label: "Range tool", keys: "T", group: "Editor" },
  { id: "toolVideoFade", label: "Video fade tool", keys: "V", group: "Editor" },
  { id: "toolAudioFade", label: "Audio fade tool", keys: "A", group: "Editor" },
  { id: "toolCrop", label: "Crop tool", keys: "C", group: "Editor" },
  { id: "frameBack", label: "Frame back", keys: "Left", group: "Editor" },
  { id: "frameForward", label: "Frame forward", keys: "Right", group: "Editor" },
];

const named: Record<string, string> = {
  PrintScreen: "PrtSc",
  Space: "Space",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Semicolon: ";",
  Quote: "'",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Minus: "-",
  Equal: "=",
  Backquote: "`",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  ArrowUp: "Up",
  ArrowDown: "Down",
  Escape: "Esc",
  Enter: "Enter",
  Tab: "Tab",
  Backspace: "Backspace",
  Delete: "Delete",
  Insert: "Insert",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  Pause: "Pause",
  ScrollLock: "ScrollLock",
  NumLock: "NumLock",
  CapsLock: "CapsLock",
  NumpadMultiply: "NumpadMultiply",
  NumpadAdd: "NumpadAdd",
  NumpadSubtract: "NumpadSubtract",
};

/** The names that the program behind the window writes for the keys shown as signs here. */
const signs: Record<string, string> = {
  ",": "comma",
  ".": "period",
  "/": "slash",
  ";": "semicolon",
  "'": "quote",
  "[": "bracketleft",
  "]": "bracketright",
  "\\": "backslash",
  "-": "minus",
  "=": "plus",
  equal: "plus",
  "`": "backquote",
  pgup: "pageup",
  pgdn: "pagedown",
};

/** One spelling for the same keys, to compare a window bind with a global hotkey. */
export function sameKeys(keys: string): string {
  const parts = keys.toLowerCase().split("+");
  const key = parts.pop() ?? "";
  return [...parts, signs[key] ?? key].join("+");
}

/** The name of the key of one event, without modifiers. `null` for a modifier. */
function keyName(event: KeyboardEvent): string | null {
  const code = event.code;
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit\d$/.test(code)) return code.slice(5);
  if (/^Numpad\d$/.test(code)) return code;
  if (/^F\d{1,2}$/.test(code)) return code;
  if (named[code]) return named[code]!;
  const key = event.key;
  if (["Control", "Alt", "Shift", "Meta", "AltGraph", "OS", "Dead", "Unidentified", "Process"].includes(key)) return null;
  if (key === " ") return "Space";
  if (named[key]) return named[key]!;
  return key.length === 1 ? key.toUpperCase() : key || null;
}

/**
 * The keys of one event as text: the modifiers in the order Ctrl, Alt,
 * Shift, Win, then one key. `null` while only modifiers are down.
 */
export function comboOf(event: KeyboardEvent, shift = true): string | null {
  const key = keyName(event);
  if (!key) return null;
  const parts: string[] = [];
  if (event.ctrlKey) parts.push("Ctrl");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey && shift) parts.push("Shift");
  if (event.metaKey) parts.push("Win");
  parts.push(key);
  return parts.join("+");
}

let changed: Partial<Record<Command, string>> = {};
let byKeys = new Map<string, Command>();

function index(): void {
  byKeys = new Map(commands.map((command) => [sameKeys(keysOf(command.id)), command.id]));
}

/** The keys of one command as they are now. */
export function keysOf(command: Command): string {
  return changed[command] ?? commands.find((one) => one.id === command)?.keys ?? "";
}

/** The command that these keys run, if there is one. */
export function commandFor(keys: string | null): Command | null {
  return keys ? (byKeys.get(sameKeys(keys)) ?? null) : null;
}

/** The changes from the defaults, as they are stored. */
export function changedBinds(): Partial<Record<Command, string>> {
  return { ...changed };
}

/** Takes stored changes. Unknown names and empty keys are dropped. */
export function setBinds(stored: unknown): void {
  changed = {};
  if (stored && typeof stored === "object") {
    for (const command of commands) {
      const keys = (stored as Record<string, unknown>)[command.id];
      if (typeof keys === "string" && keys && keys !== command.keys) changed[command.id] = keys;
    }
  }
  index();
}

/** Gives one command new keys. Returns the command that has them already, if any. */
export function bind(command: Command, keys: string): Command | null {
  const holder = commandFor(keys);
  if (holder && holder !== command) return holder;
  const standard = commands.find((one) => one.id === command)?.keys;
  if (keys === standard) delete changed[command];
  else changed[command] = keys;
  index();
  return null;
}

index();
