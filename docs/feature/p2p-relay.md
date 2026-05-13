# P2P relay

Cut server outbound bandwidth by promoting some viewers to relay traffic to peers. SFU acts as signaling only.

## Phases

- **P-1. Perf reporting.** Viewer sends `perf_report` over DC; server logs. No promotion.
- **P-2. Manual promotion.** Single relay, single leaf — validate the path.
- **P-3. Automatic promotion.** Server promotes by perf thresholds. Fallback under failure.
- **P-4. Multi-leaf per relay.**

## Flow

1. Viewer A connects, receives from SFU directly.
2. A sends perf reports over DC.
3. Server marks A as a relay candidate after stable metrics.
4. New viewer B connects; server picks a candidate in the same room.
5. Server tells A "relay to B" and B "receive from A".
6. A and B negotiate P2P via the server (signaling).
7. On success, server stops forwarding to B.
8. If A degrades, server resumes direct forwarding to B.

## DataChannel messages

| Type | From → To | Purpose |
|---|---|---|
| `perf_report` | viewer → server | Periodic up/down/RTT/loss |
| `promote` | server → viewer | Become a relay |
| `p2p_offer` / `p2p_answer` | relayed via server | Relay ↔ leaf signaling |
| `fallback` | server → viewer | Direct forwarding resumes |

All messages use the `{ "type": ..., ... }` envelope (shared with simulcast).

## Notes

- DC cannot carry RTP. Media flows over a separate `RTCPeerConnection` between A and B.
- A relay forwards only what it receives. v1: relay locks to a fixed simulcast layer; leaves needing a different layer fall back to SFU direct.
- STUN/TURN required. Self-hosted TURN shrinks savings.
