// The small pictures of the interface: the dot-grid icons of the screens
// and the line icons of the buttons. All are drawn here as inline SVG.

type Dot = [column: number, row: number, color: string, step?: number];

/** A 5 by 5 dot grid. A dot with a step blinks while its screen is selected. */
function grid(dots: Dot[]): string {
  const rects = dots
    .map(([column, row, color, step]) => {
      const blink = step === undefined ? "" : ` class="blink" style="--step:${step}"`;
      return `<rect x="${column * 4}" y="${row * 4}" width="3" height="3" fill="${color}"${blink}/>`;
    })
    .join("");
  return `<svg class="dots" viewBox="0 0 19 19" aria-hidden="true">${rects}</svg>`;
}

function shape(lines: string[], colors: Record<string, string>): string {
  const dots: Dot[] = [];
  let count = 0;
  lines.forEach((line, row) => {
    [...line].forEach((cell, column) => {
      const color = colors[cell.toLowerCase()];
      if (!color) return;
      // An upper-case cell blinks. Each one takes the next step.
      dots.push(cell !== cell.toLowerCase() ? [column, row, color, count++] : [column, row, color]);
    });
  });
  return grid(dots);
}

const cyan = "var(--cyan)";
const red = "var(--red)";
const green = "var(--green)";
const purple = "var(--purple)";

/** The pulse of the program icon: reach 0, 1, 2, 1, 0 around the middle row. */
function pulse(): string {
  const reach = [0, 1, 2, 1, 0];
  const dots: Dot[] = [];
  reach.forEach((extent, column) => {
    const color = column < 2 ? cyan : column > 2 ? green : red;
    for (let row = 2 - extent; row <= 2 + extent; row++) {
      const tip = Math.abs(row - 2) === extent;
      dots.push(tip ? [column, row, color, column] : [column, row, color]);
    }
  });
  return grid(dots);
}

/** The icon of each screen and of the activity button. */
export const dotIcons = {
  home: shape(["..X..", ".xxx.", "xxxxx", ".x.x.", ".x.x."], { x: "currentColor" }),
  audio: pulse(),
  video: shape(["ggggg", "g..Rg", "g...g", "ggggg", ".ggg."], { g: green, r: red }),
  screenshot: shape(["Wp.pW", "p...p", ".....", "p...p", "Wp.pW"], { p: purple, w: "#ffffff" }),
  activity: shape([".xxx.", "x...x", "x.O.x", "x...x", ".xxx."], { x: "currentColor", o: "currentColor" }),
};

const line = (path: string) => `<svg class="line" viewBox="0 0 16 16" aria-hidden="true"><path d="${path}"/></svg>`;

export const lineIcons = {
  list: line("M2 4h12M2 8h12M2 12h12"),
  details: line("M2 4h3M7 4h7M2 8h3M7 8h7M2 12h3M7 12h7"),
  thumbs: line("M2 2h5v5H2zM9 2h5v5H9zM2 9h5v5H2zM9 9h5v5H9z"),
  folder: line("M2 4h4l2 2h6v7H2z"),
  trash: line("M3 4h10M6 4V2h4v2M4 4l1 10h6l1-10"),
  open: line("M6 3H3v10h10v-3M9 3h4v4M13 3L7 9"),
  edit: line("M3 13l1-3 7-7 2 2-7 7z"),
  keyboard: line("M1 4h14v8H1zM4 7h1M7 7h2M11 7h1M5 10h6"),
  jump: line("M3 8h10M9 4l4 4-4 4"),
  close: line("M4 4l8 8M12 4l-8 8"),
};
