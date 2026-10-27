/**
 * A viewer's part in a P2P relay pair. A relay forwards what it receives from
 * the server to one other viewer, its leaf, over a second peer link.
 */
export type RelayRole = "relay" | "leaf";

/** The bytes a peer link has sent, from one `getStats()` call. */
export interface SendSnapshot {
  /** Milliseconds, from the stats report's own clock. */
  at: number;
  /** Summed over every outbound stream, audio included. */
  bytesSent: number;
}

type Entry = Record<string, unknown> & { type: string };

const num = (value: unknown): number => (typeof value === "number" ? value : 0);

export function readSent(entries: Iterable<Entry>): SendSnapshot {
  let at = 0;
  let bytesSent = 0;

  for (const entry of entries) {
    at = Math.max(at, num(entry["timestamp"]));
    if (entry.type === "outbound-rtp") bytesSent += num(entry["bytesSent"]);
  }

  return { at, bytesSent };
}

/** What the relay reports as `relay_outgoing`, over the interval between two snapshots. */
export function outgoingKbps(previous: SendSnapshot, next: SendSnapshot): number | null {
  const seconds = (next.at - previous.at) / 1000;
  if (seconds <= 0) return null;
  return Math.max(0, Math.round(((next.bytesSent - previous.bytesSent) * 8) / seconds / 1000));
}

// The server demotes a relay that sends well under the bitrate of the layer
// it receives, and gains nothing from one that sends more. A browser encodes
// a forwarded track again and picks its own rate, so the relay steers it
// toward what it receives: a ceiling a little above, and a start and a floor
// near it, since the browser otherwise starts low and the server judges the
// rate within seconds.
const CEILING_SHARE = 1.2;
const FLOOR_SHARE = 0.8;

/** The ceiling for the relay's video encoder, in bits per second. */
export function relayVideoCeiling(receivedKbps: number): number {
  return Math.round(receivedKbps * CEILING_SHARE * 1000);
}

const VIDEO_CODECS = new Set(["vp8", "vp9", "h264", "av1", "h265"]);

/**
 * Adds a start and a minimum bitrate to the video codecs of an SDP. Chrome
 * reads them from the remote description, so the relay applies them to the
 * leaf's answer. Other browsers ignore them.
 */
export function withBitrateHints(sdp: string, kbps: number): string {
  const params = `x-google-start-bitrate=${kbps};x-google-min-bitrate=${Math.round(kbps * FLOOR_SHARE)}`;
  const eol = sdp.includes("\r\n") ? "\r\n" : "\n";
  const lines = sdp.split(eol);
  const out: string[] = [];
  let video = false;
  const withFmtp = new Set<string>();

  // Payload types that already have an fmtp line get the parameter appended.
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
    if (rtpmap && VIDEO_CODECS.has(rtpmap[2]!.toLowerCase()) && !withFmtp.has(rtpmap[1]!)) {
      out.push(`a=fmtp:${rtpmap[1]} ${params}`);
    }

    const fmtp = /^a=fmtp:(\d+) /.exec(line);
    if (fmtp && isVideoCodec(lines, fmtp[1]!)) {
      out[out.length - 1] = `${line};${params}`;
    }
  }

  return out.join(eol);
}

function isVideoCodec(lines: string[], pt: string): boolean {
  const rtpmap = lines.find((l) => l.startsWith(`a=rtpmap:${pt} `));
  const name = rtpmap ? /^a=rtpmap:\d+ ([^/]+)\//.exec(rtpmap)?.[1] : undefined;
  return !!name && VIDEO_CODECS.has(name.toLowerCase());
}
