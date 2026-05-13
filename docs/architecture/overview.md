# System Overview

A WebRTC SFU for low-latency live streaming. Single-process; persistence and horizontal scaling are out of scope.

## Components

| | Role |
|---|---|
| ICE / DTLS / SRTP | Connectivity and transport security (via `str0m`) |
| SFU loop | Owns `Rooms`; consumes UDP; forwards RTP |
| HTTP API | Room CRUD for an external backend |
| HTTPS API | Initial SDP offer from browsers |
| DataChannel | Per-client side band: renegotiation, simulcast layer requests, P2P relay signaling |

The external backend is treated as a black box that speaks HTTP — this server makes no assumptions about its stack.

## Threading

- HTTP/HTTPS servers: tokio threads.
- SFU loop: single std thread, sole owner of `Rooms`.
- HTTP ↔ SFU communication goes through `mpsc::sync_channel<SfuMessage>`. See [ADR 0001](../decisions/0001-actor-pattern.md).

## Data flow

```
Browser ─────SDP─────▶ HTTPS ──┐
Browser ──RTP/RTCP──▶ UDP ─────┤
                               ▼
                          SFU loop ──UDP──▶ Browsers (viewers)
                               ▲
External backend ────HTTP──────┘
```

Media is forwarded per-room: a streamer's RTP fans out to all viewers in the same room. With P2P relay (planned), some viewers receive from a peer instead.

## State

```
Rooms
└── Room { streamer_id, state, clients }
    └── Client { rtc, role, tracks_in, tracks_out }
```

`TrackOut` holds a `Weak<TrackIn>`. When a streamer disconnects, the upgrade fails and viewer-side tracks are cleaned up — no manual cross-reference bookkeeping.

## Negotiation

The client opens a DataChannel **before** `createOffer`, so the DC is part of the initial SDP. The server's `accept_offer` absorbs it; no follow-up `add_channel` call. Viewers joining a `LIVE` room get a renegotiation over the DC to add outgoing m-lines.

## Error handling

- Per-client errors are scoped by `Client::tick()`. One bad client doesn't take down the SFU loop. See [ADR 0005](../decisions/0005-error-isolation-via-tick.md).
- During renegotiation, state mutates only on the success path — no rollback. See [ADR 0006](../decisions/0006-track-out-state-error-categorization.md).

## Active feature work

- [Simulcast](../feature/simulcast.md)
- [P2P relay](../feature/p2p-relay.md)

## Known limits

- HTTP handlers block on `recv()` while the SFU iterates rooms. Deferred — see [ADR 0007](../decisions/0007-defer-sync-mpsc-recv-blocking.md).
- Single-process.
- Keyframe requests fan out to all viewers in the room. See [ADR 0004](../decisions/0004-keyframe-broadcast-with-debounce.md).
