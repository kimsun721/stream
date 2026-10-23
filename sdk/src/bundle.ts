// The single file the server serves at /stream-viewer.js, for pages without a
// bundler. The npm package ships the entry points separately instead.
export * from "./index.js";
export { createPlayer, type Player, type PlayerOptions } from "./player.js";
