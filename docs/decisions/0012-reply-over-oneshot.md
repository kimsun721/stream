# 0012. Reply to HTTP handlers over `tokio::sync::oneshot`

## Status

Accepted (2026-10-08). Supersedes [ADR 0007](0007-defer-sync-mpsc-recv-blocking.md).

## Context

[ADR 0007](0007-defer-sync-mpsc-recv-blocking.md) left HTTP handlers blocking on `std::sync::mpsc::recv()` for a media loop's reply. It rejected `tokio::sync` channels because they would "couple the std-thread SFU to a tokio runtime or require bridging".

That premise was wrong when it was written. The types in `tokio::sync` are runtime agnostic, and `oneshot::Sender::send` is a plain function that a std thread can call. Nothing changed since to make it possible. The alternative was misjudged.

The deferral has a cost that is listed among the README's limits: a handler holds a tokio worker for as long as a loop takes to answer.

## Decision

- A reply travels over `tokio::sync::oneshot`. The media loop sends with a plain call and the handler awaits.
- Stream key resolution asks every loop, so its reply uses `tokio::sync::mpsc` with one sender per loop. A loop sends with `try_send`, which needs no runtime either, and the channel holds one reply per loop, so it never fills.
- The request channel stays `std::sync::mpsc::sync_channel`. A loop waits on it until str0m's next deadline, and tokio's `mpsc` has no blocking receive with a timeout outside a runtime. The mux feeds the same channel.
- A handler sends a request with `try_send` and answers `503` when the loop's channel is full. The channel also carries media from the mux, and media that waits reaches viewers late or not at all, while a refused request costs the client a retry.
- Stream key resolution answers `503` if any loop's channel is full, not `401`, since the key may belong to that loop.

## Alternatives considered

- **`spawn_blocking` around the wait.** Frees the worker but parks a thread per waiting request.
- **Move the request channel to tokio as well.** No blocking receive with a timeout.
- **Keep deferring.** The reason given for deferring does not hold.
- **Blocking send, or an unbounded request channel.** Either keeps every request at the cost of media. A handler blocked on a full channel takes the next free slot ahead of the mux, and an unbounded channel queues datagrams behind requests without limit.

## Consequences

**Gain**: a handler no longer holds a worker while it waits or while a loop is full; a handler can put a timeout on a reply; one limit leaves the README.

**Lose**: `SfuMessage` now names tokio types, so the loop's message type depends on the tokio crate, though not on a runtime. A request is refused while its loop's channel is full, and the client has to retry.
