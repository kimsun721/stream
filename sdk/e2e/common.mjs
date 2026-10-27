// Helpers shared by the end to end scenarios. Each scenario is its own
// process, started by run.mjs against a server it started.
import { chromium } from "playwright-core";

export const sdp = process.env.E2E_SDP;
export const control = process.env.E2E_CONTROL;
const auth = { Authorization: `Bearer ${process.env.E2E_API_KEY}` };

const results = [];

export function check(name, ok, detail = "") {
  results.push({ name, ok });
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? `  (${detail})` : ""}`);
}

/** Prints the tally and exits with its verdict. */
export function finish() {
  const failed = results.filter((r) => !r.ok).length;
  console.log(`\n${results.length - failed}/${results.length} passed`);
  process.exit(failed ? 1 : 0);
}

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Polls `fn` until it returns something truthy, or null after `ms`. */
export async function until(fn, ms, step = 200) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const value = await fn();
    if (value) return value;
    await sleep(step);
  }
  return null;
}

export async function api(method, path, body) {
  const response = await fetch(`${control}${path}`, {
    method,
    headers: { ...auth, ...(body ? { "Content-Type": "application/json" } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await response.text();
  return text ? JSON.parse(text) : null;
}

export const metrics = async () => (await fetch(`${control}/metrics.json`, { headers: auth })).json();

export function launch() {
  return chromium.launch({
    // Playwright's own browsers are not installed with playwright-core, so
    // this takes Chrome from its usual place unless told otherwise.
    ...(process.env.CHROME_PATH ? { executablePath: process.env.CHROME_PATH } : { channel: "chrome" }),
    headless: true,
    args: ["--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream", "--autoplay-policy=no-user-gesture-required"],
  });
}

/**
 * Publishes the fake camera over WHIP in three simulcast layers. Chrome as a
 * publisher answers keyframe requests but sends no keyframes on a schedule of
 * its own, unlike OBS.
 */
export async function publish(page, streamKey) {
  await page.goto(`${sdp}/watch`);
  return page.evaluate(
    async ({ sdp, streamKey }) => {
      const media = await navigator.mediaDevices.getUserMedia({ video: { width: 1280, height: 720, frameRate: 30 } });
      const pc = new RTCPeerConnection();
      pc.addTransceiver(media.getVideoTracks()[0], {
        direction: "sendonly",
        sendEncodings: [
          { rid: "l", scaleResolutionDownBy: 4, maxBitrate: 300_000 },
          { rid: "m", scaleResolutionDownBy: 2, maxBitrate: 800_000 },
          { rid: "h", maxBitrate: 2_500_000 },
        ],
      });
      await pc.setLocalDescription(await pc.createOffer());
      await new Promise((resolve) => {
        if (pc.iceGatheringState === "complete") return resolve();
        pc.onicegatheringstatechange = () => pc.iceGatheringState === "complete" && resolve();
        setTimeout(resolve, 2000);
      });
      const response = await fetch(`${sdp}/whip`, {
        method: "POST",
        headers: { "Content-Type": "application/sdp", Authorization: `Bearer ${streamKey}` },
        body: pc.localDescription.sdp,
      });
      if (!response.ok) return `whip ${response.status}`;
      await pc.setRemoteDescription({ type: "answer", sdp: await response.text() });
      window.publisher = pc;
      return "ok";
    },
    { sdp, streamKey },
  );
}

/** A room with a publisher on it, live. */
export async function liveRoom(browser) {
  const room = await api("POST", "/rooms");
  const publisher = await browser.newPage();
  check("publisher connects over WHIP", (await publish(publisher, room.stream_key)) === "ok");
  await until(async () => (await api("GET", `/rooms/${room.room_id}`))?.state === "Preview", 10_000);
  await api("PATCH", `/rooms/${room.room_id}`, { state: "Live" });
  return { room, publisher };
}

/** Whether the page's <video> moved on over two seconds, and at what size. */
export async function advancing(page) {
  const read = () =>
    page.evaluate(() => {
      const v = document.querySelector("video");
      return { t: v.currentTime, w: v.videoWidth, h: v.videoHeight };
    });
  const a = await read();
  await sleep(2000);
  const b = await read();
  return b.w > 0 && b.t - a.t > 1 ? `${b.w}x${b.h}, +${(b.t - a.t).toFixed(1)} s` : null;
}

/** Collects console errors, leaving out the 404s of offers to a room that is not live. */
export function collectErrors(page, name, into) {
  page.on("console", (m) => m.type() === "error" && !m.text().startsWith("Failed to load resource") && into.push(`${name}: ${m.text()}`));
  page.on("pageerror", (e) => into.push(`${name}: ${e}`));
}
