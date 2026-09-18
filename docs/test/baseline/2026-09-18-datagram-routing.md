# Baseline: five changes that did not move the numbers

Date: 2026-09-18
Commit: d439c4d on perf/70-constant-datagram-cost
Previous: [2026-09-15](2026-09-15-integration-tests.md)

## Why this run

Issue #70 set out to make a datagram cost the same whatever the client count.
Five changes landed toward that: clients keyed by id rather than held in a `Vec`,
a client split into a streamer and a viewer so the per lap streamer scan
disappears, datagrams routed by their source address instead of a scan, routes
dropped when their client leaves, and a relay leaf chosen so it never consumes a
relay candidate.

This run measures all five together, and a profile explains what it found.

## Environment

Figures are only comparable against a run from the same machine, so the whole
of it goes here.

| | |
| --- | --- |
| CPU | AMD Ryzen 7 8845HS |
| Cores | 16 |
| Kernel | 7.1.4-1-g14 |
| `net.core.rmem_default` | 212992 |
| rustc | 1.97.1 |
| Profile | release |
| Transport | loopback |
| Settings | defaults |

## Headline

Unchanged from the previous baseline.

| | Viewers | Evidence |
| --- | --- | --- |
| Healthy | 200 | No drops, `late_p99` 8192 against an idle floor of 4096 |
| Saturated | 400 | `srv_cor` reaches 0.86 and stops rising through 800 |
| Losing | 600 | `drop/s` leaves zero in earnest, at 3021 |

Relay saving, measured where `drop/s` is zero: 49 to 50 percent, up from the 47
of the previous run now that a leaf never consumes a relay candidate.

## Results

```text
viewers in one room  layers=l:300,m:800,h:2500 settle=5s window=10s
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   MB/s  save%
    50     50      0    0.27    0.21     702     4096     253     249      1      2    1513    1513       0   15127   16.3    0.0
   100    100      0    0.45    0.36     803     4096     390     385      2      3    2571    2571       0   29532   31.9    0.0
   200    200      0    0.85    0.68     675     8192     883     871      5      7    4715    4715       0   53853   58.3    0.0
   400    400      0    1.17    0.86     286    16384    2497    2461     24     31    8927    8927       0   54078   59.9    0.0
   500    500      0    1.28    0.85     151    16384    4529    4441     68     73   11023   10985      38   49957   54.5    0.0
   600    600      0    1.34    0.85     106    32768    6664    6557     80     88   12304    9284    3021   52545   52.3    0.0
   800    800      0    1.46    0.85      83    32768    8754    8630     91    100   14643    8288    6355   52284   46.1    0.0

viewers with relay  layers=l:300,m:800,h:2500 settle=5s window=10s
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   MB/s  save%
    50     50      0    0.24    0.19     910     4096     170     167      0      2    1985    1985       0   11872    8.6   50.0
   100    100      0    0.44    0.36    1071     4096     274     268      1      3    3542    3542       0   24014   16.9   50.0
   200    200      0    0.88    0.68     856     4096     670     659      3      8    6675    6675       0   47580   33.0   49.1
   400    400      0    1.29    0.84     297    16384    2387    2352     18     43   12685   12678       7   54026   31.4   57.9

400 viewers across rooms  layers=l:300,m:800,h:2500 settle=5s window=10s
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   MB/s  save%
     1    400      0    1.01    0.84     482    16384    1370    1344     15     19    8938    8938       0   45065   50.0    0.0
     4    400      0    1.12    0.86     397     8192     436     428      3     26   10263   10263       0   49100   53.0    0.0
    20    400      0    1.18    0.87     253     8192     136     133      0     64   17810   16174    1636   47400   48.5    0.0
```

## Analysis

**Nothing moved where it counts.** The room pass still holds the same share of a
saturated loop, and the CPU figure is identical.

| Viewers | Room pass, previous | Room pass, now | `srv_cor` then | now |
| --- | --- | --- | --- | --- |
| 400 | 0.659 s/s | 0.714 s/s | 0.85 | 0.86 |
| 500 | 0.666 | 0.684 | 0.87 | 0.85 |
| 600 | 0.693 | 0.706 | 0.85 | 0.85 |
| 800 | 0.707 | 0.727 | 0.86 | 0.85 |

The 50 to 200 rows read better than last time, but so does the generator's own
CPU on the same rows, which no change to the server can explain. Those rows
differ by machine state, not by code.

**A profile says why.** Sampled at 999Hz over 20 seconds against the server
process alone, at the 400 viewer step:

| Group | Share | Largest members |
| --- | --- | --- |
| str0m `poll_output` machinery | ~12% | `do_poll_output`, `Session::handle_timeout`, `Dtls::poll_output`, `poll_event`, `poll_datagram` |
| RTP send | ~10% | `StreamTx::poll_packet`, `Media::do_payload`, `EvictingBuffer` |
| TWCC and BWE | ~8% | `TwccSendRegister::apply_report` at 4.79, the largest single symbol in the whole profile |
| SCTP | ~6% | `gather_data_packets_to_retransmit`, `Association::poll_transmit` |
| Crypto | ~3% | `aes_gcm_encrypt_avx512` |
| **This server's own code** | **1.5%** | `Viewer::tick`, and nothing else in the top thirty |

`route_socket_input`, `count_gauges`, the room walk and the relay policy do not
appear in the top thirty symbols at all. That is the finding: the cost is str0m's
per client state machine, spread evenly, with no single place to attack. The
largest symbol in the profile is under five percent.

Per client, at 400 viewers, one `tick` costs 5.7us: `2461us / 400`, times
124,400 calls a second, is the 0.71 seconds the room pass takes.

**The changes were still right, they were just invisible here.** A datagram now
costs one hash lookup rather than a scan of every client, the streamer is reached
without walking the room, and route entries no longer outlive their clients. Each
removes real algorithmic cost or a real leak. None of it shows because all of it
lives inside the 1.5 percent.

## Rows that cannot be used

The 50 to 200 rows, for comparison against the previous baseline. `gen_cor`
differs by nearly a factor of two on those rows, which is machine state.

`save%` at 400 in the relay scenario, 57.9 percent. The server is saturated
there, so leaves freeze at the layer they had while the viewers the server still
serves degrade. The numerator holds while the denominator falls. The 50, 100 and
200 rows sit at the 1 to 1 ceiling, which is what that figure should look like.

The 20 room row carries 20 publishers, so its `in/s` of 17810 is not comparable
with the single room row's 8938.

## Follow-up

**Stop optimizing the single thread.** The profile leaves ~12% reachable, and two
attempts at the rest returned nothing measurable. The loop uses 0.85 of one core
on a 16 core machine, and the work is per client, independent and evenly spread,
which is the shape parallelism suits.

1. Shard rooms across worker threads, issue #71. Deciding socket ownership comes
   first, because one reader thread is the bottleneck whatever the rooms do.
2. Re-measure. A fourth baseline against this one.

Selective polling, the last item left in #70, stays open but is not worth doing
first. It attacks the `poll_output` machinery, but a viewer receiving media has
pacer work due almost every lap at these lap rates, so there is little to skip.
It would pay off for relay leaves, which receive nothing from the server, and
that is the only scenario where it should be expected to show. It also has no
safety net: a wrong deadline silently stops polling a client.
