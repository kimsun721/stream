# 0007. Defer `sync_channel::recv` blocking issue

## Status

Accepted (2026-05-11)

## Context

After [ADR 0001](0001-actor-pattern.md), HTTP handlers wait for SFU replies on `mpsc::sync_channel<Option<T>>` via `recv()`, a **blocking** call, inside an `async fn`.

Two latency problems:

1. **Wall-clock.** If a message arrives mid-iteration, it waits for the SFU loop to return to message processing, in the worst case about one full SFU tick.
2. **Tokio worker.** `std::sync::mpsc::recv()` blocks the OS thread, not just the task. Enough concurrent waiting handlers can starve unrelated requests.

## Decision

Accept and defer. Revisit after simulcast ships, with measurement.

Rationale: simulcast doesn't depend on this; behavior is correct (just slow under load); picking the right fix requires data we don't have yet.

## Alternatives considered

- **`tokio::sync::mpsc` + `oneshot` replies.** Async-native; cleanest long-term, but couples the std-thread SFU to a tokio runtime or requires bridging.
- **Read-only snapshot of `Rooms` for HTTP.** Re-introduces shared state.
- **`spawn_blocking` wrapper.** Fixes tokio-worker blocking only. Cheap stopgap.

## Consequences

**Gain**: simpler code; focus stays on simulcast/P2P relay.

**Lose**: HTTP latency varies with SFU load; tokio worker pool can stall.

## Trigger to revisit

- Measured p99 HTTP latency exceeds target under realistic load.
- Observed tokio worker starvation.
- New feature that depends on fast round trips from HTTP to the SFU.
