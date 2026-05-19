# Simulcast

Per-viewer video quality without server-side transcoding. Background: [ADR 0003](../decisions/0003-receive-simulcast-layers-from-streamer.md).

v1 ships **manual selection only**. BWE deferred until measurement data exists.

## Phases

- **A. Manual layer selection.** Streamer uploads multiple rids; server forwards a chosen rid per viewer; viewer requests layer changes over DataChannel; server coordinates keyframes on switch.
- **B. BWE measurement (observe).** Enable BWE; log per-client bitrate estimates. No behavior change.
- **C. Auto-switching with manual override.** Layer policy with up/down hysteresis. `LayerMode { Auto, Manual }` per viewer-track. Auto switches based on BWE; manual overrides until released.

## Notes

- Server → viewer is single-stream. No SDP change on viewer side.
- `KeyframeRequest` from viewer has `rid = None`. Server substitutes viewer's chosen rid before forwarding. Reference: str0m `examples/chat.rs`.
- DC messages use `{ "type": ..., ... }` envelope (shared with P2P relay).
