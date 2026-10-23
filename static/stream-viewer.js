// Built from sdk/ by `npm run build`. Do not edit.

// src/emitter.ts
var Emitter = class {
  #listeners = /* @__PURE__ */ new Map();
  /** Returns a function that removes the listener. */
  on(event, listener) {
    let set = this.#listeners.get(event);
    if (!set) this.#listeners.set(event, set = /* @__PURE__ */ new Set());
    set.add(listener);
    return () => this.off(event, listener);
  }
  off(event, listener) {
    this.#listeners.get(event)?.delete(listener);
  }
  emit(event, value) {
    for (const listener of this.#listeners.get(event) ?? []) {
      try {
        listener(value);
      } catch (error) {
        console.error(error);
      }
    }
  }
};

// src/layers.ts
function initialLayers() {
  return { mid: null, layers: [], current: null, target: null, mode: "auto", described: false };
}
function applyLayerStatus(state, message) {
  if (message.available_simulcast_layers.length === 0) {
    return { ...state, described: true };
  }
  return {
    mid: message.mid,
    layers: message.available_simulcast_layers,
    current: message.current_layer,
    target: message.target_layer,
    mode: message.layer_mode,
    described: true
  };
}
function applyLayerChanged(state, message) {
  if (message.mid !== state.mid) return { state, switched: null };
  const settled = state.mode === "auto" || state.target === message.rid;
  return {
    state: { ...state, current: message.rid, target: settled ? null : state.target },
    switched: { from: state.current, to: message.rid }
  };
}
function requestLayer(state, rid) {
  const mid = state.mid;
  if (mid === null) return { state, messages: [] };
  if (rid === null) {
    return {
      state: { ...state, mode: "auto" },
      messages: [{ type: "set_layer_mode", mid, layer_mode: "auto" }]
    };
  }
  if (!state.layers.some((layer) => layer.rid === rid)) return { state, messages: [] };
  const messages = [];
  if (state.mode !== "manual") messages.push({ type: "set_layer_mode", mid, layer_mode: "manual" });
  messages.push({ type: "set_layer", mid, rid });
  return {
    state: { ...state, mode: "manual", target: rid === state.current ? null : rid },
    messages
  };
}

// src/stats.ts
var num = (value) => typeof value === "number" ? value : 0;
var opt = (value) => typeof value === "number" ? value : null;
function readSnapshot(entries) {
  const all = Array.from(entries);
  const byId = new Map(all.map((entry) => [entry.id, entry]));
  let at = 0;
  let packetsReceived = 0;
  let packetsLost = 0;
  let video = null;
  for (const entry of all) {
    at = Math.max(at, num(entry["timestamp"]));
    if (entry.type !== "inbound-rtp") continue;
    packetsReceived += num(entry["packetsReceived"]);
    packetsLost += Math.max(0, num(entry["packetsLost"]));
    if (entry["kind"] === "video") {
      video = {
        width: num(entry["frameWidth"]),
        height: num(entry["frameHeight"]),
        bytesReceived: num(entry["bytesReceived"]),
        framesPerSecond: opt(entry["framesPerSecond"]),
        framesDropped: num(entry["framesDropped"]),
        freezeCount: num(entry["freezeCount"]),
        freezeSeconds: num(entry["totalFreezesDuration"])
      };
    }
  }
  return { at, rttMs: selectedRtt(all, byId), packetsReceived, packetsLost, video };
}
function selectedRtt(all, byId) {
  const transport = all.find((entry) => entry.type === "transport" && entry["selectedCandidatePairId"]);
  const pair = (transport && byId.get(String(transport["selectedCandidatePairId"]))) ?? all.find((entry) => entry.type === "candidate-pair" && entry["nominated"] && entry["state"] === "succeeded");
  const seconds = pair && opt(pair["currentRoundTripTime"]);
  return seconds === null || seconds === void 0 ? null : seconds * 1e3;
}
function perfReport(previous, next) {
  if (next.rttMs === null) return null;
  const received = next.packetsReceived - previous.packetsReceived;
  const lost = next.packetsLost - previous.packetsLost;
  const expected = received + lost;
  const lossPct = expected > 0 && lost > 0 ? lost / expected * 100 : 0;
  return { rtt_ms: Math.round(next.rttMs), loss_pct: Math.round(lossPct * 100) / 100 };
}
function viewerStats(previous, next) {
  const video = next.video;
  let mbps = null;
  if (previous?.video && video) {
    const seconds = (next.at - previous.at) / 1e3;
    if (seconds > 0) mbps = (video.bytesReceived - previous.video.bytesReceived) * 8 / seconds / 1e6;
  }
  return {
    width: video?.width ?? 0,
    height: video?.height ?? 0,
    framesPerSecond: video?.framesPerSecond ?? null,
    mbps,
    rttMs: next.rttMs,
    freezeCount: video?.freezeCount ?? 0,
    freezeSeconds: video?.freezeSeconds ?? 0,
    packetsLost: next.packetsLost,
    framesDropped: video?.framesDropped ?? 0
  };
}
function freezeNote(previous, next) {
  const before = previous.video;
  const after = next.video;
  if (!before || !after || after.freezeCount <= before.freezeCount) return null;
  const times = after.freezeCount - before.freezeCount;
  const ms = Math.round((after.freezeSeconds - before.freezeSeconds) * 1e3);
  const froze = times === 1 ? `froze ${ms} ms` : `froze ${times} times, ${ms} ms in all`;
  return `${froze}, ${next.packetsLost - previous.packetsLost} packets lost, ${after.framesDropped - before.framesDropped} frames dropped`;
}
function rtpStep(from, to) {
  return ((to - from + 2 ** 31) % 2 ** 32 + 2 ** 32) % 2 ** 32 - 2 ** 31;
}
function timestampDrift(previous, next, clockRate = 9e4) {
  return rtpStep(previous.rtpTimestamp, next.rtpTimestamp) / clockRate * 1e3 - (next.timestamp - previous.timestamp);
}

