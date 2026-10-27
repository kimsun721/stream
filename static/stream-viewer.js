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

// src/relay.ts
var num = (value) => typeof value === "number" ? value : 0;
function readSent(entries) {
  let at = 0;
  let bytesSent = 0;
  for (const entry of entries) {
    at = Math.max(at, num(entry["timestamp"]));
    if (entry.type === "outbound-rtp") bytesSent += num(entry["bytesSent"]);
  }
  return { at, bytesSent };
}
function outgoingKbps(previous, next) {
  const seconds = (next.at - previous.at) / 1e3;
  if (seconds <= 0) return null;
  return Math.max(0, Math.round((next.bytesSent - previous.bytesSent) * 8 / seconds / 1e3));
}
var CEILING_SHARE = 1.2;
var FLOOR_SHARE = 0.8;
function relayVideoCeiling(receivedKbps) {
  return Math.round(receivedKbps * CEILING_SHARE * 1e3);
}
var VIDEO_CODECS = /* @__PURE__ */ new Set(["vp8", "vp9", "h264", "av1", "h265"]);
function withBitrateHints(sdp, kbps) {
  const params = `x-google-start-bitrate=${kbps};x-google-min-bitrate=${Math.round(kbps * FLOOR_SHARE)}`;
  const eol = sdp.includes("\r\n") ? "\r\n" : "\n";
  const lines = sdp.split(eol);
  const out = [];
  let video = false;
  const withFmtp = /* @__PURE__ */ new Set();
  for (const line of lines) {
    if (line.startsWith("m=")) video = line.startsWith("m=video");
    else if (video && line.startsWith("a=fmtp:")) withFmtp.add(line.slice(7, line.indexOf(" ")));
  }
  video = false;
  for (const line of lines) {
    if (line.startsWith("m=")) video = line.startsWith("m=video");
    out.push(line);
    if (!video) continue;
    const rtpmap = /^a=rtpmap:(\d+) ([^/]+)\//.exec(line);
    if (rtpmap && VIDEO_CODECS.has(rtpmap[2].toLowerCase()) && !withFmtp.has(rtpmap[1])) {
      out.push(`a=fmtp:${rtpmap[1]} ${params}`);
    }
    const fmtp = /^a=fmtp:(\d+) /.exec(line);
    if (fmtp && isVideoCodec(lines, fmtp[1])) {
      out[out.length - 1] = `${line};${params}`;
    }
  }
  return out.join(eol);
}
function isVideoCodec(lines, pt) {
  const rtpmap = lines.find((l) => l.startsWith(`a=rtpmap:${pt} `));
  const name = rtpmap ? /^a=rtpmap:\d+ ([^/]+)\//.exec(rtpmap)?.[1] : void 0;
  return !!name && VIDEO_CODECS.has(name.toLowerCase());
}

