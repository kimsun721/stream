//! Counters the media loop keeps for the life of the process, so a baseline
//! measured today can be compared with one measured after an optimization. The
//! measurement points move only by an explicit edit to a call site, which the
//! history then records.
//!
//! The media loop is the only writer, so every ordering is `Relaxed`. Nothing
//! here synchronizes anything; the values are read for reporting alone.

use std::{
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    time::{Duration, Instant},
};

use serde::Serialize;

/// One bucket per power of two microseconds, so bucket `i` holds durations
/// under `2^i`. 24 of them reach 8 seconds, well past anything the loop should
/// produce.
const BUCKET_COUNT: usize = 24;

struct Histogram {
    buckets: [AtomicU64; BUCKET_COUNT],
    count: AtomicU64,
    total_us: AtomicU64,
    max_us: AtomicU64,
}

impl Histogram {
    const fn new() -> Self {
        Histogram {
            buckets: [const { AtomicU64::new(0) }; BUCKET_COUNT],
            count: AtomicU64::new(0),
            total_us: AtomicU64::new(0),
            max_us: AtomicU64::new(0),
        }
    }

    fn record(&self, duration: Duration) {
        let us = duration.as_micros().min(u64::MAX as u128) as u64;
        let bucket = (u64::BITS - us.leading_zeros()) as usize;

        self.buckets[bucket.min(BUCKET_COUNT - 1)].fetch_add(1, Relaxed);
        self.count.fetch_add(1, Relaxed);
        self.total_us.fetch_add(us, Relaxed);
        self.max_us.fetch_max(us, Relaxed);
    }

    fn snapshot(&self) -> Durations {
        let count = self.count.load(Relaxed);
        let counts: Vec<u64> = self.buckets.iter().map(|b| b.load(Relaxed)).collect();

        Durations {
            count,
            mean_us: if count == 0 {
                0
            } else {
                self.total_us.load(Relaxed) / count
            },
            max_us: self.max_us.load(Relaxed),
            p50_us: percentile(&counts, count, 50),
            p95_us: percentile(&counts, count, 95),
            p99_us: percentile(&counts, count, 99),
        }
    }
}

/// The upper bound of the bucket the percentile falls in, not an interpolated
/// value: a reported 512 means "under 512us", and the real figure is somewhere
/// in the octave below it.
fn percentile(counts: &[u64], total: u64, percent: u64) -> u64 {
    if total == 0 {
        return 0;
    }

    // At least one sample has to land at or below the answer, so a single
    // sample cannot report the empty first bucket.
    let target = (total * percent).div_ceil(100).max(1);
    let mut seen = 0;

    for (bucket, count) in counts.iter().enumerate() {
        seen += count;
        if seen >= target {
            return 1u64 << bucket;
        }
    }

    1u64 << (BUCKET_COUNT - 1)
}

#[derive(Clone, Copy)]
pub enum Phase {
    /// Draining the control channel, which blocks the media loop for as long as
    /// the web threads keep it fed.
    ControlDrain,
    /// A whole lap, socket wait included. Falls as load rises, because the wait
    /// is what disappears first.
    Lap,
    /// One room's entire block.
    RoomIteration,
    /// Polling every client in a room, which is where str0m and the socket
    /// sends live.
    ClientTick,
    /// Writing the publisher's frames out to the room's viewers.
    MediaFanout,
    /// Probing, promotion, demotion and the peer link timeout scan. Four passes
    /// over the room's clients that carry no media.
    RelayPolicy,
}

const PHASE_COUNT: usize = 6;

static PHASES: [Histogram; PHASE_COUNT] = [const { Histogram::new() }; PHASE_COUNT];

/// How far past the deadline str0m asked for the loop actually woke. Waking
/// early on a datagram records zero, so anything above it is the loop running
/// late and every timer str0m owns slipping with it.
static LAP_LATENESS: Histogram = Histogram::new();

/// Stops the phase when it leaves scope.
pub struct Timer {
    phase: Phase,
    started_at: Instant,
}

impl Drop for Timer {
    fn drop(&mut self) {
        PHASES[self.phase as usize].record(self.started_at.elapsed());
    }
}

pub fn time(phase: Phase) -> Timer {
    Timer {
        phase,
        started_at: Instant::now(),
    }
}

pub fn lap_lateness(lateness: Duration) {
    LAP_LATENESS.record(lateness);
}

macro_rules! atomics {
    ($($name:ident),* $(,)?) => {
        $(static $name: AtomicU64 = AtomicU64::new(0);)*
    };
}

atomics!(
    LAPS,
    CONTROL_MESSAGES,
    SOCKET_READS,
    SOCKET_READ_BYTES,
    SOCKET_DRAIN_FULL,
    ROOMS_VISITED,
    ROOMS_SKIPPED,
    SENT_PACKETS,
    SENT_BYTES,
    RELAY_SAVED_PACKETS,
    RELAY_SAVED_BYTES,
    MEDIA_DATAS,
    MEDIA_WRITES,
    CLIENT_TICK_ERRORS,
    CLIENTS_DISCONNECTED,
    PROMOTIONS,
    DEMOTIONS,
);

pub fn lap() {
    LAPS.fetch_add(1, Relaxed);
}

pub fn control_messages(count: u64) {
    CONTROL_MESSAGES.fetch_add(count, Relaxed);
}

pub fn socket_read(bytes: usize) {
    SOCKET_READS.fetch_add(1, Relaxed);
    SOCKET_READ_BYTES.fetch_add(bytes as u64, Relaxed);
}

/// The drain stopped at its own cap rather than at an empty socket. Whether
/// datagrams were still queued is not observable from here, so this only says
/// the loop could not finish what it started.
pub fn socket_drain_full() {
    SOCKET_DRAIN_FULL.fetch_add(1, Relaxed);
}

