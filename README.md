# Stream

A WebRTC SFU for live streaming, written in Rust on [str0m](https://github.com/algesten/str0m). Publishers push over WHIP, from OBS or a browser. Viewers get the simulcast layer their bandwidth estimate allows, and viewers with upload to spare relay the stream to each other over a peer link, taking load off the server.

## Measured

One machine, over loopback, with the load generator on the same machine (AMD Ryzen 7 8845HS, 16 threads). The publisher sends three simulcast layers.

| | |
| --- | --- |
| One media loop | saturates at about 400 viewers, on one core |
| Four media loops | 1600 viewers across 4 rooms, no packet loss |
| P2P relay | 46 to 50 percent of media bytes carried by viewers. One relay feeds one viewer, so half is the ceiling |

Each run, and what it ruled out, is recorded in [docs/test/baseline](docs/test/baseline/).

## Architecture

```
backend ─── HTTP  :8080 ──┐
clients ─── HTTPS :8443 ──┤ tokio
                          │ room commands, routed by room id
                          ▼
clients ─── UDP :40000 ──▶ mux ──▶ media loops 0..N ──▶ UDP :40000 ──▶ clients
                            ▲             │
                            └─ address ◀──┘
                               owners
```

The mux thread is the only reader of the UDP socket. It hands a datagram to the media loop that owns its source address, or to every loop when the address is new, and the loop whose client accepts it reports the address back.

Each media loop is a std thread that owns its rooms outright, so nothing in a room is shared across threads. All loops send on the same socket, so clients see a single port. A new room goes to the next loop in turn, and its id carries the loop number, so the web threads route every later command without a lookup table. There are `server.total_shards` loops, the core count by default.

## Running

```sh
cp .env.example .env            # set API_KEY, at least 16 characters
cp config.toml.example config.toml
docker compose up
```

Everything in `config.toml` has a default, so the file is optional. `.env` holds the one secret; everything else lives in the config file.

Behind NAT or in a container, set `server.public_ip` to an address clients can reach. Without it the server advertises the first interface it finds, which is the bridge address and unroutable from outside.

`[server.tls]` serves TLS on `:8443`. `:8080` is always plain HTTP and belongs on a private network. Without the section both are plain, for deployments behind a reverse proxy.

Without Docker, `cargo run` reads the same two files from the working directory.

## Authentication

Two credentials, on separate ports.

| Credential | Header | Used by | Scope |
| --- | --- | --- | --- |
| Server API key | `Authorization: Bearer <API_KEY>` | your backend | every control route |
| Stream key | `Authorization: Bearer <SK_...>` | publishers | one room |

The server API key comes from the environment and must never reach a browser. A stream key is issued per room, returned once, and stored only as a hash. It both authorizes publishing and selects the room, so no room id appears in publish URLs. See [ADR 0010](docs/decisions/0010-stream-key-authentication.md).

## Control API

HTTP `:8080`, for your backend. Every route requires the server API key.

| Method | Path | Description |
| --- | --- | --- |
| `POST` | `/rooms` | Create a room. Returns `room_id` and `stream_key` |
| `GET` | `/rooms/{id}` | Viewer count and room state |
| `PATCH` | `/rooms/{id}` | Set room state. `Idle` also ends the broadcast |
| `DELETE` | `/rooms/{id}` | Delete the room and its stream key |
| `POST` | `/rooms/{id}/stream-key` | Reissue, retiring the previous key |
| `GET` | `/metrics` | Counters and timings for the media loops, cumulative since start |

## Client API

HTTPS `:8443`, called by clients directly.

| Method | Path | Auth | Description |
| --- | --- | --- | --- |
| `POST` | `/offer` | none | Viewer joins. SDP offer in, SDP answer out |
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

## Testing

```sh
cargo test                                                    # unit and integration
cargo test --release --test load -- --ignored --nocapture     # load run
```

The integration suites start the real server and drive it over HTTP and UDP with str0m clients, including a publisher that sends simulcast RTP. [docs/test](docs/test/README.md) covers the suites, the load run and how to read it.

## Limits

- No TURN. A client that cannot reach UDP `:40000` directly cannot connect.
- A room lives on one media loop, so one room is bounded by one core. [#73](https://github.com/kimsun721/stream/issues/73) lets a room span loops.
- Rooms and stream keys live in memory. A restart drops them.
- An HTTP handler holds a tokio worker while it waits for a media loop to answer. See [ADR 0007](docs/decisions/0007-defer-sync-mpsc-recv-blocking.md).
- One process. Nothing spreads load across servers.

## Docs

- [Decisions](docs/decisions/): ADRs recording why the design is what it is
- [Architecture](docs/architecture/overview.md): components, threads and data flow
- [Features](docs/feature/): design docs for simulcast and the P2P relay
- [Tests](docs/test/README.md): suites, the load run, and recorded baselines

## Roadmap

- [x] Simulcast with manual layer selection
- [x] P2P relay
- [x] Simulcast layers switched by bandwidth estimate
- [x] WHIP, for OBS
- [x] Integration tests and a load baseline
- [x] Rooms spread across media loops
- [ ] Place clients rather than rooms, so one room can span loops
- [ ] Persist rooms and stream keys across restarts
