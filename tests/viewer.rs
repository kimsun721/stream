mod common;

use std::time::Duration;

use common::{Server, is_connected, run_until};

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
