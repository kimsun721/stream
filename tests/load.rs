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

mod common;

use std::time::{Duration, Instant};

use common::{CreatedRoom, Detached, RelayCapacity, Server, is_connected};
use serde_json::Value;

const CONNECT: Duration = Duration::from_secs(15);
/// Long enough for the connect burst to drain out of the averages.
const SETTLE: Duration = Duration::from_secs(5);
const WINDOW: Duration = Duration::from_secs(10);
/// A step gives up on stragglers rather than on the whole run, and reports the
/// count the server actually holds.
const ALL_CONNECTED: Duration = Duration::from_secs(60);

/// Above the 13000 kbps default cutoff, so the server will promote the viewer.
const PLENTY_OF_UPLOAD: u32 = 20_000;

#[tokio::test]
#[ignore = "load run"]
async fn baseline() {
    let server = Server::default();
    let client = reqwest::Client::new();

    viewers_in_one_room(&server, &client).await;
    viewers_with_relay(&server, &client).await;
    viewers_across_rooms(&server, &client).await;
}

/// What one viewer costs, which every other run is compared against. The
/// generator's own CPU is printed beside the server's: if it is far larger, the
/// run measured this machine rather than the server.
async fn viewers_in_one_room(server: &Server, client: &reqwest::Client) {
    let (publisher, room) = publish(server, client).await;

    header("viewers in one room");

    let mut viewers = Viewers::new();
    let mut added = 0;

    for target in [50, 100, 200, 400, 500, 600, 800] {
        viewers
            .add(server, client, &room.room_id, target - added, false)
            .await;
        added = target;

        viewers.measure(server, client, target).await;
    }

    teardown(server, client, viewers, vec![publisher], &[room]).await;
}

/// The same sweep with every other viewer advertising the upload a relay needs.
/// One relay carries one leaf, so the saving cannot pass half whatever the
/// viewers advertise.
async fn viewers_with_relay(server: &Server, client: &reqwest::Client) {
    let (publisher, room) = publish(server, client).await;

    header("viewers with relay");

    let mut viewers = Viewers::new();
    let mut added = 0;

    for target in [50, 100, 200, 400] {
        viewers
            .add(server, client, &room.room_id, target - added, true)
            .await;
        added = target;

        viewers.measure(server, client, target).await;
    }

    teardown(server, client, viewers, vec![publisher], &[room]).await;
}

/// One viewer count split across more rooms. Room work is a small share of the
/// loop, so this says how much of the cost is the walk itself rather than what
/// each room holds.
async fn viewers_across_rooms(server: &Server, client: &reqwest::Client) {
    header("400 viewers across rooms");

    for rooms in [1, 4, 20] {
        let mut publishers: Vec<Detached> = Vec::new();
        let mut created = Vec::new();
        let mut viewers = Viewers::new();

        for _ in 0..rooms {
            let (publisher, room) = publish(server, client).await;
            publishers.push(publisher);

            viewers
                .add(server, client, &room.room_id, 400 / rooms, false)
                .await;

            created.push(room);
        }

        viewers.measure(server, client, rooms).await;

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

async fn publish(server: &Server, client: &reqwest::Client) -> (Detached, CreatedRoom) {
    let room = server.create_room(client).await;

    let mut publisher = server
        .connect_publisher(client, &room.stream_key, &["l", "m", "h"])
        .await;
    assert!(
        publisher.run_until(CONNECT, is_connected),
        "publisher connect"
    );
    publisher.start_media(&[("l", 300), ("m", 800), ("h", 2500)]);

    server.set_live(client, &room.room_id).await;

    (publisher.detach(), room)
}

fn header(title: &str) {
    println!();
    println!("{title}");
    println!(
        "{:>6} {:>6} {:>6} {:>7} {:>7} {:>7} {:>8} {:>7} {:>7} {:>6} {:>7} {:>8} {:>6} {:>8} {:>6}",
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
        "pkt/s",
        "MB/s",
        "drops/s",
        "save%"
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
                    if relay && self.asked.is_multiple_of(2) {
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

    async fn measure(&self, server: &Server, client: &reqwest::Client, step: usize) {
        tokio::time::sleep(SETTLE).await;

        let before = Sample::take(server, client).await;
        tokio::time::sleep(WINDOW).await;
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

        // How close the drain runs to its own cap, which decides whether the
        // socket is read faster than it fills.
        let reads_per_lap = delta("socket_reads") / delta("laps").max(1.0);

        let late = Windowed::between(&before.metrics, &self.metrics, "lap_lateness");
        let room = Windowed::between(&before.metrics, &self.metrics, "room_iteration");
        let tick = Windowed::between(&before.metrics, &self.metrics, "client_tick");
        let fanout = Windowed::between(&before.metrics, &self.metrics, "media_fanout");

        // Both sides are payload bytes of frames the server meant to send, so
        // the ratio holds even where the socket could not keep up and
        // `sent_bytes` fell behind what was written.
        let written = delta("media_write_bytes");
        let saved = delta("relay_saved_bytes");
        let saving = 100.0 * saved / (written + saved).max(1.0);

        println!(
            "{step:>6} {:>6} {refused:>6} {generator_cores:>7.2} {server_cores:>7.2} {:>7.0} {:>8} {:>7} {:>7} {:>6} {:>7.0} {:>8.0} {:>6.1} {:>8.0} {saving:>6.1}",
            count(&self.metrics, "viewers"),
            per_second("laps"),
            late.percentile(99),
            room.mean(),
            tick.mean(),
            fanout.mean(),
            reads_per_lap,
            per_second("sent_packets"),
            per_second("sent_bytes") / 1_000_000.0,
            (self.drops - before.drops) as f64 / seconds,
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
        .get(server.control_url("/metrics"))
        .bearer_auth(&server.api_key)
        .send()
        .await
        .expect("GET /metrics")
        .error_for_status()
        .expect("GET /metrics status")
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
