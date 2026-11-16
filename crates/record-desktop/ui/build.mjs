// Builds the interface into `dist`, the folder that the desktop program
// embeds. Run `npm run build` after a change in `src`.
import { build } from "esbuild";
import { copyFile, mkdir } from "node:fs/promises";

await mkdir("dist", { recursive: true });
await build({
  entryPoints: ["src/main.ts"],
  bundle: true,
  format: "iife",
  target: "es2022",
  minify: true,
  legalComments: "none",
  outfile: "dist/app.js",
});
await copyFile("src/index.html", "dist/index.html");
await copyFile("src/styles.css", "dist/styles.css");