pub fn room_visited() {
    ROOMS_VISITED.fetch_add(1, Relaxed);
}

pub fn room_skipped() {
    ROOMS_SKIPPED.fetch_add(1, Relaxed);
}

pub fn sent(bytes: usize) {
    SENT_PACKETS.fetch_add(1, Relaxed);
    SENT_BYTES.fetch_add(bytes as u64, Relaxed);
}

/// Bytes a leaf did not receive because its relay is carrying them instead.
/// Only the frames the leaf would have matched count, so the caller applies the
/// same mid and rid test the fanout does.
pub fn relay_saved(bytes: usize) {
    RELAY_SAVED_PACKETS.fetch_add(1, Relaxed);
    RELAY_SAVED_BYTES.fetch_add(bytes as u64, Relaxed);
}

pub fn media_data() {
    MEDIA_DATAS.fetch_add(1, Relaxed);
}

pub fn media_write() {
    MEDIA_WRITES.fetch_add(1, Relaxed);
}

pub fn client_tick_error() {
    CLIENT_TICK_ERRORS.fetch_add(1, Relaxed);
}

pub fn client_disconnected() {
    CLIENTS_DISCONNECTED.fetch_add(1, Relaxed);
}

pub fn promoted() {
    PROMOTIONS.fetch_add(1, Relaxed);
}

pub fn demoted() {
    DEMOTIONS.fetch_add(1, Relaxed);
}

atomics!(ROOMS, CLIENTS, VIEWERS, RELAYS, CONNECTED_LEAVES);

/// What the loop currently holds, as opposed to what it has done. Written once
/// a lap because no other thread can walk `Rooms`.
pub struct Gauges {
    pub rooms: u64,
    pub clients: u64,
    pub viewers: u64,
    pub relays: u64,
    pub connected_leaves: u64,
}

pub fn gauges(gauges: Gauges) {
    ROOMS.store(gauges.rooms, Relaxed);
    CLIENTS.store(gauges.clients, Relaxed);
    VIEWERS.store(gauges.viewers, Relaxed);
    RELAYS.store(gauges.relays, Relaxed);
    CONNECTED_LEAVES.store(gauges.connected_leaves, Relaxed);
}

#[derive(Serialize)]
pub struct Durations {
    pub count: u64,
    pub mean_us: u64,
    pub max_us: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
}

/// Cumulative since start. Two reads and a subtraction give any window, and a
/// read that reset them would spoil the next one.
#[derive(Serialize)]
pub struct Snapshot {
    pub laps: u64,
    pub control_messages: u64,
    pub socket_reads: u64,
    pub socket_read_bytes: u64,
    pub socket_drain_full: u64,

    pub rooms_visited: u64,
    pub rooms_skipped: u64,

    pub sent_packets: u64,
    pub sent_bytes: u64,
    pub relay_saved_packets: u64,
    pub relay_saved_bytes: u64,
    pub media_datas: u64,
    pub media_writes: u64,

    pub client_tick_errors: u64,
    pub clients_disconnected: u64,
    pub promotions: u64,
    pub demotions: u64,

    pub rooms: u64,
    pub clients: u64,
    pub viewers: u64,
    pub relays: u64,
    pub connected_leaves: u64,

    pub lap: Durations,
    pub lap_lateness: Durations,
    pub control_drain: Durations,
    pub room_iteration: Durations,
    pub client_tick: Durations,
    pub media_fanout: Durations,
    pub relay_policy: Durations,
}

pub fn snapshot() -> Snapshot {
    Snapshot {
        laps: LAPS.load(Relaxed),
        control_messages: CONTROL_MESSAGES.load(Relaxed),
        socket_reads: SOCKET_READS.load(Relaxed),
        socket_read_bytes: SOCKET_READ_BYTES.load(Relaxed),
        socket_drain_full: SOCKET_DRAIN_FULL.load(Relaxed),

        rooms_visited: ROOMS_VISITED.load(Relaxed),
        rooms_skipped: ROOMS_SKIPPED.load(Relaxed),

        sent_packets: SENT_PACKETS.load(Relaxed),
        sent_bytes: SENT_BYTES.load(Relaxed),
        relay_saved_packets: RELAY_SAVED_PACKETS.load(Relaxed),
        relay_saved_bytes: RELAY_SAVED_BYTES.load(Relaxed),
        media_datas: MEDIA_DATAS.load(Relaxed),
        media_writes: MEDIA_WRITES.load(Relaxed),

        client_tick_errors: CLIENT_TICK_ERRORS.load(Relaxed),
        clients_disconnected: CLIENTS_DISCONNECTED.load(Relaxed),
        promotions: PROMOTIONS.load(Relaxed),
        demotions: DEMOTIONS.load(Relaxed),

        rooms: ROOMS.load(Relaxed),
        clients: CLIENTS.load(Relaxed),
        viewers: VIEWERS.load(Relaxed),
        relays: RELAYS.load(Relaxed),
        connected_leaves: CONNECTED_LEAVES.load(Relaxed),

        lap: PHASES[Phase::Lap as usize].snapshot(),
        lap_lateness: LAP_LATENESS.snapshot(),
        control_drain: PHASES[Phase::ControlDrain as usize].snapshot(),
        room_iteration: PHASES[Phase::RoomIteration as usize].snapshot(),
        client_tick: PHASES[Phase::ClientTick as usize].snapshot(),
        media_fanout: PHASES[Phase::MediaFanout as usize].snapshot(),
        relay_policy: PHASES[Phase::RelayPolicy as usize].snapshot(),
    }
}
