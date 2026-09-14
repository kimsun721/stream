# Baseline: <what this run measures>

Date: <YYYY-MM-DD>
Commit: <short sha> on <branch>
Previous: <link to the last baseline, or "none">

## Why this run

<What changed since the previous baseline, or why a first measurement was taken.
One paragraph.>

## Environment

Figures are only comparable against a run from the same machine, so the whole
of it goes here.

| | |
| --- | --- |
| CPU | |
| Cores | |
| Kernel | |
| `net.core.rmem_default` | |
| rustc | |
| Profile | release |
| Transport | loopback |
| Settings | defaults, or the `LOAD_*` variables that were set |

## Headline

| | Viewers | Evidence |
| --- | --- | --- |
| Healthy | | No drops, `late_p99` within a bucket of its idle floor |
| Saturated | | `srv_cor` stops rising |
| Losing | | `drop/s` leaves zero |

Cost per viewer at the healthy point: <`room_us / live`>

Relay saving, measured where `drop/s` is zero: <`save%`>

## Results

<Paste every table verbatim. Summarizing here loses the rows a later question
will need.>

## Analysis

<What the numbers say. Where the time goes, what the binding constraint is, what
would move it.>

## Rows that cannot be used

<Any row whose figures are distorted, and what distorts them. `save%` under
packet loss is the usual one.>

## Follow-up

<What this run says should be worked on, in order.>
