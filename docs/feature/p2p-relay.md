# P2P relay

Cut server outbound bandwidth by promoting some viewers to relay traffic to peers. SFU acts as signaling only.

P-1 to P-3 are shipped. P-2's manual trigger was removed when P-3 landed. P-4 and P-5 are not started.

## Phases

- **P-1. Perf reporting.** Viewer sends `perf_report` over DC; server logs. No promotion.
- **P-2. Manual promotion.** Single relay, single leaf, to validate the path.
- **P-3. Automatic promotion.** Server promotes by perf thresholds. Fallback under failure. Policy: [ADR 0009](../decisions/0009-auto-promote-demote-policy.md).
- **P-4. Multi-leaf per relay.**
- **P-5. Measured upload.** The server measures a candidate's upload rather than trusting the figure it reports, and checks a relay's claimed rate against what its leaf receives.

## Flow

1. Viewers connect and receive from SFU directly. Each sends perf reports over DC.
2. Server sends an upload probe to a viewer whose perf window is healthy. The viewer answers with the upload it measured.
3. A viewer whose probed upload clears the cutoff, and that has been connected long enough, is promoted. Server picks its leaf from the viewers in the room with no role, preferring one that could not relay itself.
4. Server asks the relay for an offer. Relay and leaf negotiate P2P via the server (signaling).
5. Leaf reports the peer link up. Server stops forwarding to it.
6. A link that is not up within 5 seconds is given up. The leaf was on the SFU's stream the whole time.
7. Relay reports its outgoing bitrate. If it falls below what its layers need, or its perf window turns unhealthy, both are demoted and server resumes direct forwarding to the leaf.
8. A leaf that loses the peer link, or whose relay disconnects, gets the same fallback.

Thresholds live in `config.toml`, under `[perf]` and `[relay]`.

## DataChannel messages

| Type | From → To | Purpose |
|---|---|---|
| `perf_report` | viewer → server | Periodic RTT and loss |
| `probe_available_upload` | server → viewer | Measure your upload |
| `available_upload` | viewer → server | The probe's result, in kbps |
| `request_offer` | server → relay | Become a relay: send a `p2p_offer` |
| `p2p_offer` / `p2p_answer` | relayed via server | Relay ↔ leaf signaling |
| `p2p_connected` / `p2p_disconnected` | leaf → server | The peer link came up, or dropped |
| `relay_outgoing` | relay → server | Bitrate the relay is forwarding, in kbps |
| `demote` | server → relay and leaf | The pair is dissolved. Direct forwarding resumes |

All messages use the `{ "type": ..., ... }` envelope (shared with simulcast).

## P-5. Measured upload

Today the server only asks for a probe. `available_upload` is the figure the client measured, and `relay_outgoing` after promotion is the relay's own report. The server cannot check either.

**Probe.** The server asks the candidate to send R kbps for T seconds over the connection it already has, and counts what arrives. A client cannot produce that figure without sending it.

- R is the bitrate of the layers the leaf would take, plus headroom. The probe asks whether the candidate can sustain that, not how much it could send. Filling the uplink would add delay to the candidate's own playback, which is why [ADR 0009](../decisions/0009-auto-promote-demote-policy.md) ruled out probing continuously.
- It passes when the bytes that arrived over the window reach a set share of R.

| Sent over | Change | Like a relay | Server cost |
|---|---|---|---|
| DataChannel bytes | Small, no renegotiation | Less, a relay sends RTP | SCTP |
| An added send track | Renegotiation, a track received and dropped | More, same RTP and congestion control | SRTP decryption |

Before choosing the DataChannel, check that str0m's SCTP takes a few Mbps. If it cannot, the probe measures the server rather than the client.

**Leaf cross-check.** The probe still measures the path to the server, while direct links mostly fail between two homes, on NAT or the far side's line. After promotion the leaf reports what it received from its relay, and the server compares that with `relay_outgoing`. That is the only check on the real path.

**Changes.** `probe_available_upload` carries R and T, and `available_upload` is replaced by the server's count. The probe states, timeout and retry interval stay. A new ADR supersedes the probe in ADR 0009.

**Rejected: the server as a stand-in leaf.** The server would open a peer link to the candidate as a leaf and attach the real one only after it passed. That makes the server a WebRTC client inside a media loop, decrypts the whole stream for the trial, and still measures only the path to the server.

**When.** Browsers can relay through the viewer SDK, which leaves the probe unanswered until this lands. Today only the str0m test clients answer it.

## Notes

- DC cannot carry RTP. Media flows over a separate `RTCPeerConnection` between A and B.
- A relay forwards only what it receives, so its leaf watches the relay's layer.
- One relay carries one leaf.
- No TURN. A pair that cannot connect directly times out and stays on the SFU.
- A browser re-encodes a track it forwards to another peer link, which costs the relay CPU and quality and changes the bitrate it sends. The viewer SDK holds the encoder near the bitrate the relay receives, with a start and a floor, because a browser starts low and the rate is judged after `min_p2p_connection_age_secs`. Passing encoded frames through needs the Encoded Transform API and a keyframe from the publisher at link up, which the server has no message for. The test clients forward RTP as is.
- A relay that changes layer while paired changes what its encoder is held to, and its rate lags behind. Seen with a headless Chrome publisher, whose layers come and go: the server demoted pairs while the relay moved between `h` and `m`.
