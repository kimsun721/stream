# Baseline: forwarding latency at two loads

Date: 2026-10-01
Commit: 7bc0d78 on feat/75-observability
Previous: [2026-09-30](2026-09-30-room-sharding.md)

## Why this run

Issue #75 adds two latency figures. Channel wait runs from the mux reading a
datagram to a loop taking it off its channel. Forwarding delay runs from a
frame's first packet reaching its loop to the frame being written for a viewer.
This run measures both with all four loops busy, once at a comfortable load and
once at the ceiling.

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
| Settings | `LOAD_SCENARIOS=rooms LOAD_ROOMS=4`, `LOAD_ROOM_VIEWERS` at 800 and 1600, 4 loops |

## Headline

| | 800 viewers, 200 per loop | 1600 viewers, 400 per loop |
| --- | --- | --- |
| Forwarding delay, p50 | under 0.5 ms | under 4 ms |
| Forwarding delay, p99 | under 4 ms | under 16 ms |
| Channel wait, p99 | under 4 ms | under 16 ms |
| CPU | 3.06 cores | 3.42 cores |
| Outbound | 1352.0 Mbps | 1499.2 Mbps |

Each latency is the upper bound of the bucket the percentile falls in.

## Results

```text
800 viewers across rooms  layers=l:300,m:800,h:2500 settle=5s window=10s shards=4
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   Mbps  save% wait_p99 fwd_p50 fwd_p99
     4    800      0    3.23    3.06    2418     8192    1021    1005      8      8   18747   18747       0  154747 1352.0    0.0     4096     512    4096

1600 viewers across rooms  layers=l:300,m:800,h:2500 settle=5s window=10s shards=4
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   Mbps  save% wait_p99 fwd_p50 fwd_p99
     4   1600      0    4.65    3.42     517    16384    5524    5423     84     69   35626   35626       0  166479 1499.2    0.0    16384    4096   16384
```

## Analysis

**At a comfortable load the server adds little.** With 200 viewers per loop,
half the frames are written within half a millisecond of their first packet
arriving, and 99 percent within 4 ms.

**At the ceiling, latency follows the lap.** `laps/s` is summed over four loops,
so a loop laps about 600 times a second at 800 viewers and about 130 times at
1600, about 1.7 ms and 7.7 ms a lap. A datagram waits for its loop to come back
to the channel, and a frame waits for the pass that writes it, so both p99s land
within two or three laps at either load.

**The new histograms cost nothing measurable.** At 1600 viewers `laps/s` and
`room_us` match the previous baseline, 517 against 516 and 5524 against 5490.

**What this does not cover.** The time a packet spends in the pacer after the
write, in the kernel's queues, and on the network, and the encoder and decoder
at either end. End to end delay is for the demo in #76.

## Rows that cannot be used

Earlier runs on the same commit and settings were discarded. Between them
`laps/s` and outbound moved by about 20 percent and `late_p99` doubled, which no
change to the server explains. The runs above are the ones that match the
previous baseline's `laps/s` and `room_us`.

## Follow-up

1. End to end delay, shown in the demo for #76.
