// Builds web/src into web/dist, which the Rust binary serves (and embeds in
// release builds). `node build.mjs --watch` rebuilds on save.
import { copyFile, mkdir, rm } from "node:fs/promises";
import * as esbuild from "esbuild";

const watch = process.argv.includes("--watch");

await rm("dist", { recursive: true, force: true });
await mkdir("dist", { recursive: true });
await copyFile("src/index.html", "dist/index.html");

const options = {
  entryPoints: { app: "src/main.tsx", styles: "src/styles.css" },
  jsx: "automatic",
  jsxImportSource: "preact",
  outdir: "dist",
  bundle: true,
  format: "esm",
  target: "es2022",
  sourcemap: true,
  minify: !watch,
  logLevel: "info",
};

if (watch) {
  const ctx = await esbuild.context(options);
  await ctx.watch();
} else {
  await esbuild.build(options);
}
