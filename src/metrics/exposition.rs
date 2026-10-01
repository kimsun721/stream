//! The same counters as the JSON snapshot, in the text format Prometheus
//! scrapes. Nothing is copied into `prometheus-client` types: the collector reads
//! the atomics at scrape time, so the JSON and this exposition never disagree.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use prometheus_client::{
    collector::Collector,
    encoding::{DescriptorEncoder, EncodeMetric, MetricEncoder, NoLabelSet, text},
    metrics::{MetricType, counter::ConstCounter, gauge::ConstGauge},
    registry::Registry,
};

use super::{
    BUCKET_COUNT, CHANNEL_DRAIN_FULL, CHANNEL_DROPS, CHANNEL_MESSAGES, CHANNEL_WAIT,
    CLIENT_TICK_ERRORS, CLIENTS_DISCONNECTED, DEMOTIONS, Durations, FORWARD_DELAY, Histogram,
    LAP_LATENESS, LAPS, MEDIA_DATAS, MEDIA_WRITE_BYTES, MEDIA_WRITES, PHASES, PROMOTIONS, Phase,
    RELAY_SAVED_BYTES, RELAY_SAVED_FRAMES, ROOMS_SKIPPED, ROOMS_VISITED, SENT_BYTES, SENT_PACKETS,
    SOCKET_READ_BYTES, SOCKET_READS, ShardGauges, shards,
};

pub const CONTENT_TYPE: &str = "application/openmetrics-text; version=1.0.0; charset=utf-8";

pub fn render() -> String {
    let mut registry = Registry::default();
    registry.register_collector(Box::new(Exposition));

    let mut out = String::new();
    // Writing to a `String` cannot fail, and neither can the collector.
    text::encode(&mut out, &registry).expect("encoding metrics");
    out
}

#[derive(Debug)]
struct Exposition;

impl Collector for Exposition {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), std::fmt::Error> {
        for (name, help, counter) in counters() {
            let metric = encoder.encode_descriptor(name, help, None, MetricType::Counter)?;
            ConstCounter::new(counter.load(Relaxed)).encode(metric)?;
        }

        if let Some(seconds) = process_cpu_seconds() {
            let metric = encoder.encode_descriptor(
                "process_cpu_seconds",
                "User and system CPU time the process has spent",
                None,
                MetricType::Counter,
            )?;
            ConstCounter::new(seconds).encode(metric)?;
        }

        for (name, help, field) in gauges() {
            let mut metric = encoder.encode_descriptor(name, help, None, MetricType::Gauge)?;

            for (index, slot) in shards().iter().enumerate() {
                let labels = [("loop", index.to_string())];
                ConstGauge::new(field(slot).load(Relaxed) as i64)
                    .encode(metric.encode_family(&labels)?)?;
            }
        }

        let mut metric = encoder.encode_descriptor(
            "stream_phase_seconds",
            "Time the media loops spend in each phase of a lap",
            None,
            MetricType::Histogram,
        )?;

        for (phase, label) in PHASE_LABELS {
            let labels = [("phase", label)];
            Buckets::of(&PHASES[phase as usize]).encode(metric.encode_family(&labels)?)?;
        }

        for (name, help, histogram) in histograms() {
            let metric = encoder.encode_descriptor(name, help, None, MetricType::Histogram)?;
            Buckets::of(histogram).encode(metric)?;
        }

        Ok(())
    }
}

