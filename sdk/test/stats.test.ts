import assert from "node:assert/strict";
import { test } from "node:test";

import { freezeNote, perfReport, readSnapshot, rtpStep, timestampDrift, viewerStats } from "../src/stats.ts";

const report = (
  at: number,
  video: Record<string, number>,
  audio: Record<string, number> = {},
  rtt: number | null = 0.012,
) => [
  { id: "T", type: "transport", timestamp: at, selectedCandidatePairId: "CP" },
  { id: "CP", type: "candidate-pair", timestamp: at, ...(rtt === null ? {} : { currentRoundTripTime: rtt }) },
  { id: "V", type: "inbound-rtp", kind: "video", timestamp: at, ...video },
  { id: "A", type: "inbound-rtp", kind: "audio", timestamp: at, packetsReceived: 0, packetsLost: 0, ...audio },
];

test("a snapshot reads the selected pair's round trip and sums inbound packets", () => {
  const snapshot = readSnapshot(
    report(1000, { packetsReceived: 90, packetsLost: 2, frameWidth: 1280, frameHeight: 720 }, { packetsReceived: 50 }),
  );
  assert.equal(snapshot.rttMs, 12);
  assert.equal(snapshot.packetsReceived, 140);
  assert.equal(snapshot.packetsLost, 2);
  assert.equal(snapshot.video?.width, 1280);
});

test("a perf report counts loss over the interval only", () => {
  const before = readSnapshot(report(1000, { packetsReceived: 100, packetsLost: 10 }));
  const after = readSnapshot(report(2000, { packetsReceived: 196, packetsLost: 14 }));
  assert.deepEqual(perfReport(before, after), { rtt_ms: 12, loss_pct: 4 });
});

test("no perf report before the pair has a round trip", () => {
  const before = readSnapshot(report(1000, {}));
  const after = readSnapshot(report(2000, {}, {}, null));
  assert.equal(perfReport(before, after), null);
});

test("bitrate is taken over the interval", () => {
  const before = readSnapshot(report(1000, { bytesReceived: 0 }));
  const after = readSnapshot(report(2000, { bytesReceived: 250_000 }));
  assert.equal(viewerStats(before, after).mbps, 2);
  assert.equal(viewerStats(null, after).mbps, null);
});

test("a freeze is described with the loss and drops around it", () => {
  const before = readSnapshot(report(1000, { freezeCount: 1, totalFreezesDuration: 0.2, packetsLost: 3 }));
  const after = readSnapshot(
    report(2000, { freezeCount: 2, totalFreezesDuration: 0.65, packetsLost: 8, framesDropped: 1 }),
  );
  assert.equal(freezeNote(before, after), "froze 450 ms, 5 packets lost, 1 frames dropped");
  assert.equal(freezeNote(after, after), null);
});

test("the RTP step is signed across the 32 bit wrap", () => {
  assert.equal(rtpStep(2 ** 32 - 10, 20), 30);
  assert.equal(rtpStep(20, 2 ** 32 - 10), -30);
});

test("drift is how far the media clock outran the wall clock", () => {
  assert.equal(timestampDrift({ timestamp: 0, rtpTimestamp: 0 }, { timestamp: 1000, rtpTimestamp: 90_000 }), 0);
  assert.equal(timestampDrift({ timestamp: 0, rtpTimestamp: 0 }, { timestamp: 1000, rtpTimestamp: 900_000 }), 9000);
});
