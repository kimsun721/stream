# Baseline: where one loop's time goes

Date: 2026-10-13
Commit: 2607f70 on main
Previous: [2026-10-01](2026-10-01-forwarding-latency.md)

## Why this run

The [2026-09-18 profile](2026-09-18-datagram-routing.md) was read as saying this
server's own code leaves nothing to optimize, and the work moved to sharding.
This run profiles one loop again, with call stacks and kernel time, to check
that reading.

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
| Profile | release, with debug info and frame pointers for the profiler |
| Transport | loopback |
| Settings | `LOAD_SCENARIOS=viewers LOAD_SHARDS=1 LOAD_SETTLE=10 LOAD_WINDOW=45`, `LOAD_VIEWERS` at 400 and 800, one step each |
| Profiler | `perf record -F 999 -g` on the server process for 20 seconds, from 13 seconds after every viewer joined |

perf on this machine records user space only (`perf_event_paranoid` 2), so
kernel time comes from `/proc/<pid>/task/*/stat` over the same 20 seconds.

## Headline

| | 400 viewers | 800 viewers, saturated |
| --- | --- | --- |
| Server CPU | 0.78 cores | 0.91 cores |
| Kernel share of it | 30% | 30% |
| This server's own code, self time | 3.1% of user time | 3.9% of user time |

## Results

```text
viewers in one room  layers=l:300,m:800,h:2500 settle=10s window=45s shards=1
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   Mbps  save% wait_p99 fwd_p50 fwd_p99
   400    400      0    0.93    0.78     628     8192     940     919     12     14    8906    8906       0   45178  401.0    0.0     8192     512    8192

viewers in one room  layers=l:300,m:800,h:2500 settle=10s window=45s shards=1
  step   live  refus gen_cor srv_cor  laps/s late_p99 room_us tick_us fan_us rd/lap    in/s    rx/s  drop/s    tx/s   Mbps  save% wait_p99 fwd_p50 fwd_p99
   800    800      0    1.43    0.91      69    32768    9447    9149    274    253   17339   17339       0   53193  317.5    0.0    16384   16384   32768
```

CPU by thread over the profile window, in cores:

| Thread | 400 user | 400 kernel | 800 user | 800 kernel |
| --- | --- | --- | --- | --- |
| Media loop | 0.54 | 0.21 | 0.63 | 0.23 |
| Mux | 0.01 | 0.03 | 0.01 | 0.04 |
| Total | 0.551 | 0.234 | 0.646 | 0.274 |

The loop thread calls `send_to` once per packet, 45,178 and 53,193 times a
second, which is 4.6 and 4.3 us of kernel time per packet sent.

Share of user time spent in each function and everything it calls:

| Function | 400 | 800 |
| --- | --- | --- |
| `run`, the whole loop | 97.1% | 95.9% |
| `Rtc::poll_output`, called from `Peer::poll` | 50.9% | 44.9% |
| `Session::poll_datagram`, inside `poll_output` | 22.5% | 25.2% |
| `RtcSctp::poll`, inside `poll_output` | 10.2% | 6.6% |
| `Rtc::handle_input` | 38.0% | 41.8% |
| `Input::Timeout`, given to every client every lap | 27.6% | 24.9% |
| `Media::do_payload`, packetizing frames inside the timeout | 4.0% | 7.6% |
| Received datagrams, the rest of `handle_input` | 10.5% | 16.9% |
| `Session::handle_rtcp_receive`, mostly TWCC feedback | 9.5% | 15.9% |
| `TwccSendRegister::apply_report`, inside it | 4.8% | 8.6% |
| `SrtpContext::protect_rtp` | 5.8% | 4.3% |
| `Viewer::handle_media_datas`, writing frames | 0.9% | 2.3% |

## Analysis

**The 2026-09-18 reading was wrong.** It put this server's own code at 1.5
percent and concluded the cost was str0m's, with no place to attack. This run
finds the same small self time, 3 to 4 percent, but two things were missed.

- It counted self time. str0m runs only when the loop calls it, so the time
  under `poll_output` and `handle_input` follows how often and in what way this
  server calls them. Counted that way, nearly all user time is entered from
  this server's calls.
- It saw user space only. Kernel time is 30 percent of the server's CPU at both
  loads, almost all of it on the loop thread.

Sharding still holds. No change inside one loop gets it past one core, and four
loops reached four times the viewers. What the reading missed is that one loop
still has work worth doing.

## Rows that cannot be used

The load rows are not comparable with earlier baselines. The build carries
frame pointers, and each run is one step with a longer settle. They are here to
say what load the profile was taken under.

## Follow-up

1. Lower what one loop spends per viewer, #83.
