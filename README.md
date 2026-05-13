# Stream

str0m based WebRTC SFU Streaming server

## Architecture

```
┌──────────────────────────────────────────────────┐
│                    Stream                        │
│                                                  │
│  ┌──────────────────┐  ┌──────────────────────┐  │
│  │    Axum HTTP     │  │     Axum HTTPS       │  │
│  │  (Internal API)  │  │  :8080 (SDP Offer)   │  │
│  └────────┬─────────┘  └──────────┬───────────┘  │
│           │        tokio threads  │              │
│  ─────────┴───────────────────────┴────────────  │
│  ┌────────────────────────────────────────────┐  │
│  │              SFU  (std thread)             │  │
│  │         UDP Socket Main Loop               │  │
│  │         str0m + ICE                        │  │
│  │         Data Channel                       │  │
│  └────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────┘
```

Three threads run concurrently. The HTTP and HTTPS servers run on tokio threads; the SFU runs on a dedicated std thread and solely owns `Rooms`. Cross-thread communication (new clients, room CRUD, state queries) goes through `mpsc::sync_channel<SfuMessage>`.

## Internal API

HTTP `:8443` — consumed by an external backend.

| Method   | Path          | Description                  |
| -------- | ------------- | ---------------------------- |
| `POST`   | `/rooms/{id}` | Create a room                |
| `GET`    | `/rooms/{id}` | Get room info (viewer count) |
| `PATCH`  | `/rooms/{id}` | Update room state            |
| `DELETE` | `/rooms/{id}` | Delete a room                |

HTTPS `:8080` — consumed by WebRTC clients directly.

| Method | Path     | Description                       |
| ------ | -------- | --------------------------------- |
| `POST` | `/offer` | Exchange SDP offer for SDP answer |

### Room State

```
IDLE  ──►  PREVIEW  ──►  LIVE
```

### P2P Relay _(planned)_

To reduce SFU bandwidth, high-quality clients can be promoted to relay nodes.

```
Default:  SFU ──► Client A
          SFU ──► Client B

Relay:    SFU ──► Client A (relay) ──► Client B
```

1. Measure client upload speed and stability via Data Channel
2. Promote stable clients to relay nodes
3. Negotiate P2P connection over Data Channel
4. Fall back to SFU if relay quality degrades

## Roadmap

- [ ] Simulcast — manual quality selection
- [ ] P2P relay
- [ ] Simulcast — BWE-driven auto-switching
- [ ] WHIP support (OBS integration)
- [ ] gRPC for external backend ↔ SFU communication
- [ ] VOD / replay support
- [ ] Stream thumbnail API

## Open Questions

- Data Channel message priority policy
- Relay node selection criteria
- Latency minimization on SFU fallback
- HTTP/SFU latency under load — blocking `sync_channel::recv` in async handlers
