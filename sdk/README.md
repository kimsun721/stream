# @kimsun721/stream-viewer

The browser side of [stream](https://github.com/kimsun721/stream), a WebRTC SFU for live streaming. It joins a room, plays it, follows the simulcast layers the server offers, reports link quality, and relays to another viewer when the server pairs it with one, so a page does not reimplement the viewer protocol.

```sh
npm install @kimsun721/stream-viewer
```

## A ready-made player

```js
import { createPlayer } from "@kimsun721/stream-viewer/player";

const player = createPlayer(document.getElementById("player"), {
  room: "RM_0_...",
  server: "https://stream.example.com:8443",
});

// later
player.destroy();
```

The player draws the video, a stats readout, layer buttons, a mute toggle and a debug log. Its colors are custom properties on `.sv-player` (`--sv-bg`, `--sv-panel`, `--sv-ink`, `--sv-accent` and the rest), so a page restyles it without touching the rules.

## The core, with your own UI

```js
import { Viewer } from "@kimsun721/stream-viewer";

const viewer = new Viewer({ room: "RM_0_...", server: "https://stream.example.com:8443" });
viewer.attach(document.querySelector("video"));

viewer.on("state", ({ state, reason }) => console.log(state, reason));
viewer.on("layers", (layers) => console.log(layers.layers, layers.current, layers.mode));
viewer.on("stats", (stats) => console.log(stats.width, stats.mbps));

await viewer.connect();
viewer.setLayer("l");
viewer.setAutoLayer();
viewer.close();
```

| Option | Default | |
| --- | --- | --- |
| `room` | | The room id |
| `server` | the page's origin | Base URL of the server's client API, the HTTPS port |
| `reconnect` | `true` | Join again after a dropped connection, and keep asking while the room is not live |
| `maxRetryDelayMs` | `10000` | Ceiling for the wait between attempts, which doubles from one second |
| `statsIntervalMs` | `1000` | How often stats are read and link quality reported |
| `debug` | `false` | Emit `log` events, also switched with `setDebug` |
| `rtcConfiguration` | | Passed to `RTCPeerConnection`, relay links included |
| `relay` | `true` | Forward the stream to another viewer when the server makes this one a relay |

| Event | Payload |
| --- | --- |
| `state` | `idle`, `connecting`, `connected`, `reconnecting`, `not-live`, `failed` or `closed`, with a `reason` |
| `layers` | The video track's layers, the one being sent, a pending target and the mode |
| `layerchange` | A switch, with how long it took when it answers a request |
| `stats` | Picture size, frame rate, bitrate, round trip, freezes, loss. From the relay link while a leaf watches through it |
| `role` | `relay`, `leaf` or `null`, and whether the peer link is up |
| `log` | Debug mode: switches, freezes, RTP timestamp jumps, connection changes |
| `error` | An error, retried or not |

`connect()` resolves once connected. It rejects with `JoinError` for a refused offer that is not worth retrying, and when `close()` is called first.

## Notes

- A layer change takes effect on the next keyframe of the new layer. The server asks the publisher for one once, so a publisher has to send keyframes on an interval of its own, as OBS does.
- Browsers start video muted to allow autoplay. The player has a button for sound; with the core, unmute the element from a user gesture.
- The server may pair two viewers: the relay forwards what it receives to the leaf over a peer link of their own, and the server stops sending to the leaf. Either side falls back to the server's stream when the link drops or the server demotes the pair. A page needs no code for it, and the player says nothing of it outside debug mode.
- A switch between the server's stream and a relay's keeps the old picture up until the new source has decoded a frame. A leaf whose relay goes silent for a second goes back to the server without waiting for the connection state.
- A browser encodes a forwarded track again. The relay holds its encoder near the bitrate it receives, since the server demotes a relay that sends well under it.
- The SDK does not answer the server's upload probe, which the server is to replace with a measurement of its own. Until then the server promotes no browser to relay, and the pairing above does not happen.
- A leaf watches the relay's layer. Picking a layer has no effect while it does.
- The relay link has no TURN of its own: pass ICE servers in `rtcConfiguration`, or a pair that cannot connect directly stays on the server.
- The server serves the same code as one file at `/stream-viewer.js`, which its own `/watch` page uses.

## Developing

```sh
npm ci
npm run check    # typecheck, unit tests, build
```

`npm run build` also rewrites `../static/stream-viewer.js`. Commit it with the sources; CI fails when it drifts.
