# System Overview

A WebRTC SFU for low-latency live streaming. One process; persistence and horizontal scaling are out of scope for now.

## Components

| | Role |
|---|---|
| ICE / DTLS / SRTP | Connectivity and transport security (via `str0m`) |
| mux | The only reader of the UDP socket. Hands each datagram to the media loop that owns its source address |
| Media loops | Each owns a share of the rooms, drives str0m for their clients and forwards RTP |
| HTTP API | Room CRUD and metrics for an external backend |
| HTTPS API | SDP offers from viewers, WHIP from publishers |
| DataChannel | Per-client side band: renegotiation, simulcast layer requests, P2P relay signaling |

The external backend is treated as a black box that speaks HTTP. This server makes no assumptions about its stack.

## Threading

- HTTP/HTTPS servers: tokio threads.
- mux: one std thread, blocked on the socket.
- Media loops: `server.total_shards` std threads, the core count by default. Each is the sole owner of its `Rooms`, as [ADR 0001](../decisions/0001-actor-pattern.md) describes for the single loop it started with.

Every loop has one `sync_channel<SfuMessage>` that both the web threads and the mux write to, so a loop waits on one thing: its channel, until the next str0m deadline. The mux copies each datagram into an owned buffer to cross the thread boundary, and drops it rather than wait when a loop's channel is full.

All loops send on the same socket, so clients see one address.

## Data flow

```
Browser ─────SDP─────▶ HTTPS ──┐ routed by room id
Backend ─────HTTP────▶ HTTP ───┤
                               ▼
Browser ──RTP/RTCP──▶ mux ──▶ media loop ──UDP──▶ Browsers (viewers)
                       ▲           │
                       └───────────┘ address owners
```

Media is forwarded per room: a streamer's RTP fans out to every viewer in the same room, and a viewer promoted to relay forwards it to one other viewer over a peer link.

## Placement and routing

A new room goes to the next loop in turn. Its id carries the loop number (`RM_2_...`), so every later command reaches the right loop without a table. Resolving a stream key has no room id to go on, so it asks every loop.

A datagram carries only its source address, so routing takes two maps:

| Map | Held by | Maps |
|---|---|---|
| Loop routes | mux | address to loop |
| `SocketRoutes` | each loop | address to client |

An address the mux does not know goes to every loop. The loop whose client `accepts` it records it in `SocketRoutes`, and `SocketRoutes` reports it to the mux, which from then on sends that address to that loop alone. Removing a route reports a release the same way. The mux applies a release only when it comes from the loop it has on record, because a freed address can be taken by a client on another loop before the release arrives.

## State

```
Rooms                                   one per media loop
├── rooms: RoomId → Room { state, hashed_stream_key }
│   ├── streamer: Option<Streamer { peer, tracks_in }>
│   └── viewers:  ClientId → Viewer { peer, tracks_out, relay_state }
└── socket_routes: SocketAddr → (ClientType, RoomId, ClientId)
```

`TrackOut` holds a `Weak<TrackIn>`. When a streamer disconnects, the upgrade fails and viewer-side tracks are cleaned up, with no cross-reference bookkeeping.

## Negotiation

The client opens a DataChannel **before** `createOffer`, so the DC is part of the initial SDP. The server's `accept_offer` absorbs it; no follow-up `add_channel` call. Viewers joining a `LIVE` room get a renegotiation over the DC to add outgoing m-lines.

## Error handling

- Per-client errors are scoped by each client's `tick`. One bad client does not take down its loop. See [ADR 0005](../decisions/0005-error-isolation-via-tick.md).
- During renegotiation, state mutates only on the success path, so there is nothing to roll back. See [ADR 0006](../decisions/0006-track-out-state-error-categorization.md).

## Feature docs

- [Simulcast](../feature/simulcast.md)
- [P2P relay](../feature/p2p-relay.md)

## Known limits

- HTTP handlers block on `recv()` while a loop works through its rooms. Deferred, see [ADR 0007](../decisions/0007-defer-sync-mpsc-recv-blocking.md).
- A room lives on one loop, so one room is bounded by one core. [#73](https://github.com/kimsun721/stream/issues/73) places clients instead.
- One process.
- Keyframe requests fan out to all viewers in the room. See [ADR 0004](../decisions/0004-keyframe-broadcast-with-debounce.md).
