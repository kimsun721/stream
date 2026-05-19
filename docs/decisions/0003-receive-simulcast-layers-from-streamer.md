# 0003. Receive multiple simulcast layers from streamer

## Status

Accepted (2026-04, planning)

## Context

Viewers have varying network conditions and need different qualities. Two paths:

- **A. Server-side transcoding.** Streamer uploads top quality. Server re-encodes per tier.
- **B. Simulcast.** Streamer uploads N independent encodings. Server picks one per viewer.

## Decision

Approach B. The SFU receives multiple layers; per-viewer logic chooses which to forward.

## Alternatives considered

- **A.** Server-side transcoding adds CPU per viewer-quality and inserts an encoding step that adds latency. Both conflict with the SFU's purpose.

## Consequences

**Gain**: latency stays comparable to single-stream WebRTC; server CPU stays bounded; plays to `str0m`'s native simulcast support (SDP, per-packet `rid`, per-rid keyframe requests).

**Lose**: streamer uploads more total bandwidth; the frontend must explicitly configure simulcast (`RTCRtpEncodingParameters`); layer switching with keyframe coordination becomes a real subsystem.

Implementation plan: [feature/simulcast.md](../feature/simulcast.md).
