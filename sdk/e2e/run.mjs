// Starts a server from a release build with a config of its own, runs every
// scenario against it in headless Chrome, and stops it.
//
//   cargo build --release && cd sdk && npm run e2e [-- relay]
//
// STREAM_BIN points at another server binary, CHROME_PATH at another Chrome.
import { spawn } from "node:child_process";
import { createWriteStream } from "node:fs";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const bin = resolve(process.env.STREAM_BIN ?? join(here, "../../target/release/stream"));
const scenarios = process.argv.length > 2 ? process.argv.slice(2) : ["watch", "relay"];

const ports = { sdp: 18443, control: 18080, media: 41000 };
const apiKey = "e2e-api-key-0123456789";

// Only the gates that would put a pairing minutes away are scaled down, as in
// the Rust integration tests.
const config = `[server]
public_ip = "127.0.0.1"
media_port = ${ports.media}
https_sdp_server_port = ${ports.sdp}
http_rest_server_port = ${ports.control}
total_shards = 2

[bitrate]
estimate_secs = 1

[perf]
min_samples_len = 5

[relay]
min_connection_age_secs = 2
`;

const dir = await mkdtemp(join(tmpdir(), "stream-e2e-"));
await writeFile(join(dir, "config.toml"), config);
const logPath = join(dir, "server.log");
const log = createWriteStream(logPath);

const server = spawn(bin, [], { cwd: dir, env: { ...process.env, API_KEY: apiKey, RUST_LOG: "info" } });
server.stdout.pipe(log);
server.stderr.pipe(log);
let exited = false;
server.on("exit", () => (exited = true));
server.on("error", (error) => {
  console.error(`could not start ${bin}: ${error.message}`);
  process.exit(1);
});

const control = `http://127.0.0.1:${ports.control}`;
const deadline = Date.now() + 20_000;
for (;;) {
  try {
    const response = await fetch(`${control}/metrics.json`, { headers: { Authorization: `Bearer ${apiKey}` } });
    if (response.ok) break;
  } catch {}
  if (exited || Date.now() > deadline) {
    console.error(`the server did not come up, see ${logPath}`);
    server.kill();
    process.exit(1);
  }
  await new Promise((r) => setTimeout(r, 200));
}

const env = { ...process.env, E2E_SDP: `http://127.0.0.1:${ports.sdp}`, E2E_CONTROL: control, E2E_API_KEY: apiKey };
let failed = 0;

for (const name of scenarios) {
  console.log(`\n== ${name}`);
  const code = await new Promise((done) => {
    spawn(process.execPath, [join(here, `${name}.mjs`)], { env, stdio: "inherit" }).on("exit", done);
  });
  if (code !== 0) failed += 1;
}

server.kill();
if (failed) console.log(`\nserver log: ${logPath}`);
process.exit(failed ? 1 : 0);
