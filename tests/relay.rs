mod common;

use std::time::{Duration, Instant};

use common::{Peer, RelayCapacity, Server, dc_message, is_connected};
use serde_json::json;
use str0m::Event;

const CONNECT: Duration = Duration::from_secs(10);
const PROMOTE: Duration = Duration::from_secs(10);
/// Covers the handshake timeout with room to spare.
const TEAR_DOWN: Duration = Duration::from_secs(15);
/// Long enough that a promotion would already have happened: the connection age
/// gate is the slowest step at 2 seconds.
const NEVER: Duration = Duration::from_secs(5);
const SETTLE: Duration = Duration::from_secs(1);

/// Above the 13000 kbps default cutoff.
const PLENTY_OF_UPLOAD: u32 = 20_000;

fn healthy(upload_kbps: u32) -> RelayCapacity {
    RelayCapacity {
        upload_kbps,
        rtt_ms: 5,
        loss_pct: 0.0,
    }
}

/// A live room carrying media, plus two viewers. Only the first advertises the
/// capacity a relay needs, so the server has one candidate and one leaf to pair
/// it with.
async fn room_with_two_viewers(
    server: &Server,
    client: &reqwest::Client,
    capacity: RelayCapacity,
) -> (common::Detached, Peer, Peer) {
    let room = server.create_room(client).await;

    let mut publisher = server
        .connect_publisher(client, &room.stream_key, &["h"])
        .await;
    assert!(
        publisher.run_until(CONNECT, is_connected),
        "publisher connect"
    );
    publisher.start_media(&[("h", 1200)]);
    let publisher = publisher.detach();

    server.set_live(client, &room.room_id).await;

    let mut candidate = server.connect_viewer(client, &room.room_id).await;
    assert!(candidate.run_until(CONNECT, is_connected), "viewer connect");
    candidate.advertise_relay_capacity(capacity);

    let mut leaf = server.connect_viewer(client, &room.room_id).await;
    assert!(leaf.run_until(CONNECT, is_connected), "viewer connect");

    (publisher, candidate, leaf)
}

/// Both peers have to be driven together: the server asks the candidate for an
/// offer, forwards it to the leaf, and neither makes progress while the other
/// is parked.
fn run_both(
    a: &mut Peer,
    b: &mut Peer,
    total: Duration,
    mut stop: impl FnMut(&Event) -> bool,
) -> bool {
    let give_up_at = Instant::now() + total;
    let lap = Duration::from_millis(50);

    while Instant::now() < give_up_at {
        if a.run_until(lap, &mut stop) || b.run_until(lap, &mut stop) {
            return true;
        }
    }

    false
}

#[tokio::test]
async fn a_viewer_with_upload_to_spare_is_asked_to_relay() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let (_publisher, mut candidate, mut leaf) =
        room_with_two_viewers(&server, &client, healthy(PLENTY_OF_UPLOAD)).await;

    assert!(
        run_both(&mut candidate, &mut leaf, PROMOTE, |event| dc_message(
            event,
            "request_offer"
        )
        .is_some()),
        "the server never asked the candidate for a relay offer"
    );

    assert!(
        run_both(&mut candidate, &mut leaf, PROMOTE, |event| dc_message(
            event,
            "p2p_offer"
        )
        .is_some()),
        "the offer never reached the leaf"
    );
}

#[tokio::test]
async fn a_viewer_without_upload_to_spare_is_left_alone() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let (_publisher, mut candidate, mut leaf) =
        room_with_two_viewers(&server, &client, healthy(1_000)).await;

    assert!(
        !run_both(&mut candidate, &mut leaf, NEVER, |event| dc_message(
            event,
            "request_offer"
        )
        .is_some()),
        "1 Mbps of upload is under the cutoff"
    );
}

#[tokio::test]
async fn the_server_stops_sending_to_a_connected_leaf() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let (_publisher, mut candidate, mut leaf) =
        room_with_two_viewers(&server, &client, healthy(PLENTY_OF_UPLOAD)).await;

    assert!(
        run_both(&mut candidate, &mut leaf, PROMOTE, |event| dc_message(
            event,
            "p2p_offer"
        )
        .is_some()),
        "the offer never reached the leaf"
    );

    assert!(
        received_media(&mut leaf),
        "the leaf still needs the server before the peer link is up"
    );

    leaf.send(&json!({ "type": "p2p_connected" }));
    leaf.run_until(SETTLE, |_| false);

    assert!(
        !received_media(&mut leaf),
        "the leaf is fed by the relay now, so the server has nothing to send it"
    );
    assert!(
        received_media(&mut candidate),
        "the relay still gets its own copy"
    );

    leaf.send(&json!({ "type": "p2p_disconnected" }));
    leaf.run_until(SETTLE, |_| false);

    assert!(
        received_media(&mut leaf),
        "the server takes the leaf back when the peer link drops"
    );
}

fn received_media(peer: &mut Peer) -> bool {
    peer.run_until(SETTLE, |event| matches!(event, Event::MediaData(_)))
}

/// The server skips a leaf only once the leaf reports the link up, so a
/// handshake still in progress must not cost the leaf its stream. It holds until
/// the handshake times out, which the next test covers.
#[tokio::test]
async fn a_leaf_keeps_its_stream_while_the_peer_link_hangs() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let (_publisher, mut candidate, mut leaf) =
        room_with_two_viewers(&server, &client, healthy(PLENTY_OF_UPLOAD)).await;

    assert!(
        run_both(&mut candidate, &mut leaf, PROMOTE, |event| dc_message(
            event,
            "p2p_offer"
        )
        .is_some()),
        "the offer never reached the leaf"
    );

    // No p2p_connected follows, so the link stays Connecting.
    run_both(&mut candidate, &mut leaf, SETTLE * 3, |_| false);

    assert!(received_media(&mut leaf), "the leaf lost its stream");
    assert!(received_media(&mut candidate), "the relay lost its stream");
}

/// A handshake that never completes would otherwise leave the pair joined for
/// good, taking both viewers out of the candidate pool for the rest of the
/// broadcast. Nothing in the protocol reports this: the clients have no reason
/// to say anything, so the server times it out itself.
#[tokio::test]
async fn a_peer_link_that_never_comes_up_is_torn_down() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let (_publisher, mut candidate, mut leaf) =
        room_with_two_viewers(&server, &client, healthy(PLENTY_OF_UPLOAD)).await;

    assert!(
        run_both(&mut candidate, &mut leaf, PROMOTE, |event| dc_message(
            event,
            "p2p_offer"
        )
        .is_some()),
        "the offer never reached the leaf"
    );

    assert!(
        run_both(&mut candidate, &mut leaf, TEAR_DOWN, |event| dc_message(
            event, "demote"
        )
        .is_some()),
        "the pair was never released"
    );
}
