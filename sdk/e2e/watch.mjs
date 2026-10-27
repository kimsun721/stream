// One viewer on /watch: playback, layers, a room going idle and back, and a
// room that is not live.
import { api, check, collectErrors, finish, launch, liveRoom, publish, sdp, sleep, until } from "./common.mjs";

const browser = await launch();
const { room, publisher } = await liveRoom(browser);

const viewer = await browser.newPage();
const errors = [];
collectErrors(viewer, "viewer", errors);
await viewer.goto(`${sdp}/watch?room=${room.room_id}&debug`);

const stats = () => viewer.textContent(".sv-stats");
const status = () => viewer.textContent(".sv-status");

const playing = await until(
  () =>
    viewer.evaluate(() => {
      const v = document.querySelector("video");
      return v && v.videoWidth > 0 && v.currentTime > 0.5 ? `${v.videoWidth}x${v.videoHeight}` : null;
    }),
  20_000,
);
check("the watch page plays the stream", !!playing, playing ?? "no picture");
check("status says watching", (await status())?.startsWith("Watching"), await status());

const layerButtons = await until(async () => {
  const texts = await viewer.$$eval(".sv-layers button", (bs) => bs.map((b) => b.textContent));
  return texts.length === 4 ? texts : null;
}, 10_000);
check("Auto and three layers are offered", !!layerButtons, JSON.stringify(layerButtons));

// A keyframe request for a layer is debounced for a second on the server, so
// wait past it.
await sleep(2000);
const current = (await stats()).match(/layer (\w+)/)?.[1];
const pick = current === "h" ? "m" : "h";
await viewer.click(`.sv-layers button:nth-child(${pick === "m" ? 3 : 4})`);
const manual = await until(async () => {
  const s = await stats();
  return s?.includes(`layer ${pick}, manual`) && !s.includes("waiting") ? s : null;
}, 15_000);
check("picking a layer switches to it on a keyframe", !!manual, (manual ?? (await stats())).replace(/\n/g, " | "));

await viewer.click(".sv-layers button:nth-child(1)");
const auto = await until(async () => ((await stats())?.includes(", auto") ? true : null), 10_000);
check("Auto hands the choice back to the server", !!auto);

await sleep(3000);
const log = await viewer.textContent(".sv-log");
const switched = log?.match(/switched \w+ to \w, \d+ ms after asking/)?.[0];
check("debug log records the switch with its wait", !!switched, switched);
check("a lone viewer is not paired", !(await stats()).includes("relay"));

// The room goes idle: the viewer loses its stream and waits, then the room
// comes back and the viewer resumes without a reload.
await api("PATCH", `/rooms/${room.room_id}`, { state: "Idle" });
const waiting = await until(async () => ((await status())?.includes("not live") ? await status() : null), 30_000);
check("an idle room puts the viewer in not-live and retrying", !!waiting, waiting ?? (await status()));

await publisher.evaluate(() => window.publisher.close());
check("publisher reconnects", (await publish(publisher, room.stream_key)) === "ok");
await until(async () => (await api("GET", `/rooms/${room.room_id}`))?.state === "Preview", 10_000);
await api("PATCH", `/rooms/${room.room_id}`, { state: "Live" });
const resumed = await until(
  () =>
    viewer.evaluate(() => {
      const v = document.querySelector("video");
      const s = document.querySelector(".sv-status")?.textContent ?? "";
      return s.startsWith("Watching") && v && v.videoWidth > 0 ? s : null;
    }),
  30_000,
);
check("the viewer resumes once the room is live again", !!resumed, resumed ?? (await status()));

const fresh = await browser.newPage();
await fresh.goto(`${sdp}/watch`);
const unknownRoom = await fresh.evaluate(async () => {
  const { Viewer } = await import("/stream-viewer.js");
  const v = new Viewer({ room: "RM_0_nosuchroomxx", reconnect: false });
  try {
    await v.connect();
    return "connected";
  } catch (e) {
    return `${v.state} ${e.status}`;
  }
});
check("without reconnect, a room that is not live rejects connect", unknownRoom === "failed 404", unknownRoom);

check("no errors on the watch page console", errors.length === 0, errors.slice(0, 3).join(" / "));

await browser.close();
await api("DELETE", `/rooms/${room.room_id}`);
finish();