fn counters() -> [(&'static str, &'static str, &'static AtomicU64); 19] {
    [
        ("stream_laps", "Media loop iterations", &LAPS),
        (
            "stream_channel_messages",
            "Messages the media loops took off their channels",
            &CHANNEL_MESSAGES,
        ),
        (
            "stream_channel_drain_full",
            "Drains that stopped at their cap rather than at an empty channel",
            &CHANNEL_DRAIN_FULL,
        ),
        (
            "stream_channel_drops",
            "Datagrams the mux read but dropped because a loop's channel was full",
            &CHANNEL_DROPS,
        ),
        (
            "stream_socket_reads",
            "Datagrams read from the media socket",
            &SOCKET_READS,
        ),
        (
            "stream_socket_read_bytes",
            "Bytes read from the media socket",
            &SOCKET_READ_BYTES,
        ),
        (
            "stream_rooms_visited",
            "Room iterations that did work",
            &ROOMS_VISITED,
        ),
        (
            "stream_rooms_skipped",
            "Room iterations skipped because the room was empty and idle",
            &ROOMS_SKIPPED,
        ),
        (
            "stream_sent_packets",
            "Datagrams sent on the media socket",
            &SENT_PACKETS,
        ),
        (
            "stream_sent_bytes",
            "Bytes sent on the media socket",
            &SENT_BYTES,
        ),
        (
            "stream_relay_saved_frames",
            "Frames a relay carried to its leaf instead of the server",
            &RELAY_SAVED_FRAMES,
        ),
        (
            "stream_relay_saved_bytes",
            "Payload bytes a relay carried to its leaf instead of the server",
            &RELAY_SAVED_BYTES,
        ),
        (
            "stream_media_datas",
            "Frames received from publishers",
            &MEDIA_DATAS,
        ),
        (
            "stream_media_writes",
            "Frames written to viewers",
            &MEDIA_WRITES,
        ),
        (
            "stream_media_write_bytes",
            "Payload bytes written to viewers",
            &MEDIA_WRITE_BYTES,
        ),
        (
            "stream_client_tick_errors",
            "Client ticks that failed",
            &CLIENT_TICK_ERRORS,
        ),
        (
            "stream_clients_disconnected",
            "Clients removed after disconnecting",
            &CLIENTS_DISCONNECTED,
        ),
        (
            "stream_promotions",
            "Viewers promoted to relay",
            &PROMOTIONS,
        ),
        ("stream_demotions", "Relays demoted", &DEMOTIONS),
    ]
}

type GaugeField = fn(&ShardGauges) -> &AtomicU64;

fn gauges() -> [(&'static str, &'static str, GaugeField); 5] {
    [
        ("stream_rooms", "Rooms on each media loop", |slot| {
            &slot.rooms
        }),
        (
            "stream_clients",
            "Publishers and viewers on each media loop",
            |slot| &slot.clients,
        ),
        ("stream_viewers", "Viewers on each media loop", |slot| {
            &slot.viewers
        }),
        ("stream_relays", "Viewers relaying to a leaf", |slot| {
            &slot.relays
        }),
        (
            "stream_connected_leaves",
            "Leaves receiving from their relay",
            |slot| &slot.connected_leaves,
        ),
    ]
}

const PHASE_LABELS: [(Phase, &str); 6] = [
    (Phase::ChannelDrain, "channel_drain"),
    (Phase::Lap, "lap"),
    (Phase::RoomIteration, "room_iteration"),
    (Phase::ClientTick, "client_tick"),
    (Phase::MediaFanout, "media_fanout"),
    (Phase::RelayPolicy, "relay_policy"),
];

fn histograms() -> [(&'static str, &'static str, &'static Histogram); 3] {
    [
        (
            "stream_lap_lateness_seconds",
            "How far past the deadline str0m asked for a loop woke",
            &LAP_LATENESS,
        ),
        (
            "stream_channel_wait_seconds",
            "From the mux reading a datagram to a loop taking it off its channel",
            &CHANNEL_WAIT,
        ),
        (
            "stream_forward_delay_seconds",
            "From a frame's first packet reaching its loop to the frame being written for a viewer",
            &FORWARD_DELAY,
        ),
    ]
}

/// A histogram read once, so its count, sum and buckets come from the same
/// moment. The count is taken from the buckets rather than from its own atomic,
/// which a concurrent record could have moved in between.
struct Buckets(Durations);

impl Buckets {
    fn of(histogram: &Histogram) -> Self {
        Buckets(histogram.snapshot())
    }
}

impl EncodeMetric for Buckets {
    fn encode(&self, mut encoder: MetricEncoder) -> Result<(), std::fmt::Error> {
        // Bucket `i` holds durations under `2^i` microseconds. The last one also
        // takes everything longer, so it is the `+Inf` bucket.
        let buckets: Vec<(f64, u64)> = self
            .0
            .buckets
            .iter()
            .enumerate()
            .map(|(i, &count)| {
                let upper_bound = if i == BUCKET_COUNT - 1 {
                    f64::MAX
                } else {
                    (1u64 << i) as f64 / 1_000_000.0
                };
                (upper_bound, count)
            })
            .collect();

        let count = self.0.buckets.iter().sum();
        let sum = self.0.total_us as f64 / 1_000_000.0;

        encoder.encode_histogram::<NoLabelSet>(sum, count, &buckets, None)
    }

    fn metric_type(&self) -> MetricType {
        MetricType::Histogram
    }
}

/// Linux reports CPU time in clock ticks, a hundred a second on every target
/// this runs on. Elsewhere the metric is left out.
fn process_cpu_seconds() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;

    // The command name can hold spaces and parentheses, so the fields after it
    // are only safe to split from the last close paren onwards.
    let fields: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    let user: u64 = fields.get(11)?.parse().ok()?;
    let system: u64 = fields.get(12)?.parse().ok()?;

    Some((user + system) as f64 / 100.0)
}
