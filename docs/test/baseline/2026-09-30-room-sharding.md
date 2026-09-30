# Baseline: rooms spread across four media loops

Date: 2026-09-30
Commit: bcd6b13 on perf/71-shard-room
Previous: [2026-09-23](2026-09-23-mux-thread.md)

## Why this run

Issue #71 now runs a fixed set of media loops, places each new room on the next
loop in turn, and has the mux send a known address only to the loop that owns
it. The same run at one loop and at four measures what splitting buys, and a
larger run finds where four loops saturate.

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
| Settings | defaults with `LOAD_SHARDS=1` and `4`; the last run as shown in its header |

## Headline

| | 1 loop | 4 loops |
| --- | --- | --- |
| 400 viewers, 1 room | 0.88 cores, 48.8 MB/s | 0.86 cores, 54.3 MB/s |
| 400 viewers, 4 rooms | 0.89 cores, 42.5 MB/s | 1.87 cores, 128.7 MB/s |
| 400 viewers, 20 rooms | 0.92 cores, 43.9 MB/s | 1.83 cores, 123.8 MB/s |
| 1600 viewers, 4 rooms | | 3.41 cores, 173.8 MB/s |

One loop still saturates at 400 viewers. Four loops saturate at 1600.

Relay saving at four loops, where the server is not saturated: 46 to 49
percent.

## Results

```text
viewers in one room  layers=l:300,m:800,h:2500 settle=5s window=10s shards=1
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   MB/s  save%
    50     50      0    0.21    0.16     659     2048     215     212      1      2    1508    1508       0   15186   16.4    0.0
   100    100      0    0.41    0.34     676     4096     442     436      2      4    2567    2567       0   30142   32.5    0.0
   200    200      0    0.86    0.70     650     8192     922     910      6      7    4711    4711       0   46974   50.9    0.0
   400    400      0    1.12    0.88     304    16384    2381    2345     24     29    8921    8921       0   50152   55.3    0.0
   500    500      0    1.23    0.89     181    16384    3928    3856     54     61   11024   11024       0   49053   53.1    0.0
   600    600      0    1.29    0.90     130    16384    5283    5175     86    101   13123   13123       0   47206   42.1    0.0
   800    800      0    1.41    0.90      59    32768   11018   10730    263    295   17330   17330       0   48216   36.2    0.0

viewers with relay  layers=l:300,m:800,h:2500 settle=5s window=10s shards=1
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   MB/s  save%
    50     50      0    0.18    0.16    1107     2048     116     114      0      2    1972    1972       0   11975    8.5   37.8
   100    100      0    0.40    0.36    1142     2048     251     246      0      3    3522    3522       0   24160   17.0   49.8
   200    200      0    0.90    0.72     741     4096     830     818      4      9    6664    6664       0   47688   33.7   44.6
   400    400      0    1.25    0.89     263    16384    2781    2742     21     49   12785   12785       0   54969   29.5   55.3

400 viewers across rooms  layers=l:300,m:800,h:2500 settle=5s window=10s shards=1
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   MB/s  save%
     1    400      0    1.09    0.88     304    16384    2352    2314     25     29    8930    8930       0   44005   48.8    0.0
     4    400      0    1.01    0.89     400    16384     415     406      4     26   10239   10239       0   38422   42.5    0.0
    20    400      0    1.09    0.92     297    16384     115     111      0     59   17459   17459       0   40105   43.9    0.0

viewers in one room  layers=l:300,m:800,h:2500 settle=5s window=10s shards=4
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   MB/s  save%
    50     50      0    0.18    0.15     821     2048     167     165      0      2    1506    1506       0   15169   16.3    0.0
   100    100      0    0.33    0.30     892     4096     308     304      1      3    2567    2567       0   29945   32.3    0.0
   200    200      0    0.62    0.57     826     8192     618     609      4      6    4706    4706       0   47674   51.7    0.0
   400    400      0    1.05    0.87     375    16384    2145    2115     19     24    8927    8927       0   60185   66.1    0.0
   500    500      0    1.09    0.89     328    16384    2384    2342     28     34   11013   11013       0   51728   57.3    0.0
   600    600      0    1.22    0.89     175    16384    4864    4776     67     75   13134   13134       0   52857   54.2    0.0
   800    800      0    1.36    0.89      93    32768   10525   10262    238    187   17347   17347       0   51336   38.1    0.0

viewers with relay  layers=l:300,m:800,h:2500 settle=5s window=10s shards=4
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   MB/s  save%
    50     50      0    0.17    0.15    1083     2048     117     115      0      2    1968    1968       0   12008    8.6   45.7
   100    100      0    0.32    0.31    1205     2048     211     207      0      3    3531    3531       0   24128   17.0   49.3
   200    200      0    0.66    0.60    1027     4096     510     502      2      6    6661    6661       0   48390   33.9   46.3
   400    400      0    1.20    0.88     341     8192    2436    2408     14     37   12725   12725       0   65470   40.3   49.5

400 viewers across rooms  layers=l:300,m:800,h:2500 settle=5s window=10s shards=4
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   MB/s  save%
     1    400      0    0.99    0.86     461    16384    1631    1605     16     19    8932    8932       0   48762   54.3    0.0
     4    400      0    2.22    1.87    2896     4096     554     547      3      4   10284   10284       0  119217  128.7    0.0
    20    400      0    1.68    1.83    4712     2048      60      59      0      4   17474   17474       0  114838  123.8    0.0

1600 viewers across rooms  layers=l:300,m:800,h:2500 settle=5s window=10s shards=4
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   MB/s  save%
     4   1600      0    4.63    3.41     516    16384    5490    5396     77     69   35598   35598       0  160670  173.8    0.0
     8   1600      0    4.24    3.47     653    16384    2121    2081     29     57   37409   37409       0  147943  154.4    0.0
    16   1600      0    4.18    3.50     697    16384     980     959     12     59   40981   40980       1  142804  148.0    0.0
```

## Analysis

**One room does not move.** A room lives on one loop, so the one room rows burn
the same 0.86 to 0.88 cores at one loop and at four.

**Several rooms use several cores.** At 400 viewers in 4 rooms, four loops burn
1.87 cores and send three times the bytes of one loop. One loop was saturated
and held its viewers on low layers; four loops at 100 viewers each serve the
layers the viewers can take.

**The ceiling scales with the loops.** At 1600 viewers every loop sits at about
0.86 cores, the same point where a single loop saturates at 400. Four loops hold
four times the viewers without drops.

**Bytes scale less than cores.** 1600 viewers send 173.8 MB/s, 79 percent of four
times the single loop's 55.3. The generator burns 4.6 cores beside the server's
3.4 on a machine with 8 physical cores, so the two share cores through SMT. This
machine cannot separate that from the server's own cost.

**The mux keeps up.** One thread reads 41,000 datagrams a second with no kernel
drops.

## Rows that cannot be used

`save%` at 400 in the relay scenario, saturated, and at 50 on one loop, measured
before every pair was promoted.

The 20 and 16 room rows carry one publisher per room, so their `in/s` is not
comparable with fewer rooms.

Two earlier runs on the same commit were discarded. Background load doubled
`gen_cor` and `room_us` at 50 viewers, which no change to the server can
explain.

## Follow-up

1. Place clients rather than rooms. A room still cannot outgrow one loop, and a
   room placed empty says nothing about the load it will bring.
2. Measure on separate machines, to split the server's cost from the generator's.
