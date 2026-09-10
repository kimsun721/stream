mod common;

use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};

use common::{Server, is_connected, run_until};
use str0m::Event;

/// Requires a running server. Start one with `cargo run`; the key comes from
/// `.env`, the same file the server reads.
#[tokio::test]
#[ignore = "needs a running server"]
async fn viewer_connects_to_a_live_room() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let room = server.create_live_room(&client).await;
    let (mut rtc, socket) = server.connect_viewer(&client, &room.room_id).await;

    let connected = run_until(&mut rtc, &socket, Duration::from_secs(10), is_connected);

    assert!(connected, "ICE never reported a usable path");
}

/// Also requires ffmpeg, which stands in for a real encoder over WHIP.
#[tokio::test]
#[ignore = "needs a running server and ffmpeg"]
async fn viewer_receives_media_from_a_publisher() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let room = server.create_live_room(&client).await;
    let (mut rtc, socket) = server.connect_viewer(&client, &room.room_id).await;

    assert!(
        run_until(&mut rtc, &socket, Duration::from_secs(10), is_connected),
        "ICE never reported a usable path"
    );

    let _publisher = Publisher::spawn(&server, &room.stream_key);

    let got_media = run_until(&mut rtc, &socket, Duration::from_secs(20), |event| {
        matches!(event, Event::MediaData(_))
    });

    assert!(got_media, "no media arrived after the publisher started");
}

struct Publisher(Child);

impl Publisher {
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
                &format!("http://{}:{}/whip", server.host, server.sdp_port),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn ffmpeg");

        Publisher(child)
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
