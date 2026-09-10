mod common;

use std::time::Duration;

use common::{Server, is_connected};
use reqwest::StatusCode;
use serde_json::json;

const CONNECT: Duration = Duration::from_secs(10);
const MISSING_ROOM: &str = "no-such-room";

#[tokio::test]
async fn the_control_plane_requires_the_api_key() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let no_key = client
        .post(server.control_url("/rooms"))
        .send()
        .await
        .expect("POST /rooms");
    assert_eq!(no_key.status(), StatusCode::UNAUTHORIZED, "no key");
    assert!(
        no_key.headers().contains_key("www-authenticate"),
        "a 401 names the scheme it wants"
    );

    let wrong_key = client
        .post(server.control_url("/rooms"))
        .bearer_auth("not-the-api-key")
        .send()
        .await
        .expect("POST /rooms");
    assert_eq!(wrong_key.status(), StatusCode::UNAUTHORIZED, "wrong key");
}

#[tokio::test]
async fn publishing_requires_a_stream_key() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let no_key = client
        .post(server.sdp_url("/whip"))
        .header("Content-Type", "application/sdp")
        .body("v=0\r\n")
        .send()
        .await
        .expect("POST /whip");
    assert_eq!(no_key.status(), StatusCode::UNAUTHORIZED, "no key");

    let wrong_key = client
        .post(server.sdp_url("/whip"))
        .bearer_auth("not-the-stream-key")
        .header("Content-Type", "application/sdp")
        .body("v=0\r\n")
        .send()
        .await
        .expect("POST /whip");
    assert_eq!(wrong_key.status(), StatusCode::UNAUTHORIZED, "wrong key");
}

#[tokio::test]
async fn publishing_rejects_a_body_that_is_not_sdp() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let room = server.create_room(&client).await;

    let response = client
        .post(server.sdp_url("/whip"))
        .bearer_auth(&room.stream_key)
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .expect("POST /whip");

    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn a_room_takes_only_one_streamer() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let room = server.create_room(&client).await;

    let mut publisher = server
        .connect_publisher(&client, &room.stream_key, &["h"])
        .await;
    assert!(
        publisher.run_until(CONNECT, is_connected),
        "publisher connect"
    );

    assert_eq!(
        server
            .try_connect_publisher(&client, &room.stream_key, &["h"])
            .await
            .err(),
        Some(StatusCode::CONFLICT),
        "the stream key is valid, the slot is taken"
    );
}

#[tokio::test]
async fn reissuing_a_stream_key_retires_the_old_one() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let room = server.create_room(&client).await;
    let reissued = server.reissue_stream_key(&client, &room.room_id).await;

    assert_ne!(reissued, room.stream_key);

    assert_eq!(
        server
            .try_connect_publisher(&client, &room.stream_key, &["h"])
            .await
            .err(),
        Some(StatusCode::UNAUTHORIZED),
        "the old key"
    );

    let mut publisher = server.connect_publisher(&client, &reissued, &["h"]).await;
    assert!(
        publisher.run_until(CONNECT, is_connected),
        "the new key connects"
    );
}

#[tokio::test]
async fn a_missing_room_is_not_found() {
    let server = Server::default();
    let client = reqwest::Client::new();

    let get = client
        .get(server.control_url(&format!("/rooms/{MISSING_ROOM}")))
        .bearer_auth(&server.api_key)
        .send()
        .await
        .expect("GET room");
    assert_eq!(get.status(), StatusCode::NOT_FOUND, "get");

    let patch = client
        .patch(server.control_url(&format!("/rooms/{MISSING_ROOM}")))
        .bearer_auth(&server.api_key)
        .json(&json!({ "state": "Live" }))
        .send()
        .await
        .expect("PATCH room");
    assert_eq!(patch.status(), StatusCode::NOT_FOUND, "patch");

    let delete = client
        .delete(server.control_url(&format!("/rooms/{MISSING_ROOM}")))
        .bearer_auth(&server.api_key)
        .send()
        .await
        .expect("DELETE room");
    assert_eq!(delete.status(), StatusCode::NOT_FOUND, "delete");

    let reissue = client
        .post(server.control_url(&format!("/rooms/{MISSING_ROOM}/stream-key")))
        .bearer_auth(&server.api_key)
        .send()
        .await
        .expect("POST stream-key");
    assert_eq!(reissue.status(), StatusCode::NOT_FOUND, "reissue");
}
