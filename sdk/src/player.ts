import type { LayerState } from "./layers.js";
import type { ViewerStats } from "./stats.js";
import { Viewer, type ViewerOptions, type ViewerState } from "./viewer.js";

export interface PlayerOptions extends ViewerOptions {
  /**
   * Show the wall clock over the picture. Point a camera at a clock, or show
   * one in the source, and the two side by side give the end to end delay.
   */
  clock?: boolean;
  /** Show the Debug button. Defaults to true. */
  debugToggle?: boolean;
  /** Start muted, which browsers require to play without a click. Defaults to true. */
  muted?: boolean;
  /** Called when the Debug button changes the mode. */
  onDebugChange?: (on: boolean) => void;
}

export interface Player {
  readonly viewer: Viewer;
  readonly element: HTMLElement;
  /** Closes the viewer and removes the player from the page. */
  destroy(): void;
}

const STYLE_ID = "stream-viewer-player-style";

// Colors are custom properties on the player, so a page can restyle it with
// its own values without touching the rules.
const STYLE = `
.sv-player {
  --sv-bg: #0f1113;
  --sv-panel: #181b1e;
  --sv-line: #2b3035;
  --sv-ink: #e7e9eb;
  --sv-muted: #8d959c;
  --sv-accent: #5cc2b4;
  --sv-mono: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  display: grid;
  gap: 12px;
  color: var(--sv-ink);
  font: 14px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif;
}
.sv-player * { box-sizing: border-box; }
.sv-stage { position: relative; aspect-ratio: 16 / 9; background: #000; border-radius: 8px; overflow: hidden; }
.sv-stage video { display: block; width: 100%; height: 100%; object-fit: contain; }
.sv-readout {
  position: absolute; top: 12px; padding: 6px 10px; border-radius: 4px;
  background: rgba(0, 0, 0, 0.62); font: 12px/1.5 var(--sv-mono); white-space: pre;
}
.sv-stats { left: 12px; }
.sv-clock { right: 12px; font-size: 18px; font-weight: 600; }
.sv-banner {
  position: absolute; inset: 0; display: grid; place-items: center; padding: 16px;
  color: var(--sv-muted); text-align: center; pointer-events: none;
}
.sv-controls { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
.sv-label { color: var(--sv-muted); margin-right: 4px; }
.sv-layers { display: flex; flex-wrap: wrap; gap: 6px; color: var(--sv-muted); }
.sv-player button {
  padding: 8px 14px; background: var(--sv-panel); border: 1px solid var(--sv-line); border-radius: 6px;
  color: var(--sv-ink); font: inherit; cursor: pointer;
}
.sv-player button:hover { border-color: var(--sv-muted); }
.sv-player button:focus-visible { outline: 2px solid var(--sv-accent); outline-offset: 1px; }
.sv-player button[aria-pressed="true"] { background: var(--sv-accent); border-color: var(--sv-accent); color: #0b0d0e; }
.sv-player button.sv-picked { border-color: var(--sv-accent); }
.sv-player button.sv-pending { border-color: var(--sv-accent); border-style: dashed; }
.sv-sound { margin-left: auto; }
.sv-player .sv-debug { background: none; border-color: transparent; color: var(--sv-muted); }
.sv-player .sv-debug:hover { border-color: var(--sv-line); }
.sv-player .sv-debug[aria-pressed="true"] { background: none; border-color: var(--sv-line); color: var(--sv-ink); }
.sv-status { margin: 0; color: var(--sv-muted); }
.sv-log {
  margin: 0; max-height: 260px; overflow: auto; padding: 10px 12px;
  background: var(--sv-panel); border: 1px solid var(--sv-line); border-radius: 6px;
  font: 12px/1.6 var(--sv-mono); white-space: pre-wrap;
}
`;

