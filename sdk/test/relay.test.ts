import assert from "node:assert/strict";
import { test } from "node:test";

import { outgoingKbps, readSent, relayVideoCeiling, withBitrateHints } from "../src/relay.ts";

test("sent bytes are summed over outbound streams only", () => {
  const sent = readSent([
    { type: "outbound-rtp", kind: "video", timestamp: 1000, bytesSent: 100_000 },
    { type: "outbound-rtp", kind: "audio", timestamp: 1000, bytesSent: 5_000 },
    { type: "inbound-rtp", kind: "video", timestamp: 1000, bytesReceived: 900_000 },
  ]);
  assert.deepEqual(sent, { at: 1000, bytesSent: 105_000 });
});

test("the outgoing rate is taken over the interval", () => {
  assert.equal(outgoingKbps({ at: 1000, bytesSent: 0 }, { at: 3000, bytesSent: 250_000 }), 1000);
  assert.equal(outgoingKbps({ at: 1000, bytesSent: 0 }, { at: 1000, bytesSent: 250_000 }), null);
});

test("a counter that went back reads as nothing sent", () => {
  assert.equal(outgoingKbps({ at: 1000, bytesSent: 500 }, { at: 2000, bytesSent: 0 }), 0);
});

test("the relay's ceiling sits a little above what it receives", () => {
  assert.equal(relayVideoCeiling(1000), 1_200_000);
});

const answer = [
  "v=0",
  "m=audio 9 UDP/TLS/RTP/SAVPF 111",
  "a=rtpmap:111 opus/48000/2",
  "a=fmtp:111 minptime=10;useinbandfec=1",
  "m=video 9 UDP/TLS/RTP/SAVPF 96 97 98",
  "a=rtpmap:96 VP8/90000",
  "a=rtpmap:97 rtx/90000",
  "a=fmtp:97 apt=96",
  "a=rtpmap:98 H264/90000",
  "a=fmtp:98 profile-level-id=42e01f",
  "",
].join("\r\n");

test("a start and a floor are set on each video codec and nothing else", () => {
  const params = "x-google-start-bitrate=1500;x-google-min-bitrate=1200";
  const lines = withBitrateHints(answer, 1500).split("\r\n");
  assert.ok(lines.includes(`a=fmtp:96 ${params}`));
  assert.ok(lines.includes(`a=fmtp:98 profile-level-id=42e01f;${params}`));
  assert.ok(lines.includes("a=fmtp:97 apt=96"));
  assert.ok(lines.includes("a=fmtp:111 minptime=10;useinbandfec=1"));
  assert.equal(lines.indexOf(`a=fmtp:96 ${params}`), lines.indexOf("a=rtpmap:96 VP8/90000") + 1);
});
