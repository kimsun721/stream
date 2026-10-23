/** The figures the SDK reads from one `getStats()` call. */
export interface Snapshot {
  /** Milliseconds, from the stats report's own clock. */
  at: number;
  /** Round trip time of the selected candidate pair, in milliseconds. */
  rttMs: number | null;
  /** Summed over every inbound stream, audio included. */
  packetsReceived: number;
  packetsLost: number;
  video: {
    width: number;
    height: number;
    bytesReceived: number;
    framesPerSecond: number | null;
    framesDropped: number;
    freezeCount: number;
    freezeSeconds: number;
  } | null;
}

/** What a viewer sees, published once a second. */
export interface ViewerStats {
  width: number;
  height: number;
  framesPerSecond: number | null;
  /** Video bitrate received since the previous figure, in megabits per second. */
  mbps: number | null;
  rttMs: number | null;
  freezeCount: number;
  freezeSeconds: number;
  packetsLost: number;
  framesDropped: number;
}

type Entry = Record<string, unknown> & { type: string; id: string };

const num = (value: unknown): number => (typeof value === "number" ? value : 0);
const opt = (value: unknown): number | null => (typeof value === "number" ? value : null);

/** Takes the entries of an `RTCStatsReport`, as `Array.from(report.values())`. */
export function readSnapshot(entries: Iterable<Entry>): Snapshot {
  const all = Array.from(entries);
  const byId = new Map(all.map((entry) => [entry.id, entry]));

  let at = 0;
  let packetsReceived = 0;
  let packetsLost = 0;
  let video: Snapshot["video"] = null;

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
        freezeSeconds: num(entry["totalFreezesDuration"]),
      };
    }
  }

  return { at, rttMs: selectedRtt(all, byId), packetsReceived, packetsLost, video };
}

function selectedRtt(all: Entry[], byId: Map<string, Entry>): number | null {
  const transport = all.find((entry) => entry.type === "transport" && entry["selectedCandidatePairId"]);
  const pair =
    (transport && byId.get(String(transport["selectedCandidatePairId"]))) ??
    all.find((entry) => entry.type === "candidate-pair" && entry["nominated"] && entry["state"] === "succeeded");

  const seconds = pair && opt(pair["currentRoundTripTime"]);
  return seconds === null || seconds === undefined ? null : seconds * 1000;
}

/**
 * The link quality the server judges relay candidates by. Loss is over the
 * packets that arrived or went missing since the previous snapshot.
 */
export function perfReport(
  previous: Snapshot,
  next: Snapshot,
): { rtt_ms: number; loss_pct: number } | null {
  if (next.rttMs === null) return null;

  const received = next.packetsReceived - previous.packetsReceived;
  const lost = next.packetsLost - previous.packetsLost;
  const expected = received + lost;
  const lossPct = expected > 0 && lost > 0 ? (lost / expected) * 100 : 0;

  return { rtt_ms: Math.round(next.rttMs), loss_pct: Math.round(lossPct * 100) / 100 };
}

export function viewerStats(previous: Snapshot | null, next: Snapshot): ViewerStats {
  const video = next.video;
  let mbps: number | null = null;

  if (previous?.video && video) {
    const seconds = (next.at - previous.at) / 1000;
    if (seconds > 0) mbps = ((video.bytesReceived - previous.video.bytesReceived) * 8) / seconds / 1e6;
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
    framesDropped: video?.framesDropped ?? 0,
  };
}

/**
 * Describes the freezes since the previous snapshot. Loss points at the
 * network or the sender, drops at this browser, and neither at frames that
 * simply arrived late.
 */
export function freezeNote(previous: Snapshot, next: Snapshot): string | null {
  const before = previous.video;
  const after = next.video;
  if (!before || !after || after.freezeCount <= before.freezeCount) return null;

  const times = after.freezeCount - before.freezeCount;
  const ms = Math.round((after.freezeSeconds - before.freezeSeconds) * 1000);
  const froze = times === 1 ? `froze ${ms} ms` : `froze ${times} times, ${ms} ms in all`;

  return `${froze}, ${next.packetsLost - previous.packetsLost} packets lost, ${after.framesDropped - before.framesDropped} frames dropped`;
}

/** The signed difference between two 32 bit RTP timestamps, across a wrap. */
export function rtpStep(from: number, to: number): number {
  return ((((to - from + 2 ** 31) % 2 ** 32) + 2 ** 32) % 2 ** 32) - 2 ** 31;
}

/**
 * How far the RTP timestamp moved past the wall clock between two readings, in
 * milliseconds. Layers are separate RTP streams, so a switch can move the
 * timestamp by far more than the time that passed.
 */
export function timestampDrift(
  previous: { timestamp: number; rtpTimestamp: number },
  next: { timestamp: number; rtpTimestamp: number },
  clockRate = 90_000,
): number {
  return (rtpStep(previous.rtpTimestamp, next.rtpTimestamp) / clockRate) * 1000 - (next.timestamp - previous.timestamp);
}
