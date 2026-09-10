mod common;

use std::time::Duration;

use common::{RoomView, Server, is_connected};
use reqwest::StatusCode;

const CONNECT: Duration = Duration::from_secs(10);

/// A room with a connected publisher and viewer, and no simulcast: these tests
/// only care about who is attached, not about layers.
async fn live_room(
    server: &Server,
    client: &reqwest::Client,
) -> (common::CreatedRoom, common::Peer, common::Peer) {
    let room = server.create_room(client).await;

    let mut publisher = server
        .connect_publisher(client, &room.stream_key, &["h"])
        .await;
    assert!(
        publisher.run_until(CONNECT, is_connected),
        "publisher connect"
    );

    server.set_live(client, &room.room_id).await;

    let mut viewer = server.connect_viewer(client, &room.room_id).await;
    assert!(viewer.run_until(CONNECT, is_connected), "viewer connect");

    (room, publisher, viewer)
}

#[tokio::test]
async fn ending_the_whip_session_resets_the_room() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let (room, publisher, _viewer) = live_room(&server, &client).await;
    let location = publisher.session_location.clone().expect("Location header");

    assert_eq!(
        server
            .delete_session(&client, &room.stream_key, &location)
            .await,
        StatusCode::NO_CONTENT
    );

    assert_eq!(
        server.room(&client, &room.room_id).await,
        RoomView {
            views: 0,
            state: "Idle".to_string()
        },
        "the room drops its viewers along with the streamer"
    );
}

#[tokio::test]
async fn going_idle_evicts_everyone() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let (room, publisher, _viewer) = live_room(&server, &client).await;
    let location = publisher.session_location.clone().expect("Location header");

    assert_eq!(
        server.set_idle(&client, &room.room_id).await,
        StatusCode::OK
    );

    assert_eq!(
        server.room(&client, &room.room_id).await,
        RoomView {
            views: 0,
            state: "Idle".to_string()
        }
    );

    assert_eq!(
        server
            .delete_session(&client, &room.stream_key, &location)
            .await,
        StatusCode::NOT_FOUND,
        "the session is gone, not just the state"
    );
}

#[tokio::test]
async fn a_viewer_is_refused_until_the_room_goes_live() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let room = server.create_room(&client).await;

    assert_eq!(
        server
            .try_connect_viewer(&client, &room.room_id)
            .await
            .err(),
        Some(StatusCode::NOT_FOUND),
        "idle room"
    );

    let mut publisher = server
        .connect_publisher(&client, &room.stream_key, &["h"])
        .await;
    assert!(
        publisher.run_until(CONNECT, is_connected),
        "publisher connect"
    );

    assert_eq!(
        server
            .try_connect_viewer(&client, &room.room_id)
            .await
            .err(),
        Some(StatusCode::NOT_FOUND),
        "preview room"
    );

    server.set_live(&client, &room.room_id).await;

    let mut viewer = server.connect_viewer(&client, &room.room_id).await;
    assert!(viewer.run_until(CONNECT, is_connected), "viewer connect");
}
