//! Not part of `cargo test`. Run it deliberately, and in release, because a
//! debug build spends most of its time in str0m's crypto and measures nothing
//! about this server:
//!
//! ```text
//! cargo test --release --test load -- --ignored --nocapture
//! ```
//!
//! One server serves the whole binary, so the scenarios run one after another
//! inside a single test rather than as three that a `--test-threads` above one
//! would overlap onto the same server.
//!
//! Every setting comes from the environment, because `cargo test` owns the
//! command line and rejects flags of our own. The defaults are the ones the
//! recorded baseline was measured with, so a plain run stays comparable to it:
//!
//! ```text
//! LOAD_SCENARIOS=viewers,relay,rooms
//! LOAD_VIEWERS=50,100,200,400,500,600,800
//! LOAD_RELAY_VIEWERS=50,100,200,400
//! LOAD_ROOMS=1,4,20
//! LOAD_ROOM_VIEWERS=400
//! LOAD_RELAY_SHARE=2          one viewer in this many advertises relay upload
//! LOAD_LAYERS=l:300,m:800,h:2500
//! LOAD_SETTLE=5
//! LOAD_WINDOW=10
//! LOAD_SHARDS=4               media loops the server runs
//! LOAD_METRICS_PORT=          pins the Prometheus port, for watching in Grafana
//! ```

mod common;

use std::time::{Duration, Instant};

use common::{CreatedRoom, Detached, RelayCapacity, Server, is_connected};
use serde_json::Value;

const CONNECT: Duration = Duration::from_secs(15);
/// A step gives up on stragglers rather than on the whole run, and reports the
/// count the server actually holds.
const ALL_CONNECTED: Duration = Duration::from_secs(60);

/// Above the 13000 kbps default cutoff, so the server will promote the viewer.
const PLENTY_OF_UPLOAD: u32 = 20_000;

#[tokio::test]
#[ignore = "load run"]
async fn baseline() {
    let run = Run::from_env();
    raise_open_file_limit();
    let server = Server::default();
    let client = reqwest::Client::new();

    if run.wants("viewers") {
        viewers_in_one_room(&run, &server, &client).await;
    }

    if run.wants("relay") {
        viewers_with_relay(&run, &server, &client).await;
    }

    if run.wants("rooms") {
        viewers_across_rooms(&run, &server, &client).await;
    }
}

/// Every viewer holds a socket, and a desktop session usually starts a process
/// at 1024 descriptors, which a large step passes. The hard limit is as far as
/// a process may raise itself without privileges, and the server spawned after
/// this inherits it.
fn raise_open_file_limit() {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };

    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 && limit.rlim_cur < limit.rlim_max
        {
            limit.rlim_cur = limit.rlim_max;
            libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
        }
    }
}

/// What the run was told to do. Printed above every table, so a pasted result
/// says what produced it.
struct Run {
    scenarios: Vec<String>,
    viewers: Vec<usize>,
    relay_viewers: Vec<usize>,
    rooms: Vec<usize>,
    room_viewers: usize,
    relay_share: usize,
    layers: Vec<(String, u32)>,
    settle: Duration,
    window: Duration,
    shards: usize,
}

impl Run {
    fn from_env() -> Self {
        let run = Run {
            scenarios: words("LOAD_SCENARIOS", "viewers,relay,rooms"),
            viewers: numbers("LOAD_VIEWERS", "50,100,200,400,500,600,800"),
            relay_viewers: numbers("LOAD_RELAY_VIEWERS", "50,100,200,400"),
            rooms: numbers("LOAD_ROOMS", "1,4,20"),
            room_viewers: numbers("LOAD_ROOM_VIEWERS", "400")[0],
            relay_share: numbers("LOAD_RELAY_SHARE", "2")[0],
            layers: layers("LOAD_LAYERS", "l:300,m:800,h:2500"),
            settle: Duration::from_secs(numbers("LOAD_SETTLE", "5")[0] as u64),
            window: Duration::from_secs(numbers("LOAD_WINDOW", "10")[0] as u64),
            shards: common::shards(),
        };

        // Rooms of different sizes would not be comparable with each other, so
        // an uneven split is a mistake rather than something to round away.
        for rooms in &run.rooms {
            assert!(
                run.room_viewers.is_multiple_of(*rooms),
                "LOAD_ROOM_VIEWERS {} does not divide into {rooms} rooms",
                run.room_viewers
            );
        }

        assert!(run.relay_share > 0, "LOAD_RELAY_SHARE must be at least 1");

        run
    }

