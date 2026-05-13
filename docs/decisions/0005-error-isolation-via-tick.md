# 0005. Error isolation via `Client::tick()`

## Status

Accepted (2026-04-28)

## Context

The SFU loop iterates clients, calling `renegotiate` and `poll_output` on each. Both can fail. Originally errors propagated via `?` from inside the iteration — a single failing client killed the entire SFU loop, taking down every active stream.

## Decision

`Client::tick()` wraps `renegotiate()` + `poll_output()`. The caller matches on its result: `Err` disconnects that one client and continues the loop.

## Alternatives considered

- **Explicit try/catch at every call site.** Verbose, easy to miss a site.
- **Per-client threads/tasks.** Fights `str0m`'s single-thread-per-`Rtc` model.

## Consequences

**Gain**: one client cannot bring down the SFU; one error-handling pattern at the call site.

**Lose**: `renegotiate` and `poll_output` are coupled by shared return type. Acceptable — they're always called together.
