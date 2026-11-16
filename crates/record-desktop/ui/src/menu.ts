// The menu bar: File, View, Capture, Help. It is part of the page, so it
// keeps the look of the program and works the same on every system.

export type MenuEntry =
  | { label: string; key?: string; enabled?: boolean; checked?: boolean; run: () => void }
  | "separator";

let bar: HTMLElement | null = null;
let names: string[] = [];
let entriesOf: (menu: string) => MenuEntry[] = () => [];
let open: number | null = null;

function parts(index: number): { wrap: HTMLElement; button: HTMLButtonElement; items: HTMLElement } | null {
  const wrap = bar?.children[index] as HTMLElement | undefined;
  if (!wrap) return null;
  return { wrap, button: wrap.children[0] as HTMLButtonElement, items: wrap.children[1] as HTMLElement };
}

/** Closes the open menu, if there is one. */
export function closeMenus(): void {
  if (open === null) return;
  const menu = parts(open);
  menu?.wrap.classList.remove("open");
  menu?.button.setAttribute("aria-expanded", "false");
  menu?.items.replaceChildren();
  open = null;
}

function openMenu(index: number, focusFirst = false): void {
  closeMenus();
  const menu = parts(index);
  const name = names[index];
  if (!menu || name === undefined) return;
  for (const entry of entriesOf(name)) {
    if (entry === "separator") {
      menu.items.append(document.createElement("hr"));
      continue;
    }
    const item = document.createElement("button");
    item.type = "button";
    item.className = "item";
    item.setAttribute("role", entry.checked === undefined ? "menuitem" : "menuitemcheckbox");
    if (entry.checked !== undefined) item.setAttribute("aria-checked", String(entry.checked));
    item.disabled = entry.enabled === false;
    const tick = document.createElement("span");
    tick.className = "tick";
    tick.textContent = entry.checked ? "●" : "";
    const label = document.createElement("span");
    label.textContent = entry.label;
    const key = document.createElement("kbd");
    key.textContent = entry.key ?? "";
    item.append(tick, label, key);
    item.addEventListener("click", () => {
      closeMenus();
      entry.run();
    });
    menu.items.append(item);
  }
  menu.wrap.classList.add("open");
  menu.button.setAttribute("aria-expanded", "true");
  open = index;
  if (focusFirst) step(1, true);
  else menu.button.focus();
}

/** Moves the focus to the next or the previous usable item. */
function step(direction: 1 | -1, fromEdge = false): void {
  if (open === null) return;
  const items = [...(parts(open)?.items.querySelectorAll<HTMLButtonElement>(".item:not(:disabled)") ?? [])];
  if (!items.length) return;
  const at = items.indexOf(document.activeElement as HTMLButtonElement);
  const next = fromEdge || at < 0 ? (direction === 1 ? 0 : items.length - 1) : (at + direction + items.length) % items.length;
  items[next]?.focus();
}

function onKey(event: KeyboardEvent): void {
  if (!bar) return;
  if (event.key === "Alt") {
    bar.classList.add("keys");
    return;
  }
  const dialogOpen = document.querySelector("dialog[open]") !== null;
  if (event.altKey && !event.ctrlKey && !dialogOpen && event.key.length === 1) {
    const index = names.findIndex((name) => name[0]?.toLowerCase() === event.key.toLowerCase());
    if (index >= 0) {
      event.preventDefault();
      event.stopPropagation();
      openMenu(index, true);
    }
    return;
  }
  if (open === null) return;
  const moves: Record<string, () => void> = {
    Escape: () => {
      const button = parts(open ?? 0)?.button;
      closeMenus();
      button?.blur();
    },
    ArrowDown: () => step(1),
    ArrowUp: () => step(-1),
    Home: () => step(1, true),
    End: () => step(-1, true),
    ArrowRight: () => openMenu(((open ?? 0) + 1) % names.length, true),
    ArrowLeft: () => openMenu(((open ?? 0) - 1 + names.length) % names.length, true),
  };
  const move = moves[event.key];
  if (move) {
    event.preventDefault();
    event.stopPropagation();
    move();
  }
}

/** Builds the menu bar. `entries` is asked each time a menu opens. */
export function setupMenus(element: HTMLElement, menuNames: string[], entries: (menu: string) => MenuEntry[]): void {
  bar = element;
  names = menuNames;
  entriesOf = entries;
  menuNames.forEach((name, index) => {
    const wrap = document.createElement("div");
    wrap.className = "menu";
    const button = document.createElement("button");
    button.type = "button";
    button.setAttribute("role", "menuitem");
    button.setAttribute("aria-haspopup", "menu");
    button.setAttribute("aria-expanded", "false");
    const first = document.createElement("u");
    first.textContent = name.slice(0, 1);
    button.append(first, name.slice(1));
    button.addEventListener("click", () => (open === index ? closeMenus() : openMenu(index)));
    button.addEventListener("mouseenter", () => {
      if (open !== null && open !== index) openMenu(index);
    });
    const items = document.createElement("div");
    items.className = "items";
    items.setAttribute("role", "menu");
    items.setAttribute("aria-label", name);
    wrap.append(button, items);
    element.append(wrap);
  });
  document.addEventListener("pointerdown", (event) => {
    if (open !== null && !element.contains(event.target as Node)) closeMenus();
  });
  document.addEventListener("keydown", onKey, true);
  document.addEventListener("keyup", (event) => {
    if (event.key === "Alt") element.classList.remove("keys");
  });
  window.addEventListener("blur", () => {
    element.classList.remove("keys");
    closeMenus();
  });
}
