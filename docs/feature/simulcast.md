# Simulcast

Per-viewer video quality without server-side transcoding. Background: [ADR 0003](../decisions/0003-receive-simulcast-layers-from-streamer.md).

All three phases are shipped. A viewer starts in auto mode and can switch to manual.

## Phases

- **A. Manual layer selection.** Streamer uploads multiple rids; server forwards a chosen rid per viewer; viewer requests layer changes over DataChannel; server coordinates keyframes on switch.
- **B. BWE measurement (observe).** Enable BWE; log per-client bitrate estimates. No behavior change.
- **C. Auto-switching with manual override.** Layer policy with up/down hysteresis. `LayerMode { Auto, Manual }` per viewer-track. Auto switches based on BWE; manual overrides until released.

## Layer selection

- A new viewer starts on the cheapest measured layer, in auto mode.
- Auto takes the highest layer whose measured bitrate plus `headroom_margin_percent` fits the viewer's bandwidth estimate, or the cheapest when none fits. Moving up needs `upswitch_margin_percent` on top, which keeps layers from flapping. Moving down does not.
- `set_layer` is ignored in auto mode. A viewer sends `set_layer_mode` with `manual` first.

## Switching

A viewer-track holds the layer being sent and a target. A request sets the target and asks the streamer for a keyframe of it, once. The current layer keeps flowing until a keyframe of the target arrives, and that keyframe is the first frame sent from the new layer. A new viewer starts the same way, so nothing is sent before a keyframe.

The server does not ask again. The publisher has to send keyframes on an interval of its own, as OBS does.

## DataChannel messages

| Type | From → To | Purpose |
|---|---|---|
| `layer_status` | server → viewer | A track's layers with their measured bitrate, plus `current_layer`, `target_layer` and `layer_mode`. Sent once the track is negotiated |
| `layer_changed` | server → viewer | The switch happened |
| `set_layer` | viewer → server | Ask for a layer. Manual mode only |
| `set_layer_mode` | viewer → server | `auto` or `manual` |

## Notes

- Server → viewer is single-stream. No SDP change on viewer side.
- `KeyframeRequest` from viewer has `rid = None`. Server substitutes the viewer's target rid, or the current one when no switch is pending, before forwarding. Reference: str0m `examples/chat.rs`.
- DC messages use `{ "type": ..., ... }` envelope (shared with P2P relay).
