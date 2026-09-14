# Tests

Unit tests live beside the code they cover. Everything under `tests/` talks to a
real server process over HTTP and UDP, linking nothing from the binary, so it
exercises the same paths a browser would.

## Running

```text
cargo test                          unit and integration
cargo test -- --include-ignored     adds the tests that need ffmpeg installed
```

The load run is separate and is described below.

## Suites

| File | Covers |
| --- | --- |
| `tests/viewer.rs` | Connecting, receiving media, layer advertisement, manual and automatic layer selection |
| `tests/room_lifecycle.rs` | Ending a WHIP session, going idle, refusing viewers before a room is live |
| `tests/control_api.rs` | API key, stream key, content type, duplicate streamer, key reissue, missing rooms |
| `tests/relay.rs` | Relay promotion, the upload cutoff, leaf gating, the peer link timeout |
| `tests/load.rs` | The load baseline. Ignored by default |

Each load run worth keeping is recorded under [`baseline/`](baseline/), from
[`_template.md`](baseline/_template.md), so a later run is compared against an
earlier one rather than against memory.

## The harness

`tests/common/mod.rs` spawns one server per test binary and hands out clients
built on str0m. A few things about it are worth knowing before writing a test.

**The server is a real process.** It is spawned once per binary, given its own
working directory and `config.toml`, and killed with the test binary. Rooms
carry random ids, so tests never collide and there is no reason to pay for a
process each.

**The test config scales down the gates that are counted in minutes**, and none
of them to zero. A test that disables a gate stops proving the gate works. The
value cutoffs stay at their defaults and the clients report numbers on either
side of them.

**The publisher sends real RTP.** It declares simulcast and writes fixed size
frames per rid at a rate the test picks. The payload is not VP8: the server
forwards frames without decoding them and sizes a layer by byte count alone, so
bytes of the right length are indistinguishable from an encoder's. This is what
makes layer estimates, egress bandwidth estimates and relay savings measurable
at all, none of which the SDP alone can produce.

**Relay clients answer the protocol on their own.** A peer replies to an upload
probe and to a peer to peer offer without the test arranging it. Declaring the
link up is opt in, because the tests that assert on the moment it happens need
to send that themselves.

## Load run

```text
cargo test --release --test load -- --ignored --nocapture
```

Release matters. A debug build spends most of its time in str0m's crypto and
measures nothing about this server.

Every setting comes from the environment, because `cargo test` owns the command
line and rejects flags of our own. The defaults are what the recorded baseline
was measured with, so a plain run stays comparable to it.

| Variable | Default | Meaning |
| --- | --- | --- |
| `LOAD_SCENARIOS` | `viewers,relay,rooms` | Which scenarios to run |
| `LOAD_VIEWERS` | `50,100,200,400,500,600,800` | Steps for the first scenario |
| `LOAD_RELAY_VIEWERS` | `50,100,200,400` | Steps for the second |
| `LOAD_ROOMS` | `1,4,20` | Room counts for the third |
| `LOAD_ROOM_VIEWERS` | `400` | Viewers split across those rooms |
| `LOAD_RELAY_SHARE` | `2` | One viewer in this many advertises relay upload |
| `LOAD_LAYERS` | `l:300,m:800,h:2500` | The publisher's layers, in kbps |
| `LOAD_SETTLE` | `5` | Seconds before a step starts measuring |
| `LOAD_WINDOW` | `10` | Seconds a step measures over |

An uneven room split is refused rather than rounded away, since rooms of
different sizes cannot be compared with each other.

### Scenarios

**Viewers in one room.** What one viewer costs, which every other run is
compared against.

**Viewers with relay.** The same sweep with some viewers advertising the upload
a relay needs. One relay carries one leaf, so half the viewers is the most that
can be paired.

**Viewers across rooms.** One viewer count split across more rooms, which
separates the cost of walking the room map from the cost of what a room holds.

Each scenario deletes its rooms when it finishes. Without that the server would
carry the last scenario's viewers into the next one for as long as their ICE
takes to time out, and measure them too.

### Columns

| Column | Meaning |
| --- | --- |
| `step` | The step's target: viewers, or rooms in the third scenario |
| `live` | Viewers the server actually holds. A gap against `step` means connects are failing |
| `refus` | Connects refused at the HTTP layer |
| `gen_cor` | Cores the load generator itself burned |
| `srv_cor` | Cores the server burned. The media loop is one thread, so 1.0 is the ceiling |
| `laps/s` | Media loop iterations per second |
| `late_p99` | How far past the deadline str0m asked for the loop woke, in microseconds |
| `room_us` | Mean time for one room iteration |
| `tick_us` | Of that, polling the room's clients |
| `fan_us` | Of that, writing the publisher's frames to viewers |
| `rd/lap` | Datagrams read per lap. At 100 the drain stopped at its own cap |
| `in/s` | Datagrams that arrived |
| `rx/s` | Of those, the ones read |
| `drop/s` | Of those, the ones the kernel discarded |
| `tx/s` | Datagrams sent, one `sendto` each |
| `MB/s` | Outbound bytes |
| `save%` | Media bytes the relays carried instead of the server |

### Reading a run

Check `refus` and the gap between `step` and `live` first. If either moved, the
run lost clients and the rest of the row describes a different population than
intended.

Then compare `gen_cor` against `srv_cor`. If the generator is far larger, the
run measured this machine rather than the server.

The figures worth deriving:

| Question | Figure |
| --- | --- |
| What does the room pass cost | `room_us` times rooms times `laps/s`, against `srv_cor` |
| What does one viewer cost | `room_us / live` |
| Is the socket keeping up | `rx/s` against `in/s`, with `rd/lap` saying whether the cap was the reason |
| How much traffic do relays remove | `save%`, but only on rows where `drop/s` is zero |

### What the figures cannot say

`late_p99` has a floor of about 4096 even on an idle server. That is the socket
read timeout's own resolution, not the loop running late, so read the change
under load rather than the absolute value.

Percentiles are the upper bound of the bucket the sample landed in. A reported
512 means "under 512", with the true figure somewhere in the octave below.

`save%` inflates once packets are being dropped. Leaves stop receiving media, so
their bandwidth estimate freezes and automatic layer selection can no longer
move them down, while the viewers the server still serves do degrade. The
numerator holds while the denominator falls.

Loopback has no loss and a very large MTU, so this measures where the server
runs out of CPU, not where it runs out of network. The generator shares the
machine with the server, which is why its own CPU is reported beside it.