    fn wants(&self, scenario: &str) -> bool {
        self.scenarios.iter().any(|s| s == scenario)
    }

    fn layer_rates(&self) -> Vec<(&str, u32)> {
        self.layers
            .iter()
            .map(|(rid, kbps)| (rid.as_str(), *kbps))
            .collect()
    }

    fn rids(&self) -> Vec<&str> {
        self.layers.iter().map(|(rid, _)| rid.as_str()).collect()
    }

    fn describe(&self) -> String {
        let layers: Vec<String> = self
            .layers
            .iter()
            .map(|(rid, kbps)| format!("{rid}:{kbps}"))
            .collect();

        format!(
            "layers={} settle={}s window={}s shards={}",
            layers.join(","),
            self.settle.as_secs(),
            self.window.as_secs(),
            self.shards
        )
    }
}

fn setting(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_string())
}

fn words(name: &str, fallback: &str) -> Vec<String> {
    setting(name, fallback)
        .split(',')
        .map(|word| word.trim().to_string())
        .filter(|word| !word.is_empty())
        .collect()
}

fn numbers(name: &str, fallback: &str) -> Vec<usize> {
    let values: Vec<usize> = words(name, fallback)
        .iter()
        .map(|word| {
            word.parse()
                .unwrap_or_else(|_| panic!("{name} holds {word}, which is not a number"))
        })
        .collect();

    assert!(!values.is_empty(), "{name} is empty");

    values
}

/// `rid:kbps` pairs, in the order the publisher declares them.
fn layers(name: &str, fallback: &str) -> Vec<(String, u32)> {
    words(name, fallback)
        .iter()
        .map(|pair| {
            let (rid, kbps) = pair
                .split_once(':')
                .unwrap_or_else(|| panic!("{name} holds {pair}, which is not rid:kbps"));

            let kbps = kbps
                .parse()
                .unwrap_or_else(|_| panic!("{name} holds {kbps}, which is not a number"));

            (rid.to_string(), kbps)
        })
        .collect()
}

/// What one viewer costs, which every other run is compared against. The
/// generator's own CPU is printed beside the server's: if it is far larger, the
/// run measured this machine rather than the server.
async fn viewers_in_one_room(run: &Run, server: &Server, client: &reqwest::Client) {
    let (publisher, room) = publish(run, server, client).await;

    header(run, "viewers in one room");

    let mut viewers = Viewers::new();
    let mut added = 0;

    for target in run.viewers.clone() {
        viewers
            .add(run, server, client, &room.room_id, target - added, false)
            .await;
        added = target;

        viewers.measure(run, server, client, target).await;
    }

    teardown(server, client, viewers, vec![publisher], &[room]).await;
}

/// The same sweep with every other viewer advertising the upload a relay needs.
/// One relay carries one leaf, so the saving cannot pass half whatever the
/// viewers advertise.
async fn viewers_with_relay(run: &Run, server: &Server, client: &reqwest::Client) {
    let (publisher, room) = publish(run, server, client).await;

    header(run, "viewers with relay");

    let mut viewers = Viewers::new();
    let mut added = 0;

    for target in run.relay_viewers.clone() {
        viewers
            .add(run, server, client, &room.room_id, target - added, true)
            .await;
        added = target;

        viewers.measure(run, server, client, target).await;
    }

    teardown(server, client, viewers, vec![publisher], &[room]).await;
}

