# 0011. Room lifecycle and explicit go-live

## Status

Proposed

## Context

Rooms were built around a single broadcast, and the flow around them was never designed. It
shows: `Room::add_client` sets no state for a streamer, so a connected encoder leaves the room
Idle and viewers get a 404. `streamer_id` is written once and never cleared or read, so it points
at a removed client as soon as the streamer disconnects. Nothing returns a room to a reusable
state, and nothing deletes it either.

[ADR 0010](0010-stream-key-authentication.md) puts the stream key on the room and shows it once,
so the room now has to outlive the broadcast it hosts. That forces the lifecycle to be explicit.

## Decision

Three states, with the room persisting across all of them:

- **Idle.** The room exists and holds a stream key. No streamer, no viewers. Skipped by most of
  the tick loop.
- **Preview.** A validated WHIP session is ingesting. Media arrives but is not served, so the
  streamer can check the feed before anyone sees it.
- **Live.** Viewers may join and are served.

Transitions:

- A validated WHIP session moves Idle to Preview.
- Preview to Live is an explicit control plane call. Connecting an encoder never publishes on its
  own.
- Ending the stream evicts viewers, clears `streamer_id`, removes the streamer, and returns the
  room to Idle. The stream key survives.

A room is removed only by an explicit delete, never by a broadcast ending.

## Alternatives considered

- **Go Live as soon as the encoder connects.** Publishes by accident, and the streamer has no
  moment to check the feed. YouTube splits ingest from going live for the same reason.
- **Delete the room when the broadcast ends.** Discards the stream key with it, so the streamer
  reconfigures the encoder before every broadcast.
- **Leave viewers attached across a broadcast boundary.** They hold a room with no streamer and
  no tracks, and the next broadcast inherits stale state.

## Consequences

**Gain**: connecting an encoder is no longer publishing; `streamer_id` gets a defined clearing
point; Idle rooms cost almost nothing per tick; a room is reusable without reissuing its key.

**Lose**: going live needs a second action outside the encoder, so it cannot be driven from OBS
alone; rooms accumulate until something deletes them.

**Trigger to revisit**: rooms sitting Idle long enough that operators want them reclaimed
automatically.
