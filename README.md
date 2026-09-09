# Stream

str0m based WebRTC SFU Streaming server

## Architecture

```
┌──────────────────────────────────────────────────┐
│                    Stream                        │
│                                                  │
│  ┌──────────────────┐  ┌──────────────────────┐  │
│  │    Axum HTTP     │  │      Axum HTTP       │  │
│  │  :8080 (Control) │  │  :8443 (SDP / WHIP)  │  │
│  └────────┬─────────┘  └──────────┬───────────┘  │
│           │        tokio threads  │              │
│  ─────────┴───────────────────────┴────────────  │
│  ┌────────────────────────────────────────────┐  │
│  │              SFU  (std thread)             │  │
│  │         UDP :40000  Main Loop              │  │
│  │         str0m + ICE                        │  │
│  │         Data Channel                       │  │
│  └────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────┘
```

Three threads run concurrently. The HTTP servers run on tokio threads; the SFU runs on a dedicated std thread and solely owns `Rooms`. Cross-thread communication (new clients, room CRUD, state queries) goes through `mpsc::sync_channel<SfuMessage>`. Both HTTP ports serve TLS when `[server.tls]` is configured, and plain HTTP otherwise, for deployments behind a reverse proxy.

## Running

```sh
cp .env.example .env            # set API_KEY, at least 16 characters
cp config.toml.example config.toml
docker compose up
```

Everything in `config.toml` has a default, so the file is optional. `.env` holds the one secret; everything else lives in the config file.

Behind NAT or in a container, set `server.public_ip` to an address clients can reach. Without it the server advertises the first interface it finds, which is the bridge address and unroutable from outside.

Without Docker, `cargo run` reads the same two files from the working directory.

## Authentication

Two credentials, on separate ports.

| Credential | Header | Used by | Scope |
| --- | --- | --- | --- |
| Server API key | `Authorization: Bearer <API_KEY>` | your backend | every control route |
| Stream key | `Authorization: Bearer <SK_...>` | publishers | one room |

The server API key comes from the environment and must never reach a browser. A stream key is issued per room, returned once, and stored only as a hash. It both authorizes publishing and selects the room, so no room id appears in publish URLs. See [ADR 0010](docs/decisions/0010-stream-key-authentication.md).

## Control API

HTTP `:8080` — consumed by an external backend. Requires the server API key.

| Method | Path | Description |
| --- | --- | --- |
| `POST` | `/rooms` | Create a room. Returns `room_id` and `stream_key` |
| `GET` | `/rooms/{id}` | Viewer count and room state |
| `PATCH` | `/rooms/{id}` | Set room state. `Idle` also ends the broadcast |
| `DELETE` | `/rooms/{id}` | Delete the room and its stream key |
| `POST` | `/rooms/{id}/stream-key` | Reissue, retiring the previous key |

## Client API

HTTP `:8443` — consumed by WebRTC clients directly.

| Method | Path | Auth | Description |
| --- | --- | --- | --- |
| `POST` | `/offer` | none | Viewer joins. SDP offer for SDP answer |
| `POST` | `/whip` | stream key | Publish. `application/sdp` in and out |
| `DELETE` | `/whip/sessions/{id}` | stream key | End the publishing session |

`/whip` works from OBS and from a browser alike.

## Room State

```
Idle  ──►  Preview  ──►  Live  ──┐
 ▲          publisher   control  │
 └─────────────────────────────◄─┘
```

A room starts `Idle`. A publisher passing stream key auth moves it to `Preview`, where media arrives but is not served. The control plane promotes `Preview` to `Live`, so connecting an encoder never publishes to viewers on its own. Ending the stream evicts everyone and returns the room to `Idle`, keeping its stream key.

A viewer calling `/offer` on an `Idle` or `Preview` room gets `404`. See [ADR 0011](docs/decisions/0011-room-lifecycle-and-go-live.md).

## P2P Relay

To reduce SFU bandwidth, stable viewers are promoted to relay nodes.

```
Default:  SFU ──► Client A
          SFU ──► Client B

Relay:    SFU ──► Client A (relay) ──► Client B
```

Promotion needs a healthy perf window, a passing upload probe, and a minimum connection age. A relay reports its real outbound rate and is demoted when it cannot sustain the load, returning its leaf to direct forwarding. Thresholds live in `config.toml`. See [ADR 0009](docs/decisions/0009-auto-promote-demote-policy.md) and [feature/p2p-relay.md](docs/feature/p2p-relay.md).

## Docs

- [Decisions](docs/decisions/) — ADRs recording why the design is what it is
- [Architecture](docs/architecture/overview.md) — components and data flow
- [Features](docs/feature/) — design docs for work in progress

## Roadmap

- [x] Simulcast — manual quality selection
- [x] P2P relay
- [x] Simulcast — BWE-driven auto-switching
- [x] WHIP support (OBS integration)
- [ ] Integration tests and a load baseline
- [ ] Room sharding across worker threads
- [ ] gRPC for external backend ↔ SFU communication
- [ ] VOD / replay support
- [ ] Stream thumbnail API

## Open Questions

- Data Channel message priority policy
- Latency minimization on SFU fallback
- HTTP/SFU latency under load — blocking `sync_channel::recv` in async handlers
- Room persistence — a restart drops every room and stream key
