# 0001. Actor pattern — single-thread ownership of `Rooms`

## Status

Accepted (2026-05-11)

## Context

`Rooms` was `Arc<Mutex<HashMap<u64, Room>>>`, shared between the SFU thread and HTTP handlers.

The SFU thread is a timeout-driven polling loop that iterates every room and every client on each tick — it holds the lock effectively 100% of the time. HTTP handlers were starved. The pattern also fights `str0m`, which is sans-IO and designed for single-thread ownership per `Rtc`.

## Decision

The SFU thread owns `Rooms` directly. No `Arc`, no `Mutex`. HTTP handlers send messages over `mpsc::sync_channel<SfuMessage>`; all reads and writes happen inside the SFU thread.

`Rooms` is a newtype struct with handler methods (`create`, `delete`, `update_state`, `get_views`, `register_client`). The dispatcher in `run()` is one method call per match arm.

## Alternatives considered

- **`RwLock`.** Most SFU iterations need write access. No help.
- **Async actor on tokio.** Couples the SFU to a runtime it doesn't otherwise need.
- **Per-room threads.** Doesn't address HTTP/SFU contention.

## Consequences

**Gain**: unambiguous ownership; no lock contention; exhaustive `SfuMessage` matching (catch-all `_ => {}` removed) catches missing handlers at compile time.

**Lose**: HTTP handlers now block on `mpsc::recv()` for replies (see [ADR 0007](0007-defer-sync-mpsc-recv-blocking.md)). External read-only observers (e.g. metrics scrapers) must also go through messages.