/// One viewer count split across more rooms. Room work is a small share of the
/// loop, so this says how much of the cost is the walk itself rather than what
/// each room holds.
async fn viewers_across_rooms(run: &Run, server: &Server, client: &reqwest::Client) {
    header(run, &format!("{} viewers across rooms", run.room_viewers));

    for rooms in run.rooms.clone() {
        let mut publishers: Vec<Detached> = Vec::new();
        let mut created = Vec::new();
        let mut viewers = Viewers::new();

        for _ in 0..rooms {
            let (publisher, room) = publish(run, server, client).await;
            publishers.push(publisher);

            viewers
                .add(
                    run,
                    server,
                    client,
                    &room.room_id,
                    run.room_viewers / rooms,
                    false,
                )
                .await;

            created.push(room);
        }

        viewers.measure(run, server, client, rooms).await;

        teardown(server, client, viewers, publishers, &created).await;
    }
}

/// Deleting a room drops its clients with it. Without that the server carries
/// the last scenario's viewers into the next one for as long as their ICE takes
/// to time out, and measures them too.
async fn teardown(
    server: &Server,
    client: &reqwest::Client,
    viewers: Viewers,
    publishers: Vec<Detached>,
    rooms: &[CreatedRoom],
) {
    drop(viewers);
    drop(publishers);

    for room in rooms {
        server.delete_room(client, &room.room_id).await;
    }

    let give_up_at = Instant::now() + ALL_CONNECTED;

    while Instant::now() < give_up_at {
        if count(&metrics(server, client).await, "clients") == 0 {
            return;
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn publish(run: &Run, server: &Server, client: &reqwest::Client) -> (Detached, CreatedRoom) {
    let room = server.create_room(client).await;

    let mut publisher = server
        .connect_publisher(client, &room.stream_key, &run.rids())
        .await;
    assert!(
        publisher.run_until(CONNECT, is_connected),
        "publisher connect"
    );
    publisher.start_media(&run.layer_rates());

    server.set_live(client, &room.room_id).await;

    (publisher.detach(), room)
}

fn header(run: &Run, title: &str) {
    println!();
    println!("{title}  {}", run.describe());
    println!(
        "{:>6} {:>6} {:>6} {:>7} {:>7} {:>7} {:>8} {:>7} {:>7} {:>6} {:>6} {:>7} {:>7} {:>7} {:>7} {:>6} {:>6} {:>8} {:>7} {:>7}",
        "step",
        "live",
        "refus",
        "gen_cor",
        "srv_cor",
        "laps/s",
        "late_p99",
        "room_us",
        "tick_us",
        "fan_us",
        "rd/lap",
        "in/s",
        "rx/s",
        "drop/s",
        "tx/s",
        "Mbps",
        "save%",
        "wait_p99",
        "fwd_p50",
        "fwd_p99"
    );
}

/// Viewers accumulate across a scenario's steps, and stop only when the
/// scenario drops them.
struct Viewers {
    held: Vec<Detached>,
    asked: usize,
    refused: usize,
}

impl Viewers {
    fn new() -> Self {
        Viewers {
            held: Vec::new(),
            asked: 0,
            refused: 0,
        }
    }

    /// Adds exactly `count` more, whichever room they go to, so a scenario can
    /// fill several rooms in turn.
    async fn add(
        &mut self,
        run: &Run,
        server: &Server,
        client: &reqwest::Client,
        room_id: &str,
        more: usize,
        relay: bool,
    ) {
        let until = self.asked + more;

        while self.asked < until {
            match server.try_connect_viewer(client, room_id).await {
                Ok(mut viewer) => {
                    // Every other viewer offers to relay, which is the most
                    // pairs the server can form out of them.
                    if relay && self.asked.is_multiple_of(run.relay_share) {
                        viewer.advertise_relay_capacity(RelayCapacity {
                            upload_kbps: PLENTY_OF_UPLOAD,
                            rtt_ms: 5,
                            loss_pct: 0.0,
                        });
                    }

                    // Leaves have to report their link up, or the server keeps
                    // feeding them and the saving never shows.
                    viewer.accept_p2p_links();

                    // ICE finishes on the peer's own thread, so the run does not
                    // wait on it one viewer at a time.
                    self.held.push(viewer.detach());
                }
                Err(_) => self.refused += 1,
            }

            self.asked += 1;
        }

        let want = (self.asked - self.refused) as u64;
        let give_up_at = Instant::now() + ALL_CONNECTED;

        while Instant::now() < give_up_at {
            if count(&metrics(server, client).await, "viewers") >= want {
                break;
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn measure(&self, run: &Run, server: &Server, client: &reqwest::Client, step: usize) {
        tokio::time::sleep(run.settle).await;

        let before = Sample::take(server, client).await;
        tokio::time::sleep(run.window).await;
        let after = Sample::take(server, client).await;

        after.report(&before, step, self.refused);
    }
}

struct Sample {
    at: Instant,
    generator_ticks: u64,
    server_ticks: u64,
    drops: u64,
    metrics: Value,
}

impl Sample {
    async fn take(server: &Server, client: &reqwest::Client) -> Self {
        Sample {
            at: Instant::now(),
            generator_ticks: cpu_ticks("self"),
            server_ticks: cpu_ticks(&server.pid.to_string()),
            drops: udp_drops(server.media_port),
            metrics: metrics(server, client).await,
        }
    }

    fn report(&self, before: &Sample, step: usize, refused: usize) {
        let seconds = self.at.duration_since(before.at).as_secs_f64();
        let delta = |field: &str| {
            count(&self.metrics, field).saturating_sub(count(&before.metrics, field)) as f64
        };
        let per_second = |field: &str| delta(field) / seconds;

        let generator_cores =
            ticks_to_cores(self.generator_ticks - before.generator_ticks, seconds);
        let server_cores = ticks_to_cores(self.server_ticks - before.server_ticks, seconds);

        // A datagram is lost either in the kernel, before the mux reads it, or
        // in the mux, when the media loop's channel is full. Both count as
        // dropped, so `in/s` stays arrived and `rx/s` stays what the loop got.
        let queued = delta("socket_reads") - delta("channel_drops");
        let reads_per_lap = queued / delta("laps").max(1.0);

        let late = Windowed::between(&before.metrics, &self.metrics, "lap_lateness");
        let room = Windowed::between(&before.metrics, &self.metrics, "room_iteration");
        let tick = Windowed::between(&before.metrics, &self.metrics, "client_tick");
        let fanout = Windowed::between(&before.metrics, &self.metrics, "media_fanout");
        let wait = Windowed::between(&before.metrics, &self.metrics, "channel_wait");
        let forward = Windowed::between(&before.metrics, &self.metrics, "forward_delay");

        // Both sides are payload bytes of frames the server meant to send, so
        // the ratio holds even where the socket could not keep up and
        // `sent_bytes` fell behind what was written.
        let written = delta("media_write_bytes");
        let saved = delta("relay_saved_bytes");
        let saving = 100.0 * saved / (written + saved).max(1.0);

        let dropped = ((self.drops - before.drops) as f64 + delta("channel_drops")) / seconds;
        let received = queued / seconds;

        println!(
            "{step:>6} {:>6} {refused:>6} {generator_cores:>7.2} {server_cores:>7.2} {:>7.0} {:>8} {:>7} {:>7} {:>6} {:>6.0} {:>7.0} {:>7.0} {:>7.0} {:>7.0} {:>6.1} {saving:>6.1} {:>8} {:>7} {:>7}",
            count(&self.metrics, "viewers"),
            per_second("laps"),
            late.percentile(99),
            room.mean(),
            tick.mean(),
            fanout.mean(),
            reads_per_lap,
            received + dropped,
            received,
            dropped,
            per_second("sent_packets"),
            per_second("sent_bytes") * 8.0 / 1_000_000.0,
            wait.percentile(99),
            forward.percentile(50),
            forward.percentile(99),
        );
    }
}

/// Clock ticks are a hundred a second on every target this runs on, so a core
/// second is a hundred ticks.
fn ticks_to_cores(ticks: u64, seconds: f64) -> f64 {
    ticks as f64 / 100.0 / seconds
}

fn cpu_ticks(pid: &str) -> u64 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("proc stat");

    // The command field can hold spaces and parentheses, so the fields after it
    // are only safe to split from the last close paren onwards.
    let tail = &stat[stat.rfind(')').expect("proc stat comm") + 1..];
    let fields: Vec<&str> = tail.split_whitespace().collect();

    let utime: u64 = fields[11].parse().expect("utime");
    let stime: u64 = fields[12].parse().expect("stime");

    utime + stime
}

/// The system wide UDP counters mix in the generator's own sockets, which on
/// loopback are the same machine's. Per socket is the only honest source.
fn udp_drops(port: u16) -> u64 {
    let table = std::fs::read_to_string("/proc/net/udp").expect("proc net udp");

    for line in table.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();

        let Some(local_port) = fields[1].split(':').nth(1) else {
            continue;
        };

        if u16::from_str_radix(local_port, 16) == Ok(port) {
            return fields[12].parse().unwrap_or(0);
        }
    }

    0
}

async fn metrics(server: &Server, client: &reqwest::Client) -> Value {
    client
        .get(server.control_url("/metrics.json"))
        .bearer_auth(&server.api_key)
        .send()
        .await
        .expect("GET /metrics.json")
        .error_for_status()
        .expect("GET /metrics.json status")
        .json()
        .await
        .expect("metrics json")
}

fn count(metrics: &Value, field: &str) -> u64 {
    metrics[field].as_u64().unwrap_or(0)
}

/// `Value`'s own `Display` drops the format width, so every figure has to come
/// out as a number before it is printed.
fn stat(metrics: &Value, name: &str, field: &str) -> u64 {
    metrics[name][field].as_u64().unwrap_or(0)
}

/// The server reports its durations cumulatively, so a step's own figures only
/// exist as the difference between two readings. Subtracting the buckets gives
/// back a histogram of exactly the samples the window took.
struct Windowed {
    count: u64,
    total_us: u64,
    buckets: Vec<u64>,
}

impl Windowed {
    fn between(before: &Value, after: &Value, name: &str) -> Self {
        let buckets = |metrics: &Value| -> Vec<u64> {
            metrics[name]["buckets"]
                .as_array()
                .map(|values| values.iter().map(|v| v.as_u64().unwrap_or(0)).collect())
                .unwrap_or_default()
        };

        let (start, end) = (buckets(before), buckets(after));

        // A restarted server would report figures below the previous reading,
        // and a window that spans one is meaningless rather than negative.
        Windowed {
            count: stat(after, name, "count").saturating_sub(stat(before, name, "count")),
            total_us: stat(after, name, "total_us").saturating_sub(stat(before, name, "total_us")),
            buckets: end
                .iter()
                .enumerate()
                .map(|(i, count)| count.saturating_sub(start.get(i).copied().unwrap_or(0)))
                .collect(),
        }
    }

    fn mean(&self) -> u64 {
        self.total_us.checked_div(self.count).unwrap_or(0)
    }

    /// The upper bound of the bucket the percentile falls in, so 512 means
    /// "under 512us" with the real figure in the octave below.
    fn percentile(&self, percent: u64) -> u64 {
        if self.count == 0 {
            return 0;
        }

        let target = (self.count * percent).div_ceil(100).max(1);
        let mut seen = 0;

        for (bucket, count) in self.buckets.iter().enumerate() {
            seen += count;

            if seen >= target {
                return 1u64 << bucket;
            }
        }

        0
    }
}
