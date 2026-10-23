import { Emitter } from "./emitter.js";
import {
  applyLayerChanged,
  applyLayerStatus,
  initialLayers,
  requestLayer,
  type LayerState,
} from "./layers.js";
import {
  type ClientMessage,
  type OfferAnswer,
  type OfferRequest,
  type ServerMessage,
} from "./protocol.js";
import {
  freezeNote,
  perfReport,
  readSnapshot,
  timestampDrift,
  viewerStats,
  type Snapshot,
  type ViewerStats,
} from "./stats.js";

export type ViewerState =
  /** Created, `connect()` not called yet. */
  | "idle"
  | "connecting"
  | "connected"
  /** The connection dropped and the SDK is joining again. */
  | "reconnecting"
  /** The room exists but is not live. With `reconnect`, the SDK keeps asking. */
  | "not-live"
  /** Given up for good. `reason` says why. */
  | "failed"
  /** `close()` was called. */
  | "closed";

export interface ViewerOptions {
  /** The room to watch, `RM_...`. */
  room: string;
  /** Base URL of the server's client API. Defaults to the page's own origin. */
  server?: string;
  /** Join again after a dropped connection or a room that is not live yet. Defaults to true. */
  reconnect?: boolean;
  /** Ceiling for the wait between attempts, which doubles from one second. Defaults to 10 s. */
  maxRetryDelayMs?: number;
  /** How often stats are read and perf reports sent. Defaults to one second. */
  statsIntervalMs?: number;
  /** Emit `log` events. Can be switched later with `setDebug`. */
  debug?: boolean;
  /** Passed to `RTCPeerConnection`, for ICE servers and the like. */
  rtcConfiguration?: RTCConfiguration;
}

export type ViewerEvents = {
  state: { state: ViewerState; reason?: string };
  layers: LayerState;
  /** The server switched layer. `waitedMs` is set when this switch answers a request. */
  layerchange: { from: string | null; to: string; waitedMs: number | null };
  stats: ViewerStats;
  /** Debug mode only. */
  log: { at: Date; text: string };
  error: Error;
};

/** The server answered the offer with an error status. */
export class JoinError extends Error {
  override name = "JoinError";
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

// How long a connection may sit in `disconnected` before it is given up. ICE
// often recovers from a short outage on its own.
const DISCONNECTED_GRACE_MS = 5000;
// The server takes candidates from the offer itself, so gathering has to
// finish before it is posted. Host candidates take milliseconds.
const GATHERING_TIMEOUT_MS = 2000;
const TIMESTAMP_PROBE_MS = 250;

export class Viewer extends Emitter<ViewerEvents> {
  /** Carries the room's tracks across reconnects, so a `<video>` is attached once. */
  readonly stream = new MediaStream();

  readonly #options: Required<Omit<ViewerOptions, "server" | "rtcConfiguration">> &
    Pick<ViewerOptions, "server" | "rtcConfiguration">;

  #state: ViewerState = "idle";
  #layers: LayerState = initialLayers();
  #debug: boolean;

  #pc: RTCPeerConnection | null = null;
  #channel: RTCDataChannel | null = null;
  #messages: Promise<void> = Promise.resolve();

  #attempts = 0;
  #joinedOnce = false;
  #retryTimer: ReturnType<typeof setTimeout> | undefined;
  #graceTimer: ReturnType<typeof setTimeout> | undefined;
  #statsTimer: ReturnType<typeof setInterval> | undefined;
  #probeTimer: ReturnType<typeof setInterval> | undefined;

  #lastSnapshot: Snapshot | null = null;
  #lastSource: { timestamp: number; rtpTimestamp: number } | null = null;
  #asked: { rid: string; at: number } | null = null;
  #ignored = new Set<string>();

  #joined: Promise<void> | null = null;
  #settleJoined: { resolve: () => void; reject: (error: Error) => void } | null = null;

  constructor(options: ViewerOptions) {
    super();
    this.#options = {
      room: options.room,
      server: options.server,
      reconnect: options.reconnect ?? true,
      maxRetryDelayMs: options.maxRetryDelayMs ?? 10_000,
      statsIntervalMs: options.statsIntervalMs ?? 1000,
      debug: options.debug ?? false,
      rtcConfiguration: options.rtcConfiguration,
    };
    this.#debug = this.#options.debug;
  }

  get state(): ViewerState {
    return this.#state;
  }

  get layers(): LayerState {
    return this.#layers;
  }

  get debug(): boolean {
    return this.#debug;
  }

  /**
   * Joins the room. Resolves once the server has answered the first offer.
   * With `reconnect`, a room that is not live yet is retried until it is, so
   * the promise waits. Rejects on an error that is not worth retrying, or on
   * `close()`.
   */
  connect(): Promise<void> {
    if (this.#state === "closed") return Promise.reject(new Error("The viewer is closed"));
    if (this.#joined) return this.#joined;

    this.#joined = new Promise((resolve, reject) => {
      this.#settleJoined = { resolve, reject };
    });
    // Callers that only listen for events need not handle the rejection.
    this.#joined.catch(() => {});

    void this.#attempt();
    return this.#joined;
  }

  /** Points a `<video>` at this viewer's stream. */
  attach(video: HTMLVideoElement): HTMLVideoElement {
    video.srcObject = this.stream;
    video.playsInline = true;
    video.autoplay = true;
    return video;
  }

  /** Asks for a simulcast layer. The server switches on the layer's next keyframe. */
  setLayer(rid: string): void {
    this.#request(rid);
  }

  /** Lets the server pick the layer from this viewer's bandwidth estimate. */
  setAutoLayer(): void {
    this.#request(null);
  }

