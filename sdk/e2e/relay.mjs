// Two viewers on /watch, one promoted to relay for the other: pairing, the
// handover both ways, the relay's reports, and a relay that leaves.
import { advancing, api, check, collectErrors, finish, launch, liveRoom, metrics, sdp, sleep, until } from "./common.mjs";

// The SDK leaves the upload probe unanswered until the server measures upload
// itself, so the test answers it for the page meant to relay, and records what
// the page sends.
const answerProbe = () => {
  const create = RTCPeerConnection.prototype.createDataChannel;
  window.sent = [];
  RTCPeerConnection.prototype.createDataChannel = function (...args) {
    const channel = create.apply(this, args);
    const send = channel.send.bind(channel);
    channel.send = (data) => {
      window.sent.push(JSON.parse(data));
      send(data);
    };
    channel.addEventListener("message", (event) => {
      if (JSON.parse(event.data).type === "probe_available_upload") {
        send(JSON.stringify({ type: "available_upload", available_upload_kbps: 20_000 }));
      }
    });
    return channel;
  };
};

// Records when the page's <video> draws each frame, so a test can tell how
// long the picture stood still.
const recordFrames = () => {
  window.drawn = [];
  const watch = () => {
    const video = document.querySelector("video");
    if (!video) return setTimeout(watch, 100);
    const step = (now) => {
      window.drawn.push(now);
      video.requestVideoFrameCallback(step);
    };
    video.requestVideoFrameCallback(step);
  };
  watch();
};

/**
 * The longest the picture stood still within [from, to] on the page clock,
 * counted from the last frame drawn before `from`.
 */
const longestGap = (page, from, to) =>
  page.evaluate(
    ({ from, to }) => {
      const first = window.drawn.findLastIndex((t) => t < from);
      const times = window.drawn.slice(Math.max(first, 0)).filter((t) => t <= to);
      let gap = 0;
      for (let i = 1; i < times.length; i++) gap = Math.max(gap, times[i] - times[i - 1]);
      return Math.round(gap);
    },
    { from, to },
  );
const now = (page) => page.evaluate(() => performance.now());

const browser = await launch();
const { room } = await liveRoom(browser);
const errors = [];

async function watch(name, probe) {
  const page = await browser.newPage();
  collectErrors(page, name, errors);
  if (probe) await page.addInitScript(answerProbe);
  await page.addInitScript(recordFrames);
  await page.goto(`${sdp}/watch?room=${room.room_id}&debug`);
  return page;
}
const stats = (page) => page.textContent(".sv-stats");
const logOf = (page) => page.textContent(".sv-log");

const relay = await watch("relay", true);
// The server's own pick moves a viewer between layers as its estimate moves,
// and the headless publisher's layers come and go. A relay changing layer
// mid pairing is its own case, so the relay here holds one.
await until(async () => ((await relay.$$(".sv-layers button")).length === 4 ? true : null), 15_000);
await sleep(2000);
await relay.click(".sv-layers button:nth-child(3)");
await until(async () => ((await stats(relay)).includes("layer m, manual") ? true : null), 15_000);

const leaf = await watch("leaf", false);
await until(() => advancing(leaf), 15_000, 0);
const pairStart = await now(leaf);

const paired = await until(async () => {
  const [r, l] = [await stats(relay), await stats(leaf)];
  return r.includes("relaying to a viewer") && l.includes("watching through a relay") ? true : null;
}, 45_000);
check(
  "the server pairs the two and the link comes up",
  !!paired,
  paired ? "" : `${await stats(relay)} | ${await stats(leaf)}`.replace(/\n/g, " "),
);
await sleep(2000);
const takeover = await longestGap(leaf, pairStart, await now(leaf));
check("the leaf's picture does not stall when the relay takes over", takeover < 300, `longest gap ${takeover} ms`);

const leafPlays = await until(() => advancing(leaf), 10_000, 0);
check("the leaf plays the relay's stream", !!leafPlays, leafPlays ?? "stalled");
const relayPlays = await advancing(relay);
check("the relay keeps playing its own", !!relayPlays, relayPlays ?? "stalled");

const before = await metrics();
await sleep(10_000);
const after = await metrics();
const still = (await stats(leaf)).includes("watching through a relay");
check(
  "the pair holds for 10 s, the relay's rate passing the server's check",
  still && after.demotions === before.demotions,
  `demotions ${before.demotions} -> ${after.demotions}`,
);
check(
  "the server counts the leaf as served by its relay",
  after.connected_leaves === 1 && after.relays === 1,
  `relays ${after.relays}, connected leaves ${after.connected_leaves}`,
);
const outgoing = (await relay.evaluate(() => window.sent.filter((m) => m.type === "relay_outgoing").map((m) => m.kbps))).slice(-5);
check("the relay reports what it sends", outgoing.length > 0 && outgoing.every((k) => k > 0), `kbps ${outgoing.join(", ")}`);

// Outside debug mode the player says nothing about the relay.
await leaf.click(".sv-debug");
const quiet = await stats(leaf);
check("without debug the leaf's player does not mention the relay", !quiet.includes("relay"), quiet.replace(/\n/g, " | "));
await leaf.click(".sv-debug");

// The relay leaves without a word. The leaf notices from its media stopping.
const leftAt = await now(leaf);
await relay.close();
const noticed = await until(async () => ((await logOf(leaf)).includes("lost the relay") ? await now(leaf) : null), 15_000, 50);
check(
  "the leaf notices its relay is gone within 2 s",
  noticed !== null && noticed - leftAt < 2000,
  noticed ? `${Math.round(noticed - leftAt)} ms` : "not noticed",
);
const leafBack = await until(() => advancing(leaf), 15_000, 0);
check("the leaf plays the server's stream again", !!leafBack, leafBack ?? "stalled");
console.log(`      picture stood still for ${await longestGap(leaf, leftAt, await now(leaf))} ms around the relay leaving`);

check("no errors on either page", errors.length === 0, errors.slice(0, 3).join(" / "));

await browser.close();
await api("DELETE", `/rooms/${room.room_id}`);
finish();
