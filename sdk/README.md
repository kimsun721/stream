# @kimsun721/stream-viewer

The browser side of [stream](https://github.com/kimsun721/stream), a WebRTC SFU for live streaming. It joins a room, plays it, follows the simulcast layers the server offers, and reports link quality, so a page does not reimplement the viewer protocol.

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
| `rtcConfiguration` | | Passed to `RTCPeerConnection` |

| Event | Payload |
| --- | --- |
| `state` | `idle`, `connecting`, `connected`, `reconnecting`, `not-live`, `failed` or `closed`, with a `reason` |
| `layers` | The video track's layers, the one being sent, a pending target and the mode |
| `layerchange` | A switch, with how long it took when it answers a request |
| `stats` | Picture size, frame rate, bitrate, round trip, freezes, loss |
| `log` | Debug mode: switches, freezes, RTP timestamp jumps, connection changes |
| `error` | An error, retried or not |

`connect()` resolves once connected. It rejects with `JoinError` for a refused offer that is not worth retrying, and when `close()` is called first.

## Notes

- A layer change takes effect on the next keyframe of the new layer. The server asks the publisher for one once, so a publisher has to send keyframes on an interval of its own, as OBS does.
- Browsers start video muted to allow autoplay. The player has a button for sound; with the core, unmute the element from a user gesture.
- Relaying to other viewers is not supported yet. The server never promotes a viewer that does not answer the relay protocol.
- The server serves the same code as one file at `/stream-viewer.js`, which its own `/watch` page uses.

## Developing

```sh
npm ci
npm run check    # typecheck, unit tests, build
```

`npm run build` also rewrites `../static/stream-viewer.js`. Commit it with the sources; CI fails when it drifts.
