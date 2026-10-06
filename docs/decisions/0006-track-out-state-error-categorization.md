# 0006. Categorize errors instead of rolling back `TrackOut` state

## Status

Accepted (2026-04-29)

## Context

`TrackOut`'s state machine is `ToOpen → Negotiating(mid) → Open(mid)`. The original renegotiation flow flipped state to `Negotiating` **before** sending the SDP offer over the DataChannel. If the DC write failed, state was stuck in `Negotiating` with no rollback, so subsequent retries skipped the track, deadlocking it.

## Decision

No rollback. Split error sources into two categories:

- **Transient / timing** (DC not yet open, prior offer still pending): validate up front, return early **without mutating state**. The next tick retries naturally.
- **Hard** (`change.apply`, `serde`, DC `write` once the channel is open): bubble up via `?`. The per-client error handler ([ADR 0005](0005-error-isolation-via-tick.md)) disconnects the client; the state goes away with it.

State only mutates on the success path.

## Alternatives considered

- **Explicit rollback per failure point.** Hard to keep consistent; easy to miss when adding new failure sources.
- **Mutate state after `channel.write` succeeds.** Opens a race window between the write and the mutation.

## Consequences

**Gain**: no rollback bookkeeping; new failure points have a clear taxonomy (transient → retry, hard → tear down).

**Lose**: a hard error tears down the entire client even if the failure was scoped to one track. Categorization is convention-based, not type-enforced.
