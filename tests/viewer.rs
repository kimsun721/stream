mod common;

use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};

use common::{Server, dc_message, is_connected};
use serde_json::{Value, json};
use str0m::Event;

const CONNECT: Duration = Duration::from_secs(10);
const REACT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn viewer_connects_to_a_live_room() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let room = server.create_live_room(&client).await;
    let mut viewer = server.connect_viewer(&client, &room.room_id).await;

    assert!(
        viewer.run_until(CONNECT, is_connected),
        "ICE never reported a usable path"
    );
}

/// Also requires ffmpeg, which stands in for a real encoder over WHIP.
#[tokio::test]
#[ignore = "needs ffmpeg"]
async fn viewer_receives_media_from_a_publisher() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let room = server.create_live_room(&client).await;
    let mut viewer = server.connect_viewer(&client, &room.room_id).await;

    assert!(viewer.run_until(CONNECT, is_connected), "viewer connect");

    let _publisher = Ffmpeg::spawn(&server, &room.stream_key);

    let got_media = viewer.run_until(Duration::from_secs(20), |event| {
        matches!(event, Event::MediaData(_))
    });

    assert!(got_media, "no media arrived after the publisher started");
}

#[tokio::test]
async fn viewer_is_told_which_layers_exist() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let (_publisher, room, mut viewer) = live_simulcast(&server, &client, &["l", "m", "h"]).await;

    let status = wait_for(&mut viewer, "layer_status").expect("no layer_status arrived");

    let rids: Vec<&str> = status["available_simulcast_layers"]
        .as_array()
        .expect("layer array")
        .iter()
        .map(|layer| layer["rid"].as_str().expect("rid"))
        .collect();

    assert_eq!(rids, ["l", "m", "h"], "advertised layers");
    assert_eq!(status["layer_mode"], "auto", "viewers start in auto");
    assert!(
        !status["chosen_layer"].is_null(),
        "a layer is already chosen"
    );

    drop(room);
}

#[tokio::test]
async fn viewer_switches_layers_by_hand() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let (_publisher, room, mut viewer) = live_simulcast(&server, &client, &["l", "m", "h"]).await;

    let status = wait_for(&mut viewer, "layer_status").expect("no layer_status arrived");
    let mid = status["mid"].clone();
    let chosen = status["chosen_layer"].as_str().expect("chosen layer");

    // Auto owns the choice until the viewer takes it, so a set_layer in auto
    // mode is ignored.
    viewer.send(&json!({ "type": "set_layer_mode", "mid": mid, "layer_mode": "manual" }));

    let target = ["l", "m", "h"]
        .into_iter()
        .find(|rid| *rid != chosen)
        .expect("another layer");

    viewer.send(&json!({ "type": "set_layer", "mid": mid, "rid": target }));

    let changed = wait_for(&mut viewer, "layer_changed").expect("no layer_changed arrived");

    assert_eq!(changed["rid"], target, "switched to the requested layer");

    drop(room);
}

/// The estimates are what auto layer selection filters on, and they come from
/// counted RTP bytes rather than from the SDP, so nothing but real traffic
/// proves the path is alive.
#[tokio::test]
async fn the_server_measures_every_layer_it_receives() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let room = server.create_room(&client).await;

    let mut publisher = server
        .connect_publisher(&client, &room.stream_key, &["l", "m", "h"])
        .await;
    assert!(
        publisher.run_until(CONNECT, is_connected),
        "publisher connect"
    );

    publisher.start_media(&[("l", 300), ("m", 800), ("h", 2500)]);

    // layer_status is sent once, when the viewer's track opens, so the window
    // has to have filled before the viewer arrives.
    publisher.run_until(Duration::from_secs(2), |_| false);
    let _publisher = publisher.detach();

    server.set_live(&client, &room.room_id).await;

    let mut viewer = server.connect_viewer(&client, &room.room_id).await;
    assert!(viewer.run_until(CONNECT, is_connected), "viewer connect");

    let status = wait_for(&mut viewer, "layer_status").expect("no layer_status arrived");

    let estimates: Vec<(String, Option<u64>)> = status["available_simulcast_layers"]
        .as_array()
        .expect("layer array")
        .iter()
        .map(|layer| {
            (
                layer["rid"].as_str().expect("rid").to_string(),
                layer["bitrate_estimate"].as_u64(),
            )
        })
        .collect();

    let measured: Vec<u64> = estimates
        .iter()
        .map(|(rid, estimate)| estimate.unwrap_or_else(|| panic!("{rid} was never measured")))
        .collect();

    assert!(
        measured.windows(2).all(|w| w[0] < w[1]),
        "layers rank by the bitrate they carry: {estimates:?}"
    );

    let (low, high) = (measured[0], measured[measured.len() - 1]);
    assert!(
        high > low * 4,
        "the gap matches what was sent, 300 against 2500 kbps: {estimates:?}"
    );
}

/// A room with a simulcast publisher attached and a connected viewer. The
/// publisher connects first because that moves the room to `Preview`, and the
/// viewer needs it `Live`.
async fn live_simulcast(
    server: &Server,
    client: &reqwest::Client,
    rids: &[&str],
) -> (common::Detached, common::CreatedRoom, common::Peer) {
    let room = server.create_room(client).await;

    let mut publisher = server
        .connect_publisher(client, &room.stream_key, rids)
        .await;
    assert!(
        publisher.run_until(CONNECT, is_connected),
        "publisher connect"
    );

    server.set_live(client, &room.room_id).await;

    let mut viewer = server.connect_viewer(client, &room.room_id).await;
    assert!(viewer.run_until(CONNECT, is_connected), "viewer connect");

    (publisher.detach(), room, viewer)
}

fn wait_for(viewer: &mut common::Peer, tag: &str) -> Option<Value> {
    let mut found = None;

    viewer.run_until(REACT, |event| {
        found = dc_message(event, tag);
        found.is_some()
    });

    found
}

struct Ffmpeg(Child);

impl Ffmpeg {
    fn spawn(server: &Server, stream_key: &str) -> Self {
        let child = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-re",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=15",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "whip",
                "-authorization",
                stream_key,
                &server.sdp_url("/whip"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn ffmpeg");

        Ffmpeg(child)
    }
}

impl Drop for Ffmpeg {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
