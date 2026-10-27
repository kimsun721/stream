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
  outgoingKbps,
  readSent,
  relayVideoCeiling,
  withBitrateHints,
  type RelayRole,
  type SendSnapshot,
} from "./relay.js";
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
  /** Passed to `RTCPeerConnection`, for ICE servers and the like. Also used for relay links. */
  rtcConfiguration?: RTCConfiguration;
  /**
   * Forward the stream to another viewer when the server makes this one a
   * relay. Costs this viewer upload and the CPU to encode the stream again.
   * Defaults to true. Receiving from a relay needs no option.
   */
  relay?: boolean;
}

export type ViewerEvents = {
  state: { state: ViewerState; reason?: string };
  layers: LayerState;
  /** The server switched layer. `waitedMs` is set when this switch answers a request. */
  layerchange: { from: string | null; to: string; waitedMs: number | null };
  stats: ViewerStats;
  /**
   * This viewer's part in a P2P relay pair, or null for none. `linked` is
   * whether the peer link is up: a linked leaf plays its relay's stream.
   */
  role: { role: RelayRole | null; linked: boolean };
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
const RECEIVED_WINDOW = 5;
// A picture is swapped only once the source taking over has decoded a frame,
// so the viewer sees the old one meanwhile rather than a freeze. The server
// asks the publisher for a keyframe when it takes a leaf back, and a publisher
// on a fixed schedule, as OBS is, may take seconds to send one.
const HANDOVER_TIMEOUT_MS = 10_000;
const HANDOVER_POLL_MS = 100;
// A relay that closes its page takes seconds to show in the connection state.
// Its media stops at once, so a leaf goes by that.
const RELAY_SILENCE_MS = 1000;
const RELAY_WATCH_MS = 250;

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
  #sfuTracks = new Map<string, MediaStreamTrack>();

  // At most one of the two links is open: a relay's to its leaf, or a leaf's
  // from its relay.
  #role: RelayRole | null = null;
  #linked = false;
  #relayLink: RTCPeerConnection | null = null;
  #leafLink: RTCPeerConnection | null = null;
  #relayTracks = new Map<string, MediaStreamTrack>();
  #lastSent: SendSnapshot | null = null;
  #linking = false;
  #watchTimer: ReturnType<typeof setInterval> | undefined;
  // Video received from the server over the last few seconds, which a relay
  // holds its encoder to. The highest of them, so a freeze just before a
  // promotion does not start the relay near zero.
  #received: number[] = [];
  #lastLeafSnapshot: Snapshot | null = null;
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
      relay: options.relay ?? true,
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

  get role(): { role: RelayRole | null; linked: boolean } {
    return { role: this.#role, linked: this.#linked };
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
      const track = event.track;
      this.#sfuTracks.set(track.kind, track);
      if (!this.#onRelayStream()) this.#show(track);
      const sender = this.#relayLink?.getSenders().find((s) => s.track?.kind === track.kind);
      if (sender) void sender.replaceTrack(track).catch((error) => this.emit("error", error));
      this.#log(`receiving ${track.kind}`);
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

      case "probe_available_upload": {
        // A browser cannot measure its own upload honestly. The server is to
        // measure it instead, and until then this viewer is never promoted.
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
  async #offerToLeaf(): Promise<void> {
    if (!this.#options.relay) {
      this.#log("asked to relay, declined by the relay option");
      return;
    }
    this.#dropLink();

    const link = new RTCPeerConnection(this.#options.rtcConfiguration);
    this.#relayLink = link;
    this.#setRole("relay", false);

    // The browser encodes the tracks again on the way out, so its own key
    // frame starts the leaf's picture and no request reaches the publisher.
    const kbps = this.#receivedKbps();
    for (const track of this.#sfuTracks.values()) {
      const ceiling = track.kind === "video" && kbps ? relayVideoCeiling(kbps) : null;
      link.addTransceiver(track, {
        direction: "sendonly",
        streams: [this.stream],
        ...(ceiling === null ? {} : { sendEncodings: [{ maxBitrate: ceiling }] }),
      });
    }

    link.onconnectionstatechange = () => {
      if (this.#relayLink !== link) return;
      const state = link.connectionState;
      this.#log(`link to leaf ${state}`);
      if (state === "connected") this.#setRole("relay", true);
      // The leaf reports a lost link and the server demotes both, so the
      // relay only waits for that.
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
  async #holdRelayCeiling(link: RTCPeerConnection): Promise<void> {
    const sender = link.getSenders().find((s) => s.track?.kind === "video");
    const kbps = this.#receivedKbps();
    if (!sender || !kbps) return;

    const ceiling = relayVideoCeiling(kbps);
    const parameters = sender.getParameters();
    const encoding = parameters.encodings[0];
    const current = encoding?.maxBitrate;
    if (!encoding || (current && Math.abs(current - ceiling) < current * 0.1)) return;

    encoding.maxBitrate = ceiling;
    await sender.setParameters(parameters);
  }

  // The leaf side. The server has paired this viewer with a relay, which
  // offers a link. The server keeps sending until the link is up.
  async #answerRelay(sdp: string): Promise<void> {
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

  async #onLeafLink(link: RTCPeerConnection): Promise<void> {
    if (this.#leafLink !== link) return;
    const state = link.connectionState;
    this.#log(`link from relay ${state}`);

    if (state === "connected" && !this.#linked && !this.#linking) {
      // The server stops sending on p2p_connected, so it goes only once the
      // relay's picture is there to take over. Left unsent, the server gives
      // the link up on its own.
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
      // The link never came up. The server gives up on it by itself.
      this.#dropLink("could not reach the relay");
    }
  }

  #watchRelay(link: RTCPeerConnection): void {
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

  #relayLost(): void {
    // Not worth the grace the server connection gets: the server's stream is
    // one message away.
    this.#send({ type: "p2p_disconnected" });
    this.#dropLink("lost the relay, back on the server's stream");
  }

  /** Closes whichever relay link is open and plays the server's stream again. */
  #dropLink(reason?: string): void {
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

    // Audio needs no keyframe and comes back at once.
    const audio = this.#sfuTracks.get("audio");
    if (audio) this.#show(audio);

    const pc = this.#pc;
    if (!wasOnRelay || !pc) {
      for (const track of this.#sfuTracks.values()) this.#show(track);
      close();
      return;
    }

    // The relay's picture stays up, or its last frame if the relay is gone,
    // until the server's stream has a frame of its own.
    void framesDecoded(pc)
      .then((before) => decodedPast(pc, before, HANDOVER_TIMEOUT_MS))
      .then((ready) => {
        const video = this.#sfuTracks.get("video");
        if (this.#pc === pc && !this.#onRelayStream() && video) this.#show(video);
        if (!ready) this.#log("the server's stream sent no picture in time");
        close();
      });
  }

  #receivedKbps(): number | null {
    return this.#received.length ? Math.max(...this.#received) : null;
  }

  #onRelayStream(): boolean {
    return this.#role === "leaf" && this.#linked;
  }

  /** Puts `track` in the attached stream in place of any track of its kind. */
  #show(track: MediaStreamTrack): void {
    for (const old of this.stream.getTracks()) {
      if (old.kind === track.kind && old !== track) this.stream.removeTrack(old);
    }
    if (!this.stream.getTracks().includes(track)) this.stream.addTrack(track);
  }

  #setRole(role: RelayRole | null, linked: boolean): void {
    if (this.#role === role && this.#linked === linked) return;
    this.#role = role;
    this.#linked = linked;
    // The relay's stream has RTP timestamps of its own.
    this.#lastSource = null;
    this.emit("role", { role, linked });
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

    // The server judges its own link to this viewer, so perf reports come
    // from it even while the picture comes from a relay.
    if (previous) {
      const perf = perfReport(previous, next);
      if (perf) this.#send({ type: "perf_report", ...perf });

      const mbps = viewerStats(previous, next).mbps;
      if (mbps !== null && !this.#onRelayStream()) {
        this.#received = [...this.#received.slice(-(RECEIVED_WINDOW - 1)), Math.round(mbps * 1000)];
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

  async #readLeafStats(link: RTCPeerConnection): Promise<{ previous: Snapshot | null; next: Snapshot }> {
    const next = readSnapshot(Array.from((await link.getStats()).values()));
    const previous = this.#lastLeafSnapshot;
    this.#lastLeafSnapshot = next;
    return { previous, next };
  }

  async #reportOutgoing(link: RTCPeerConnection): Promise<void> {
    const next = readSent(Array.from((await link.getStats()).values()));
    const previous = this.#lastSent;
    this.#lastSent = next;
    if (this.#relayLink !== link || !this.#linked || !previous) return;

    const kbps = outgoingKbps(previous, next);
    if (kbps !== null) this.#send({ type: "relay_outgoing", kbps });
  }

  #describe(previous: Snapshot | null, next: Snapshot): void {
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

  #syncProbe(): void {
    clearInterval(this.#probeTimer);
    if (this.#debug && this.#pc) {
      this.#probeTimer = setInterval(() => this.#probeTimestamp(), TIMESTAMP_PROBE_MS);
    }
  }

  // Read from the receiver, not from drawn frames: a browser draws a window
  // nobody can see about once a second.
  #probeTimestamp(): void {
    const link = this.#onRelayStream() ? this.#leafLink : this.#pc;
    const receiver = link?.getReceivers().find((r) => r.track?.kind === "video");
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
    clearInterval(this.#watchTimer);
    // The server drops this viewer's pairing along with the connection.
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

async function framesDecoded(pc: RTCPeerConnection): Promise<number> {
  if (pc.connectionState === "closed") return 0;
  return readSnapshot(Array.from((await pc.getStats()).values())).video?.framesDecoded ?? 0;
}

/** Whether `pc` decodes a video frame past `before` within `timeoutMs`. */
async function decodedPast(pc: RTCPeerConnection, before: number, timeoutMs: number): Promise<boolean> {
  if (!pc.getReceivers().some((r) => r.track?.kind === "video")) return true;
  const giveUpAt = performance.now() + timeoutMs;
  while (performance.now() < giveUpAt) {
    if (pc.connectionState === "closed") return false;
    if ((await framesDecoded(pc)) > before) return true;
    await new Promise((resolve) => setTimeout(resolve, HANDOVER_POLL_MS));
  }
  return false;
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