  setDebug(on: boolean): void {
    if (this.#debug === on) return;
    this.#debug = on;
    this.#lastSource = null;
    this.#syncProbe();
    if (on) this.#log("debug on");
  }

  close(): void {
    if (this.#state === "closed") return;
    clearTimeout(this.#retryTimer);
    this.#teardown();
    this.#setState("closed");
    this.#settleJoined?.reject(new Error("The viewer was closed"));
    this.#settleJoined = null;
  }

  async #attempt(): Promise<void> {
    // A room that is not live stays that way in the state while it is retried,
    // rather than flickering through "connecting" on every attempt.
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

  #retryable(error: Error): boolean {
    if (error instanceof JoinError) return error.status >= 500;
    // A network failure in fetch, or the connection failing to come up.
    return true;
  }

  #retry(): void {
    const delay = Math.min(1000 * 2 ** this.#attempts, this.#options.maxRetryDelayMs);
    this.#attempts += 1;
    this.#log(`trying again in ${Math.round(delay / 1000)} s`);
    this.#retryTimer = setTimeout(() => void this.#attempt(), delay);
  }

  #fail(error: Error): void {
    this.#setState("failed", error.message);
    this.emit("error", error);
    this.#settleJoined?.reject(error);
    this.#settleJoined = null;
  }

  async #join(): Promise<void> {
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

    // The server adds the media tracks over this channel once it opens, so
    // the channel has to be part of the first offer.
    const channel = pc.createDataChannel("data");
    this.#channel = channel;
    channel.onmessage = (event) => {
      const message = JSON.parse(String(event.data)) as ServerMessage;
      this.#messages = this.#messages
        .then(() => this.#handle(pc, message))
        .catch((error) => this.emit("error", error instanceof Error ? error : new Error(String(error))));
    };

    await pc.setLocalDescription(await pc.createOffer());
    await gathered(pc);

    const body: OfferRequest = {
      type: "offer",
      sdp: pc.localDescription?.sdp ?? "",
      room_id: this.#options.room,
    };

    const response = await fetch(url(this.#options.server, "offer"), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });

    if (response.status === 404) throw new JoinError(404, "This room is not live");
    if (!response.ok) throw new JoinError(response.status, `The server refused the offer: ${response.status}`);

    const answer = (await response.json()) as OfferAnswer;

    if (this.#pc !== pc) return;
    await pc.setRemoteDescription({ type: "answer", sdp: answer.sdp });
    this.#log("offer answered");
  }

  #onConnectionState(pc: RTCPeerConnection): void {
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

  #lost(pc: RTCPeerConnection, reason: string): void {
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

  async #handle(pc: RTCPeerConnection, message: ServerMessage): Promise<void> {
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
        // The relay protocol. Left unanswered, the server never promotes this viewer.
        if (!this.#ignored.has(message.type)) {
          this.#ignored.add(message.type);
          this.#log(`${message.type} ignored, relay is not supported yet`);
        }
      }
    }
  }

  #request(rid: string | null): void {
    const { state, messages } = requestLayer(this.#layers, rid);
    if (messages.length === 0) return;

    this.#asked = rid !== null && rid !== this.#layers.current ? { rid, at: performance.now() } : null;
    this.#log(rid === null ? "asked for auto" : `asked for ${rid}`);
    this.#layers = state;
    for (const message of messages) this.#send(message);
    this.emit("layers", this.#layers);
  }

  #send(message: ClientMessage): void {
    if (this.#channel?.readyState === "open") this.#channel.send(JSON.stringify(message));
  }

  #startStats(): void {
    clearInterval(this.#statsTimer);
    this.#lastSnapshot = null;
    this.#statsTimer = setInterval(() => void this.#readStats(), this.#options.statsIntervalMs);
    this.#syncProbe();
  }

  async #readStats(): Promise<void> {
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

  #syncProbe(): void {
    clearInterval(this.#probeTimer);
    if (this.#debug && this.#pc) {
      this.#probeTimer = setInterval(() => this.#probeTimestamp(), TIMESTAMP_PROBE_MS);
    }
  }

  // Read from the receiver, not from drawn frames: a browser draws a window
  // nobody can see about once a second.
  #probeTimestamp(): void {
    const receiver = this.#pc?.getReceivers().find((r) => r.track?.kind === "video");
    const source = receiver?.getSynchronizationSources().sort((a, b) => b.timestamp - a.timestamp)[0];
    if (!source || source.rtpTimestamp === undefined) return;

    const now = { timestamp: source.timestamp, rtpTimestamp: source.rtpTimestamp };
    if (!this.#lastSource) {
      this.#log("reading RTP timestamps");
    } else if (now.timestamp !== this.#lastSource.timestamp) {
      const drift = timestampDrift(this.#lastSource, now);
      if (Math.abs(drift) > 1000) {
        this.#log(`RTP timestamp jumped ${Math.round(Math.abs(drift) / 1000)} s ${drift > 0 ? "forward" : "back"}`);
      }
    }
    this.#lastSource = now;
  }

  #teardown(): void {
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

  #setState(state: ViewerState, reason?: string): void {
    if (this.#state === state) return;
    this.#state = state;
    this.emit("state", reason === undefined ? { state } : { state, reason });
  }

  #log(text: string): void {
    if (this.#debug) this.emit("log", { at: new Date(), text });
  }
}

function url(server: string | undefined, path: string): string {
  const base = server ?? globalThis.location.origin;
  return `${base.replace(/\/+$/, "")}/${path}`;
}

function gathered(pc: RTCPeerConnection): Promise<void> {
  if (pc.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((resolve) => {
    const done = () => {
      if (pc.iceGatheringState === "complete") resolve();
    };
    pc.addEventListener("icegatheringstatechange", done);
    setTimeout(resolve, GATHERING_TIMEOUT_MS);
  });
}
