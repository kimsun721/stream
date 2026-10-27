import { build } from "esbuild";

const common = { bundle: true, format: "esm", target: "es2022", logLevel: "warning" };

// The package: two entry points sharing one chunk, so a page importing both
// gets one Viewer class.
await build({
  ...common,
  entryPoints: { index: "src/index.ts", player: "src/player.ts" },
  outdir: "dist",
  splitting: true,
});

// The server's copy, one file. Checked in, and CI fails if it drifts from the
// sources.
await build({
  ...common,
  entryPoints: ["src/bundle.ts"],
  outfile: "../static/stream-viewer.js",
  banner: { js: "// Built from sdk/ by `npm run build`. Do not edit." },
});