// src/stats.ts
var num2 = (value) => typeof value === "number" ? value : 0;
var opt = (value) => typeof value === "number" ? value : null;
function readSnapshot(entries) {
  const all = Array.from(entries);
  const byId = new Map(all.map((entry) => [entry.id, entry]));
  let at = 0;
  let packetsReceived = 0;
  let packetsLost = 0;
  let video = null;
  for (const entry of all) {
    at = Math.max(at, num2(entry["timestamp"]));
    if (entry.type !== "inbound-rtp") continue;
    packetsReceived += num2(entry["packetsReceived"]);
    packetsLost += Math.max(0, num2(entry["packetsLost"]));
    if (entry["kind"] === "video") {
      video = {
        width: num2(entry["frameWidth"]),
        height: num2(entry["frameHeight"]),
        bytesReceived: num2(entry["bytesReceived"]),
        framesPerSecond: opt(entry["framesPerSecond"]),
        framesDecoded: num2(entry["framesDecoded"]),
        framesDropped: num2(entry["framesDropped"]),
        freezeCount: num2(entry["freezeCount"]),
        freezeSeconds: num2(entry["totalFreezesDuration"])
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
var RECEIVED_WINDOW = 5;
var HANDOVER_TIMEOUT_MS = 1e4;
var HANDOVER_POLL_MS = 100;
var RELAY_SILENCE_MS = 1e3;
var RELAY_WATCH_MS = 250;
var Viewer = class extends Emitter {
  /** Carries the room's tracks across reconnects, so a `<video>` is attached once. */
  stream = new MediaStream();
  #options;
  #state = "idle";
  #layers = initialLayers();
  #debug;
  #pc = null;
  #channel = null;
  #sfuTracks = /* @__PURE__ */ new Map();
  // At most one of the two links is open: a relay's to its leaf, or a leaf's
  // from its relay.
  #role = null;
  #linked = false;
  #relayLink = null;
  #leafLink = null;
  #relayTracks = /* @__PURE__ */ new Map();
  #lastSent = null;
  #linking = false;
  #watchTimer;
  // Video received from the server over the last few seconds, which a relay
  // holds its encoder to. The highest of them, so a freeze just before a
  // promotion does not start the relay near zero.
  #received = [];
  #lastLeafSnapshot = null;
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
      rtcConfiguration: options.rtcConfiguration,
      relay: options.relay ?? true
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
  get role() {
    return { role: this.#role, linked: this.#linked };
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
      const track = event.track;
      this.#sfuTracks.set(track.kind, track);
      if (!this.#onRelayStream()) this.#show(track);
      const sender = this.#relayLink?.getSenders().find((s) => s.track?.kind === track.kind);
      if (sender) void sender.replaceTrack(track).catch((error) => this.emit("error", error));
      this.#log(`receiving ${track.kind}`);
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
      case "probe_available_upload": {
        if (!this.#ignored.has(message.type)) {
          this.#ignored.add(message.type);
          this.#log("upload probe ignored, the server is to measure upload itself");
        }
        break;
      }
      case "request_offer": {
        await this.#offerToLeaf();
        break;
      }
      case "p2p_answer": {
        const link = this.#relayLink;
        if (!link || link.signalingState !== "have-local-offer") break;
        const kbps = this.#receivedKbps();
        const sdp = kbps ? withBitrateHints(message.sdp, kbps) : message.sdp;
        await link.setRemoteDescription({ type: "answer", sdp });
        break;
      }
      case "p2p_offer": {
        await this.#answerRelay(message.sdp);
        break;
      }
      case "demote": {
        this.#dropLink("demoted, back on the server's stream");
        break;
      }
    }
  }
  // The relay side. The server has promoted this viewer and pairs it with a
  // leaf, so this viewer offers the leaf what it receives.
  async #offerToLeaf() {
    if (!this.#options.relay) {
      this.#log("asked to relay, declined by the relay option");
      return;
    }
    this.#dropLink();
    const link = new RTCPeerConnection(this.#options.rtcConfiguration);
    this.#relayLink = link;
    this.#setRole("relay", false);
    const kbps = this.#receivedKbps();
    for (const track of this.#sfuTracks.values()) {
      const ceiling = track.kind === "video" && kbps ? relayVideoCeiling(kbps) : null;
      link.addTransceiver(track, {
        direction: "sendonly",
        streams: [this.stream],
        ...ceiling === null ? {} : { sendEncodings: [{ maxBitrate: ceiling }] }
      });
    }
    link.onconnectionstatechange = () => {
      if (this.#relayLink !== link) return;
      const state = link.connectionState;
      this.#log(`link to leaf ${state}`);
      if (state === "connected") this.#setRole("relay", true);
      else if (this.#linked) this.#setRole("relay", false);
    };
    await link.setLocalDescription(await link.createOffer());
    await gathered(link);
    if (this.#relayLink !== link) return;
    this.#send({ type: "p2p_offer", sdp: link.localDescription?.sdp ?? "" });
    this.#log("promoted to relay, offered a link to the leaf");
  }
  // Follows the layer this viewer receives, which changes with its own
  // bandwidth and its picks.
  async #holdRelayCeiling(link) {
    const sender = link.getSenders().find((s) => s.track?.kind === "video");
    const kbps = this.#receivedKbps();
    if (!sender || !kbps) return;
    const ceiling = relayVideoCeiling(kbps);
    const parameters = sender.getParameters();
    const encoding = parameters.encodings[0];
    const current = encoding?.maxBitrate;
    if (!encoding || current && Math.abs(current - ceiling) < current * 0.1) return;
    encoding.maxBitrate = ceiling;
    await sender.setParameters(parameters);
  }
  // The leaf side. The server has paired this viewer with a relay, which
  // offers a link. The server keeps sending until the link is up.
  async #answerRelay(sdp) {
    this.#dropLink();
    const link = new RTCPeerConnection(this.#options.rtcConfiguration);
    this.#leafLink = link;
    this.#setRole("leaf", false);
    link.ontrack = (event) => {
      this.#relayTracks.set(event.track.kind, event.track);
      if (this.#onRelayStream()) this.#show(event.track);
    };
    link.onconnectionstatechange = () => void this.#onLeafLink(link);
    await link.setRemoteDescription({ type: "offer", sdp });
    await link.setLocalDescription(await link.createAnswer());
    await gathered(link);
    if (this.#leafLink !== link) return;
    this.#send({ type: "p2p_answer", sdp: link.localDescription?.sdp ?? "" });
    this.#log("paired with a relay, answered its link");
  }
  async #onLeafLink(link) {
    if (this.#leafLink !== link) return;
    const state = link.connectionState;
    this.#log(`link from relay ${state}`);
    if (state === "connected" && !this.#linked && !this.#linking) {
      this.#linking = true;
      const ready = await decodedPast(link, 0, HANDOVER_TIMEOUT_MS);
      this.#linking = false;
      if (this.#leafLink !== link) return;
      if (!ready) {
        this.#log("the relay sent no picture, staying on the server's stream");
        return;
      }
      for (const track of this.#relayTracks.values()) this.#show(track);
      this.#setRole("leaf", true);
      this.#lastLeafSnapshot = null;
      this.#send({ type: "p2p_connected" });
      this.#watchRelay(link);
      this.#log("watching through the relay");
    } else if ((state === "disconnected" || state === "failed") && this.#linked) {
      this.#relayLost();
    } else if (state === "failed") {
      this.#dropLink("could not reach the relay");
    }
  }
  #watchRelay(link) {
    clearInterval(this.#watchTimer);
    let packets = -1;
    let quietSince = performance.now();
    this.#watchTimer = setInterval(async () => {
      const received = readSnapshot(Array.from((await link.getStats()).values())).packetsReceived;
      if (this.#leafLink !== link || !this.#linked) return;
      if (received !== packets) {
        packets = received;
        quietSince = performance.now();
      } else if (performance.now() - quietSince >= RELAY_SILENCE_MS) {
        this.#relayLost();
      }
    }, RELAY_WATCH_MS);
  }
  #relayLost() {
    this.#send({ type: "p2p_disconnected" });
    this.#dropLink("lost the relay, back on the server's stream");
  }
  /** Closes whichever relay link is open and plays the server's stream again. */
  #dropLink(reason) {
    if (this.#role === null) return;
    if (reason) this.#log(reason);
    const wasOnRelay = this.#onRelayStream();
    const links = [this.#relayLink, this.#leafLink];
    const relayTracks = [...this.#relayTracks.values()];
    clearInterval(this.#watchTimer);
    this.#relayLink = null;
    this.#leafLink = null;
    this.#relayTracks.clear();
    this.#linking = false;
    this.#lastSent = null;
    this.#lastLeafSnapshot = null;
    this.#setRole(null, false);
    const close = () => {
      for (const link of links) {
        if (!link) continue;
        link.ontrack = null;
        link.onconnectionstatechange = null;
        link.close();
      }
      for (const track of relayTracks) track.stop();
    };
    const audio = this.#sfuTracks.get("audio");
    if (audio) this.#show(audio);
    const pc = this.#pc;
    if (!wasOnRelay || !pc) {
      for (const track of this.#sfuTracks.values()) this.#show(track);
      close();
      return;
    }
    void framesDecoded(pc).then((before) => decodedPast(pc, before, HANDOVER_TIMEOUT_MS)).then((ready) => {
      const video = this.#sfuTracks.get("video");
      if (this.#pc === pc && !this.#onRelayStream() && video) this.#show(video);
      if (!ready) this.#log("the server's stream sent no picture in time");
      close();
    });
  }
  #receivedKbps() {
    return this.#received.length ? Math.max(...this.#received) : null;
  }
  #onRelayStream() {
    return this.#role === "leaf" && this.#linked;
  }
  /** Puts `track` in the attached stream in place of any track of its kind. */
  #show(track) {
    for (const old of this.stream.getTracks()) {
      if (old.kind === track.kind && old !== track) this.stream.removeTrack(old);
    }
    if (!this.stream.getTracks().includes(track)) this.stream.addTrack(track);
  }
  #setRole(role, linked) {
    if (this.#role === role && this.#linked === linked) return;
    this.#role = role;
    this.#linked = linked;
    this.#lastSource = null;
    this.emit("role", { role, linked });
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
      const mbps = viewerStats(previous, next).mbps;
      if (mbps !== null && !this.#onRelayStream()) {
        this.#received = [...this.#received.slice(-(RECEIVED_WINDOW - 1)), Math.round(mbps * 1e3)];
      }
    }
    if (this.#relayLink) {
      await this.#reportOutgoing(this.#relayLink);
      await this.#holdRelayCeiling(this.#relayLink).catch((error) => this.emit("error", error));
    }
    const shown = this.#onRelayStream() && this.#leafLink ? await this.#readLeafStats(this.#leafLink) : null;
    if (this.#pc !== pc) return;
    if (shown) this.#describe(shown.previous, shown.next);
    else this.#describe(previous, next);
  }
  async #readLeafStats(link) {
    const next = readSnapshot(Array.from((await link.getStats()).values()));
    const previous = this.#lastLeafSnapshot;
    this.#lastLeafSnapshot = next;
    return { previous, next };
  }
  async #reportOutgoing(link) {
    const next = readSent(Array.from((await link.getStats()).values()));
    const previous = this.#lastSent;
    this.#lastSent = next;
    if (this.#relayLink !== link || !this.#linked || !previous) return;
    const kbps = outgoingKbps(previous, next);
    if (kbps !== null) this.#send({ type: "relay_outgoing", kbps });
  }
  #describe(previous, next) {
    if (previous) {
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
    const link = this.#onRelayStream() ? this.#leafLink : this.#pc;
    const receiver = link?.getReceivers().find((r) => r.track?.kind === "video");
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
    clearInterval(this.#watchTimer);
    this.#dropLink();
    const pc = this.#pc;
    this.#pc = null;
    this.#channel = null;
    this.#messages = Promise.resolve();
    this.#lastSnapshot = null;
    this.#lastSource = null;
    this.#asked = null;
    this.#received = [];
    if (pc) {
      pc.ontrack = null;
      pc.onconnectionstatechange = null;
      pc.close();
    }
    for (const track of this.stream.getTracks()) {
      track.stop();
      this.stream.removeTrack(track);
    }
    this.#sfuTracks.clear();
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
async function framesDecoded(pc) {
  if (pc.connectionState === "closed") return 0;
  return readSnapshot(Array.from((await pc.getStats()).values())).video?.framesDecoded ?? 0;
}
async function decodedPast(pc, before, timeoutMs) {
  if (!pc.getReceivers().some((r) => r.track?.kind === "video")) return true;
  const giveUpAt = performance.now() + timeoutMs;
  while (performance.now() < giveUpAt) {
    if (pc.connectionState === "closed") return false;
    if (await framesDecoded(pc) > before) return true;
    await new Promise((resolve) => setTimeout(resolve, HANDOVER_POLL_MS));
  }
  return false;
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
    const relay = viewer.debug ? relayText(viewer.role) : null;
    if (relay) text += `
${relay}`;
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
    viewer.on("role", () => renderStats()),
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
function relayText({ role, linked }) {
  if (role === "relay") return linked ? "relaying to a viewer" : "linking to a viewer";
  if (role === "leaf") return linked ? "watching through a relay" : "linking to a relay";
  return null;
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
