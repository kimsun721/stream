# Baseline: one publisher, viewers added until the server loses packets

Date: 2026-09-15
Commit: 30776bc on feat/68-integration-tests
Previous: none

## Why this run

The first measurement. Nothing before it could be measured at all: the load
harness, the counters it reads and the endpoint it reads them from all landed in
this issue. Everything queued behind it is performance work, and none of that
can be judged without a figure to compare against.

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
| Healthy | 200 | No drops, `late_p99` 8192 against an idle floor of 4096, `srv_cor` 0.77 |
| Saturated | 400 | `srv_cor` reaches 0.85 and stops rising through 800 |
| Losing | 600 | `drop/s` leaves zero, at 3850 |

Cost per viewer at the healthy point: 5.9us of room pass per lap per viewer.

Relay saving, measured where `drop/s` is zero: 47%.

The loss point moves between runs. An earlier run on the same commit lost
packets at 500, this one at 600. Treat 500 to 600 as the knee and only read a
later change as real if it moves more than that.

## Results

```text
viewers in one room  layers=l:300,m:800,h:2500 settle=5s window=10s
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   MB/s  save%
    50     50      0    0.47    0.32     635     4096     459     454      2      2    1512    1512       0   14653   15.8    0.0
   100    100      0    0.65    0.54     714     8192     669     661      4      4    2585    2585       0   27997   30.3    0.0
   200    200      0    0.91    0.77     560     8192    1188    1174     10      8    4689    4689       0   41347   45.0    0.0
   400    400      0    1.09    0.85     474    16384    1391    1370     16     19    8923    8923       0   49984   54.9    0.0
   500    500      0    1.24    0.87     285    16384    2336    2300     30     39   11030   11030       0   50551   55.5    0.0
   600    600      0    1.35    0.85     137    32768    5061    4988     62     61   12277    8427    3850   54753   54.8    0.0
   800    800      0    1.45    0.86      86    32768    8222    8088    112    100   14620    8599    6022   53180   47.1    0.0

viewers with relay  layers=l:300,m:800,h:2500 settle=5s window=10s
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   MB/s  save%
    50     50      0    0.53    0.36     895     4096     323     319      0      2    1993    1993       0   11729    8.2   38.1
   100    100      0    0.71    0.55     958     4096     476     471      1      4    3548    3548       0   23120   15.8   47.3
   200    200      0    0.91    0.74     758     8192     834     826      4      9    6675    6675       0   39395   24.4   47.9
   400    400      0    1.25    0.85     356     8192    1963    1943     13     36   12726   12726       0   58267   35.6   46.9

400 viewers across rooms  layers=l:300,m:800,h:2500 settle=5s window=10s
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   MB/s  save%
     1    400      0    1.12    0.86     387    16384    1803    1779     18     23    8925    8925       0   54162   59.5    0.0
     4    400      0    1.13    0.87     360     8192     472     466      4     29   10281   10281       0   48938   54.0    0.0
    20    400      0    1.17    0.88     320     8192     102     101      0     55   17590   17445     144   49220   53.8    0.0
```

## Analysis

**The room pass is where the server spends itself.** `room_us` times rooms times
`laps/s` accounts for 74 to 91 percent of `srv_cor` on every row of every
scenario, and `tick_us` is 98 percent of `room_us`. The cost is polling clients,
not fanning media out: `fan_us` never passes 112us even at 800 viewers.

| Viewers | Room pass | Share of `srv_cor` |
| --- | --- | --- |
| 50 | 0.29 s/s | 91% |
| 200 | 0.67 s/s | 86% |
| 400 | 0.66 s/s | 78% |
| 800 | 0.71 s/s | 82% |

**That is what starves the socket.** The room pass holds a fixed 0.7 seconds of
every second from 200 viewers on, so `laps/s` has to fall as each lap grows:
714, 560, 474, 285, 137, 86. The loop is away from the socket longer each time,
and the kernel's receive buffer holds only about a hundred datagrams, so once
more than that arrive while the loop is busy the rest are discarded. `in/s` goes
on rising to 14620 while `rx/s` falls away from it, and the gap is `drop/s`.

Two mechanisms, not one. At 600 `rd/lap` is 61, well under the cap of 100, so
the drain was not the limit: the buffer overflowed while the loop was elsewhere.
At 800 `rd/lap` is exactly 100 and the drain cap binds as well.

**The drops feed back.** Cost per viewer is 3.5 to 5.9us up to 500 and then 8.4
and 10.3 at 600 and 800. Lost packets mean retransmit timers and ICE retries,
which is more work per client, which slows the lap further.

**Relay removes bandwidth, not CPU.** At 200 viewers it cuts outbound from 45.0
to 24.4 MB/s, and the 47.9 percent it reports reconstructs a total of 46.8 MB/s
against the 45.0 measured without it, so the counter agrees with itself. But
`srv_cor` at 400 is 0.85 either way. A leaf is still a client: it is still
polled, still sends RTCP, still costs a tick.

**Splitting rooms does nothing.** 400 viewers across 1, 4 and 20 rooms cost
0.86, 0.87 and 0.88 cores. The room pass share is 81, 78 and 74 percent. Walking
more rooms is not what the loop is paying for.

## Rows that cannot be used

`save%` on rows with drops. Leaves receive no media, so their bandwidth estimate
freezes and automatic layer selection cannot move them down, while the viewers
the server still serves do degrade. The numerator holds while the denominator
falls. Every `save%` quoted above comes from a row with `drop/s` at zero.

`save%` at 50 viewers, 38.1 percent. The step measures shortly after those
viewers connect and not every pair has been promoted yet. The figure settles at
47 from 100 viewers on.

The 20 room row carries 20 publishers, not one. `in/s` is 17590 against 8925 in
the single room row for the same 400 viewers, because each publisher sends three
layers. That row says room count does not help; it does not isolate room count.

The first row of each scenario runs just after the previous one tore down, and
`gen_cor` is visibly higher there. Read the low viewer rows as approximate.

## Follow-up

1. Route datagrams by source address. `route_socket_input` scans every client
   calling `accepts` on each, so each datagram costs O(clients). Reaching a
   client in constant time needs `Room.clients` keyed rather than a `Vec`, which
   also removes the seven `find(|c| c.id == ...)` scans in the relay path.
2. Stop feeding `Input::Timeout` to every client every lap. str0m says when each
   one next needs one.
3. Raise or remove the drain cap, once a datagram is cheap enough that draining
   is not itself the cost.
4. Re-measure. Room sharding comes after that, because splitting the room pass
   is not what this run says is wrong.
