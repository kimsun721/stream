# Baseline: the socket read on its own thread

Date: 2026-09-23
Commit: 7be296e on perf/71-shard-room
Previous: [2026-09-18](2026-09-18-datagram-routing.md)

## Why this run

The first step of issue #71. Reading the media socket moved from the media loop
to a mux thread of its own, which hands each datagram to the loop over the same
channel the web threads already use. There is still one loop. This run measures
what the extra hop costs before the loop is split.

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

| | Viewers | Evidence |
| --- | --- | --- |
| Healthy | 200 | No drops, `srv_cor` 0.68, `late_p99` 8192 as in every earlier run |
| Saturated | 400 | `srv_cor` reaches 0.88 and stops rising through 800 |
| Losing | none through 800 | `drop/s` is zero on every row. Overload now shows as delay instead, see below |

Cost per viewer at the healthy point: 4.8us of room pass per lap per viewer.

Relay saving, measured where the server is not saturated: 48 to 51 percent.

## Results

```text
viewers in one room  layers=l:300,m:800,h:2500 settle=5s window=10s
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   Mbps  save%
    50     50      0    0.25    0.19     788     2048     194     191      1      2    1507    1507       0   15142  130.4    0.0
   100    100      0    0.42    0.35     886     4096     334     329      1      3    2565    2565       0   30053  259.2    0.0
   200    200      0    0.89    0.68     629     8192     967     956      6      7    4713    4713       0   57212  494.4    0.0
   400    400      0    1.17    0.88     299    16384    2444    2408     24     30    8930    8930       0   57551  508.0    0.0
   500    500      0    1.19    0.89     266    16384    2657    2608     34     41   11028   11028       0   52377  461.6    0.0
   600    600      0    1.30    0.89     157    16384    4420    4333     67     84   13139   13139       0   52903  428.0    0.0
   800    800      0    1.47    0.90      58    32768   11059   10759    276    298   17344   17344       0   47956  285.6    0.0

viewers with relay  layers=l:300,m:800,h:2500 settle=5s window=10s
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   Mbps  save%
    50     50      0    0.23    0.19    1081     2048     133     130      0      2    1973    1973       0   10913   59.2   47.6
   100    100      0    0.42    0.37    1178     2048     242     237      1      3    3524    3524       0   23108  127.2   50.7
   200    200      0    0.87    0.68     873     4096     647     637      3      8    6672    6672       0   47322  263.2   47.8
   400    400      0    1.30    0.89     263    16384    2736    2697     21     49   12823   12823       0   52926  222.4   57.4

400 viewers across rooms  layers=l:300,m:800,h:2500 settle=5s window=10s
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   Mbps  save%
     1    400      0    1.12    0.88     344    16384    2075    2042     21     26    8927    8927       0   47675  426.4    0.0
     4    400      0    1.05    0.88     518     8192     327     320      2     20   10261   10261       0   48875  432.8    0.0
    20    400      0    1.14    0.92     398     8192      86      83      0     44   17501   17501       0   46915  413.6    0.0
```

## Analysis

**The hop costs nothing measurable.** The 400 row matches the previous run, and
`srv_cor`, which now includes the mux thread, is up by about 0.02.

**Drops are gone, and not because of the drain cap.** At 600 `rd/lap` is 84,
under even the old cap of 100. Before, nothing emptied the socket while the loop
walked its rooms, and the kernel buffer holds only about a hundred datagrams.
Now the mux empties it continuously into a channel 2048 deep. The 20 room row,
which lost 1636 a second last time, loses none.

**The knee improved because the loss feedback broke.** Lost packets used to
bring retransmits and ICE retries, which made each client cost more.

| Viewers | `laps/s` before | now | `room_us` before | now | `drop/s` before | now |
| --- | --- | --- | --- | --- | --- | --- |
| 500 | 151 | 266 | 4529 | 2657 | 38 | 0 |
| 600 | 106 | 157 | 6664 | 4420 | 3021 | 0 |
| 800 | 83 | 58 | 8754 | 11059 | 6355 | 0 |

**800 got worse.** The kernel used to shed half the input, so the loop handled
8288 datagrams a second. It now handles all 17344, laps fall to 58 a second, and
outbound falls from 368.8 to 285.6 Mbps. Overload has changed shape: it no longer
shows as `drop/s` but as falling `laps/s` and `Mbps` and a `late_p99` of 32768,
the time a datagram waits in the channel. `drop/s` only moves again once the
channel itself fills.

**Saturation did not move.** `srv_cor` sits at 0.88 to 0.90 from 400 on. The
ceiling is one core of str0m work, which this step does not touch.

**The idle floor of `late_p99` halved,** from 4096 to 2048. The old floor was the
socket read timeout's resolution; the loop now waits on a channel, which wakes
more precisely.

## Rows that cannot be used

`save%` at 400 in the relay scenario, 57.4 percent. There are no drops, but the
server is saturated, so leaves still hold their layer while the viewers it
serves degrade. The 50 to 200 rows are the ones to quote.

The 20 room row carries 20 publishers, so its `in/s` is not comparable with the
single room row's.

## Follow-up

1. Run N media loops, each owning the rooms that hash to it, with the web
   threads routing by room id and the mux handing every datagram to every loop.
2. Give the mux a map from source address to loop, so only unknown addresses
   are handed to all of them.
3. Re-measure with the loop count as a setting. The rooms scenario is where it
   should show; one room should not move.
