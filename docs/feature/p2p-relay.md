# P2P relay

Cut server outbound bandwidth by promoting some viewers to relay traffic to peers. SFU acts as signaling only.

P-1 to P-3 are shipped. P-2's manual trigger was removed when P-3 landed. P-4 is not started.

## Phases

- **P-1. Perf reporting.** Viewer sends `perf_report` over DC; server logs. No promotion.
- **P-2. Manual promotion.** Single relay, single leaf — validate the path.
- **P-3. Automatic promotion.** Server promotes by perf thresholds. Fallback under failure. Policy: [ADR 0009](../decisions/0009-auto-promote-demote-policy.md).
- **P-4. Multi-leaf per relay.**

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

## Notes

- DC cannot carry RTP. Media flows over a separate `RTCPeerConnection` between A and B.
- A relay forwards only what it receives, so its leaf watches the relay's layer.
- One relay carries one leaf.
- No TURN. A pair that cannot connect directly times out and stays on the SFU.
