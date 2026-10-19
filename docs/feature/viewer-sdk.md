# Viewer SDK

A browser package that plays a room, so an integrator does not reimplement the viewer protocol. Not started.

## Why

The viewer protocol is this server's own: an SDP offer to join, renegotiation over the DataChannel, the layer messages, and periodic perf reports. `static/watch.html` speaks all of it, but only inside that page.

## Scope

- Join a room and hand back a stream to attach to a `<video>`.
- Layer status as events, and layer selection, automatic or manual.
- Leave.
- Perf reports, sent without the caller's involvement.

Everything else, the UI included, is the caller's. `watch.html` is rebuilt on the SDK, as its first user and its example.

## Needs

- A protocol version, so a server and an SDK can tell whether they match. There is none today.
- A package name under a scope, since `stream` is taken.
- A build, type definitions and CI for the package.

## Out of scope

- Relay. A browser re-encodes a track it forwards. See the notes in [P2P relay](p2p-relay.md).
- Publishing, which goes over WHIP.

## Notes

- WHEP, on the roadmap, complements it: plain playback in any WHEP player, while the SDK carries layer selection.
- Client SDKs are how SFUs usually ship a protocol of their own: `livekit-client`, `mediasoup-client`, `janus.js`.
- Messages it wraps: [Simulcast](simulcast.md).
