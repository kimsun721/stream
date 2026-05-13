# 0004. Broadcast keyframe to all viewers, with 1s debounce

## Status

Accepted (2026-04-21)

## Context

A viewer's PLI/FIR arrives as `Event::KeyframeRequest` on **that viewer's** `Rtc`. The server forwards it to the streamer. The streamer's keyframe is then forwarded to every viewer in the room — there is no per-viewer keyframe in the SFU forwarding model.

Identifying the originator to deliver only to them would require per-viewer in-flight state and outbound RTP gating. Significant complexity for marginal benefit.

## Decision

Don't track the originator. Forward the keyframe request to the streamer; the resulting media data fans out to everyone as usual.

Throttle to **one outbound keyframe request per `(track, kind)` per 1 second**, tracked via `TrackInEntry.last_keyframe_request`.

## Alternatives considered

- **Per-viewer keyframe gating.** Complex; doesn't pay off at low room sizes.
- **No throttle.** A single misbehaving viewer triggers keyframe storms.

## Consequences

**Gain**: forwarding stays simple; the throttle caps worst-case load.

**Lose**: viewers receive keyframes they didn't ask for; the 1-second window can collapse two legitimate distinct keyframe needs.

## Notes

With simulcast, this should narrow to per `(track, kind, rid)` — only viewers consuming that rid need the keyframe.