// src/viewer.ts
var JoinError = class extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
  status;
  name = "JoinError";
};
var DISCONNECTED_GRACE_MS = 5e3;
var GATHERING_TIMEOUT_MS = 2e3;
var TIMESTAMP_PROBE_MS = 250;
var Viewer = class extends Emitter {
  /** Carries the room's tracks across reconnects, so a `<video>` is attached once. */
  stream = new MediaStream();
  #options;
  #state = "idle";
  #layers = initialLayers();
  #debug;
  #pc = null;
  #channel = null;
  #messages = Promise.resolve();
  #attempts = 0;
  #joinedOnce = false;
  #retryTimer;
  #graceTimer;
  #statsTimer;
  #probeTimer;
  #lastSnapshot = null;
  #lastSource = null;
  #asked = null;
  #ignored = /* @__PURE__ */ new Set();
  #joined = null;
  #settleJoined = null;
  constructor(options) {
    super();
    this.#options = {
      room: options.room,
      server: options.server,
      reconnect: options.reconnect ?? true,
      maxRetryDelayMs: options.maxRetryDelayMs ?? 1e4,
      statsIntervalMs: options.statsIntervalMs ?? 1e3,
      debug: options.debug ?? false,
      rtcConfiguration: options.rtcConfiguration
    };
    this.#debug = this.#options.debug;
  }
  get state() {
    return this.#state;
  }
  get layers() {
    return this.#layers;
  }
  get debug() {
    return this.#debug;
  }
  /**
   * Joins the room. Resolves once the server has answered the first offer.
   * With `reconnect`, a room that is not live yet is retried until it is, so
   * the promise waits. Rejects on an error that is not worth retrying, or on
   * `close()`.
   */
  connect() {
    if (this.#state === "closed") return Promise.reject(new Error("The viewer is closed"));
    if (this.#joined) return this.#joined;
    this.#joined = new Promise((resolve, reject) => {
      this.#settleJoined = { resolve, reject };
    });
    this.#joined.catch(() => {
    });
    void this.#attempt();
    return this.#joined;
  }
  /** Points a `<video>` at this viewer's stream. */
  attach(video) {
    video.srcObject = this.stream;
    video.playsInline = true;
    video.autoplay = true;
    return video;
  }
  /** Asks for a simulcast layer. The server switches on the layer's next keyframe. */
  setLayer(rid) {
    this.#request(rid);
  }
  /** Lets the server pick the layer from this viewer's bandwidth estimate. */
  setAutoLayer() {
    this.#request(null);
  }
  setDebug(on) {
    if (this.#debug === on) return;
    this.#debug = on;
    this.#lastSource = null;
    this.#syncProbe();
    if (on) this.#log("debug on");
  }
  close() {
    if (this.#state === "closed") return;
    clearTimeout(this.#retryTimer);
    this.#teardown();
    this.#setState("closed");
    this.#settleJoined?.reject(new Error("The viewer was closed"));
    this.#settleJoined = null;
  }
  async #attempt() {
    if (this.#state !== "not-live") this.#setState(this.#joinedOnce ? "reconnecting" : "connecting");
    try {
      await this.#join();
    } catch (error) {
      this.#teardown();
      if (this.#state === "closed") return;
      const err = error instanceof Error ? error : new Error(String(error));
      if (err instanceof JoinError && err.status === 404) {
        this.#setState("not-live", err.message);
        if (this.#options.reconnect) return this.#retry();
        return this.#fail(err);
      }
      if (this.#retryable(err) && this.#options.reconnect) {
        this.emit("error", err);
        this.#log(`join failed: ${err.message}`);
        return this.#retry();
      }
      return this.#fail(err);
    }
  }
  #retryable(error) {
    if (error instanceof JoinError) return error.status >= 500;
    return true;
  }
  #retry() {
    const delay = Math.min(1e3 * 2 ** this.#attempts, this.#options.maxRetryDelayMs);
    this.#attempts += 1;
    this.#log(`trying again in ${Math.round(delay / 1e3)} s`);
    this.#retryTimer = setTimeout(() => void this.#attempt(), delay);
  }
  #fail(error) {
    this.#setState("failed", error.message);
    this.emit("error", error);
    this.#settleJoined?.reject(error);
    this.#settleJoined = null;
  }
  async #join() {
    const pc = new RTCPeerConnection(this.#options.rtcConfiguration);
    this.#pc = pc;
    this.#layers = initialLayers();
    this.emit("layers", this.#layers);
    pc.ontrack = (event) => {
      for (const old of this.stream.getTracks().filter((t) => t.kind === event.track.kind)) {
        this.stream.removeTrack(old);
      }
      this.stream.addTrack(event.track);
      this.#log(`receiving ${event.track.kind}`);
    };
    pc.onconnectionstatechange = () => this.#onConnectionState(pc);
    const channel = pc.createDataChannel("data");
    this.#channel = channel;
    channel.onmessage = (event) => {
      const message = JSON.parse(String(event.data));
      this.#messages = this.#messages.then(() => this.#handle(pc, message)).catch((error) => this.emit("error", error instanceof Error ? error : new Error(String(error))));
    };
    await pc.setLocalDescription(await pc.createOffer());
    await gathered(pc);
    const body = {
      type: "offer",
      sdp: pc.localDescription?.sdp ?? "",
      room_id: this.#options.room
    };
    const response = await fetch(url(this.#options.server, "offer"), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body)
    });
    if (response.status === 404) throw new JoinError(404, "This room is not live");
    if (!response.ok) throw new JoinError(response.status, `The server refused the offer: ${response.status}`);
    const answer = await response.json();
    if (this.#pc !== pc) return;
    await pc.setRemoteDescription({ type: "answer", sdp: answer.sdp });
    this.#log("offer answered");
  }
  #onConnectionState(pc) {
    if (this.#pc !== pc) return;
    const state = pc.connectionState;
    this.#log(`connection ${state}`);
    if (state === "connected") {
      clearTimeout(this.#graceTimer);
      this.#attempts = 0;
      this.#joinedOnce = true;
      this.#setState("connected");
      this.#startStats();
      this.#settleJoined?.resolve();
      this.#settleJoined = null;
    } else if (state === "disconnected") {
      clearTimeout(this.#graceTimer);
      this.#graceTimer = setTimeout(() => this.#lost(pc, "The connection was lost"), DISCONNECTED_GRACE_MS);
    } else if (state === "failed") {
      this.#lost(pc, "The connection failed");
    }
  }
  #lost(pc, reason) {
    if (this.#pc !== pc || this.#state === "closed") return;
    this.#teardown();
    if (this.#options.reconnect) {
      this.#log(reason);
      this.#retry();
      this.#setState("reconnecting", reason);
    } else {
      this.#fail(new Error(reason));
    }
  }
  async #handle(pc, message) {
    if (this.#pc !== pc) return;
    switch (message.type) {
      case "offer": {
        await pc.setRemoteDescription({ type: "offer", sdp: message.sdp });
        await pc.setLocalDescription(await pc.createAnswer());
        this.#send({ type: "answer", sdp: pc.localDescription?.sdp ?? "" });
        break;
      }
      case "layer_status": {
        this.#layers = applyLayerStatus(this.#layers, message);
        this.emit("layers", this.#layers);
        break;
      }
      case "layer_changed": {
        const { state, switched } = applyLayerChanged(this.#layers, message);
        if (!switched) break;
        this.#layers = state;
        const asked = this.#asked?.rid === switched.to ? this.#asked : null;
        const waitedMs = asked ? Math.round(performance.now() - asked.at) : null;
        if (asked) this.#asked = null;
        if (waitedMs !== null) this.#log(`switched ${switched.from} to ${switched.to}, ${waitedMs} ms after asking`);
        else this.#log(switched.from ? `switched ${switched.from} to ${switched.to}` : `started on ${switched.to}`);
        this.emit("layerchange", { from: switched.from, to: switched.to, waitedMs });
        this.emit("layers", this.#layers);
        break;
      }
      default: {
        if (!this.#ignored.has(message.type)) {
          this.#ignored.add(message.type);
          this.#log(`${message.type} ignored, relay is not supported yet`);
        }
      }
    }
  }
  #request(rid) {
    const { state, messages } = requestLayer(this.#layers, rid);
    if (messages.length === 0) return;
    this.#asked = rid !== null && rid !== this.#layers.current ? { rid, at: performance.now() } : null;
    this.#log(rid === null ? "asked for auto" : `asked for ${rid}`);
    this.#layers = state;
    for (const message of messages) this.#send(message);
    this.emit("layers", this.#layers);
  }
  #send(message) {
    if (this.#channel?.readyState === "open") this.#channel.send(JSON.stringify(message));
  }
  #startStats() {
    clearInterval(this.#statsTimer);
    this.#lastSnapshot = null;
    this.#statsTimer = setInterval(() => void this.#readStats(), this.#options.statsIntervalMs);
    this.#syncProbe();
  }
  async #readStats() {
    const pc = this.#pc;
    if (!pc) return;
    const report = await pc.getStats();
    const next = readSnapshot(Array.from(report.values()));
    const previous = this.#lastSnapshot;
    this.#lastSnapshot = next;
    if (this.#pc !== pc) return;
    if (previous) {
      const perf = perfReport(previous, next);
      if (perf) this.#send({ type: "perf_report", ...perf });
      const freezes = freezeNote(previous, next);
      if (freezes) this.#log(freezes);
      const before = previous.video;
      const after = next.video;
      if (after?.width && (before?.width !== after.width || before?.height !== after.height)) {
        this.#log(`picture is now ${after.width}x${after.height}`);
      }
    }
    this.emit("stats", viewerStats(previous, next));
  }
  #syncProbe() {
    clearInterval(this.#probeTimer);
    if (this.#debug && this.#pc) {
      this.#probeTimer = setInterval(() => this.#probeTimestamp(), TIMESTAMP_PROBE_MS);
    }
  }
  // Read from the receiver, not from drawn frames: a browser draws a window
  // nobody can see about once a second.
  #probeTimestamp() {
    const receiver = this.#pc?.getReceivers().find((r) => r.track?.kind === "video");
    const source = receiver?.getSynchronizationSources().sort((a, b) => b.timestamp - a.timestamp)[0];
    if (!source || source.rtpTimestamp === void 0) return;
    const now = { timestamp: source.timestamp, rtpTimestamp: source.rtpTimestamp };
    if (!this.#lastSource) {
      this.#log("reading RTP timestamps");
    } else if (now.timestamp !== this.#lastSource.timestamp) {
      const drift = timestampDrift(this.#lastSource, now);
      if (Math.abs(drift) > 1e3) {
        this.#log(`RTP timestamp jumped ${Math.round(Math.abs(drift) / 1e3)} s ${drift > 0 ? "forward" : "back"}`);
      }
    }
    this.#lastSource = now;
  }
  #teardown() {
    clearTimeout(this.#graceTimer);
    clearInterval(this.#statsTimer);
    clearInterval(this.#probeTimer);
    const pc = this.#pc;
    this.#pc = null;
    this.#channel = null;
    this.#messages = Promise.resolve();
    this.#lastSnapshot = null;
    this.#lastSource = null;
    this.#asked = null;
    if (pc) {
      pc.ontrack = null;
      pc.onconnectionstatechange = null;
      pc.close();
    }
    for (const track of this.stream.getTracks()) {
      track.stop();
      this.stream.removeTrack(track);
    }
  }
  #setState(state, reason) {
    if (this.#state === state) return;
    this.#state = state;
    this.emit("state", reason === void 0 ? { state } : { state, reason });
  }
  #log(text) {
    if (this.#debug) this.emit("log", { at: /* @__PURE__ */ new Date(), text });
  }
};
function url(server, path) {
  const base = server ?? globalThis.location.origin;
  return `${base.replace(/\/+$/, "")}/${path}`;
}
function gathered(pc) {
  if (pc.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((resolve) => {
    const done = () => {
      if (pc.iceGatheringState === "complete") resolve();
    };
    pc.addEventListener("icegatheringstatechange", done);
    setTimeout(resolve, GATHERING_TIMEOUT_MS);
  });
}

// src/player.ts
var STYLE_ID = "stream-viewer-player-style";
var STYLE = `
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
function createPlayer(container, options) {
  injectStyle(container.ownerDocument);
  const viewer = new Viewer(options);
  const doc = container.ownerDocument;
  const make = (tag, className, text = "") => {
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
  let lastStats = null;
  const note = (text, at = /* @__PURE__ */ new Date()) => {
    if (!viewer.debug) return;
    log.textContent += `${clockText(at)}  ${text}
`;
    log.scrollTop = log.scrollHeight;
  };
  const renderStatus = (state, reason) => {
    const text = statusText(state, options.room, reason);
    status.textContent = text;
    banner.textContent = state === "connected" ? "" : text;
  };
  const renderStats = () => {
    const layers = viewer.layers;
    let text = lastStats?.width ? `${lastStats.width}x${lastStats.height}` : "no video";
    if (lastStats?.framesPerSecond) text += `  ${Math.round(lastStats.framesPerSecond)} fps`;
    if (lastStats?.mbps != null) text += `
${lastStats.mbps.toFixed(2)} Mbps`;
    if (layers.current) text += `
layer ${layers.current}, ${layers.mode}`;
    if (layers.target) text += `
waiting for a keyframe of ${layers.target}`;
    if (viewer.debug && lastStats) {
      text += `
freezes ${lastStats.freezeCount}, ${lastStats.freezeSeconds.toFixed(1)} s`;
      if (lastStats.rttMs !== null) text += `
rtt ${Math.round(lastStats.rttMs)} ms`;
    }
    stats.textContent = text;
  };
  const renderLayers = (layers) => {
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
      const button = make("button", "", bitrate_estimate ? `${rid}  ${Math.round(bitrate_estimate / 1e3)} kbps` : rid);
      button.type = "button";
      button.setAttribute("aria-pressed", String(layers.mode === "manual" && layers.current === rid));
      button.classList.toggle("sv-picked", layers.mode === "auto" && layers.current === rid);
      button.classList.toggle("sv-pending", layers.target === rid);
      button.onclick = () => viewer.setLayer(rid);
      layersBox.append(button);
    }
  };
  const setDebug = (on) => {
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
    viewer.on("log", ({ at, text }) => note(text, at))
  ];
  sound.onclick = () => {
    video.muted = !video.muted;
    sound.textContent = video.muted ? "Unmute" : "Mute";
    if (!video.muted) void video.play().catch(() => {
    });
  };
  debugButton.onclick = () => {
    const on = !viewer.debug;
    setDebug(on);
    options.onDebugChange?.(on);
  };
  const onVisibility = () => note(doc.hidden ? "page hidden" : "page visible");
  const onBlur = () => note("window lost focus");
  const onFocus = () => note("window focused");
  const win = doc.defaultView;
  doc.addEventListener("visibilitychange", onVisibility);
  win?.addEventListener("blur", onBlur);
  win?.addEventListener("focus", onFocus);
  let frame = 0;
  const tick = () => {
    clock.textContent = clockText(/* @__PURE__ */ new Date());
    frame = requestAnimationFrame(tick);
  };
  if (options.clock) tick();
  setDebug(options.debug ?? false);
  renderLayers(viewer.layers);
  renderStatus(viewer.state);
  void viewer.connect().catch(() => {
  });
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
    }
  };
}
function statusText(state, room, reason) {
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
function clockText(now) {
  const pad = (n, width = 2) => String(n).padStart(width, "0");
  return `${pad(now.getHours())}:${pad(now.getMinutes())}:${pad(now.getSeconds())}.${pad(now.getMilliseconds(), 3)}`;
}
function injectStyle(doc) {
  if (doc.getElementById(STYLE_ID)) return;
  const style = doc.createElement("style");
  style.id = STYLE_ID;
  style.textContent = STYLE;
  doc.head.append(style);
}
export {
  JoinError,
  Viewer,
  createPlayer
};