/** Builds a player inside `container` and starts watching. */
export function createPlayer(container: HTMLElement, options: PlayerOptions): Player {
  injectStyle(container.ownerDocument);

  const viewer = new Viewer(options);
  const doc = container.ownerDocument;
  const make = <K extends keyof HTMLElementTagNameMap>(tag: K, className: string, text = "") => {
    const element = doc.createElement(tag);
    element.className = className;
    element.textContent = text;
    return element;
  };

  const root = make("div", "sv-player");
  const stage = make("div", "sv-stage");
  const video = viewer.attach(make("video", ""));
  video.muted = options.muted ?? true;
  const stats = make("div", "sv-readout sv-stats", "no stream");
  const clock = make("div", "sv-readout sv-clock");
  const banner = make("div", "sv-banner");
  stage.append(video, stats, banner);
  if (options.clock) stage.append(clock);

  const controls = make("div", "sv-controls");
  const layersBox = make("div", "sv-layers");
  const sound = make("button", "sv-sound", video.muted ? "Unmute" : "Mute");
  const debugButton = make("button", "sv-debug", "Debug");
  sound.type = "button";
  debugButton.type = "button";
  controls.append(make("span", "sv-label", "Layer"), layersBox, sound);
  if (options.debugToggle ?? true) controls.append(debugButton);

  const status = make("p", "sv-status");
  const log = make("pre", "sv-log");

  root.append(stage, controls, status, log);
  container.append(root);

  let lastStats: ViewerStats | null = null;

  const note = (text: string, at = new Date()) => {
    if (!viewer.debug) return;
    log.textContent += `${clockText(at)}  ${text}\n`;
    log.scrollTop = log.scrollHeight;
  };

  const renderStatus = (state: ViewerState, reason?: string) => {
    const text = statusText(state, options.room, reason);
    status.textContent = text;
    banner.textContent = state === "connected" ? "" : text;
  };

  const renderStats = () => {
    const layers = viewer.layers;
    let text = lastStats?.width ? `${lastStats.width}x${lastStats.height}` : "no video";
    if (lastStats?.framesPerSecond) text += `  ${Math.round(lastStats.framesPerSecond)} fps`;
    if (lastStats?.mbps != null) text += `\n${lastStats.mbps.toFixed(2)} Mbps`;
    if (layers.current) text += `\nlayer ${layers.current}, ${layers.mode}`;
    if (layers.target) text += `\nwaiting for a keyframe of ${layers.target}`;
    if (viewer.debug && lastStats) {
      text += `\nfreezes ${lastStats.freezeCount}, ${lastStats.freezeSeconds.toFixed(1)} s`;
      if (lastStats.rttMs !== null) text += `\nrtt ${Math.round(lastStats.rttMs)} ms`;
    }
    stats.textContent = text;
  };

  const renderLayers = (layers: LayerState) => {
    layersBox.replaceChildren();

    if (!layers.mid) {
      layersBox.textContent = layers.described ? "single layer" : "none yet";
      return;
    }

    const auto = make("button", "", "Auto");
    auto.type = "button";
    auto.setAttribute("aria-pressed", String(layers.mode === "auto"));
    auto.onclick = () => viewer.setAutoLayer();
    layersBox.append(auto);

    for (const { rid, bitrate_estimate } of layers.layers) {
      const button = make("button", "", bitrate_estimate ? `${rid}  ${Math.round(bitrate_estimate / 1000)} kbps` : rid);
      button.type = "button";
      button.setAttribute("aria-pressed", String(layers.mode === "manual" && layers.current === rid));
      button.classList.toggle("sv-picked", layers.mode === "auto" && layers.current === rid);
      button.classList.toggle("sv-pending", layers.target === rid);
      button.onclick = () => viewer.setLayer(rid);
      layersBox.append(button);
    }
  };

  const setDebug = (on: boolean) => {
    viewer.setDebug(on);
    debugButton.setAttribute("aria-pressed", String(on));
    log.hidden = !on;
    renderStats();
  };

  const unsubscribe = [
    viewer.on("state", ({ state, reason }) => renderStatus(state, reason)),
    viewer.on("layers", (layers) => {
      renderLayers(layers);
      renderStats();
    }),
    viewer.on("stats", (next) => {
      lastStats = next;
      renderStats();
    }),
    viewer.on("log", ({ at, text }) => note(text, at)),
  ];

  sound.onclick = () => {
    video.muted = !video.muted;
    sound.textContent = video.muted ? "Unmute" : "Mute";
    if (!video.muted) void video.play().catch(() => {});
  };

  debugButton.onclick = () => {
    const on = !viewer.debug;
    setDebug(on);
    options.onDebugChange?.(on);
  };

  // Page events that explain a stall the stream itself does not.
  const onVisibility = () => note(doc.hidden ? "page hidden" : "page visible");
  const onBlur = () => note("window lost focus");
  const onFocus = () => note("window focused");
  const win = doc.defaultView;
  doc.addEventListener("visibilitychange", onVisibility);
  win?.addEventListener("blur", onBlur);
  win?.addEventListener("focus", onFocus);

  let frame = 0;
  const tick = () => {
    clock.textContent = clockText(new Date());
    frame = requestAnimationFrame(tick);
  };
  if (options.clock) tick();

  setDebug(options.debug ?? false);
  renderLayers(viewer.layers);
  renderStatus(viewer.state);
  void viewer.connect().catch(() => {});

  return {
    viewer,
    element: root,
    destroy() {
      cancelAnimationFrame(frame);
      for (const off of unsubscribe) off();
      doc.removeEventListener("visibilitychange", onVisibility);
      win?.removeEventListener("blur", onBlur);
      win?.removeEventListener("focus", onFocus);
      viewer.close();
      root.remove();
    },
  };
}

function statusText(state: ViewerState, room: string, reason?: string): string {
  switch (state) {
    case "idle":
    case "connecting":
      return "Connecting...";
    case "connected":
      return `Watching ${room}`;
    case "reconnecting":
      return reason ? `${reason}. Reconnecting...` : "Reconnecting...";
    case "not-live":
      return "This room is not live. Waiting for it to start...";
    case "failed":
      return reason ?? "Could not connect.";
    case "closed":
      return "Stopped.";
  }
}

function clockText(now: Date): string {
  const pad = (n: number, width = 2) => String(n).padStart(width, "0");
  return `${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}.${pad(now.getMilliseconds(), 3)}`;
}

function injectStyle(doc: Document): void {
  if (doc.getElementById(STYLE_ID)) return;
  const style = doc.createElement("style");
  style.id = STYLE_ID;
  style.textContent = STYLE;
  doc.head.append(style);
}
