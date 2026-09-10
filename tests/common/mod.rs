//! Talks to the server over HTTP and UDP only, so it links nothing from the
//! binary and can point at a remote host.

use std::{
    io::ErrorKind,
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    time::{Duration, Instant},
};

use serde::Deserialize;
use serde_json::json;
use str0m::{
    Candidate, Event, IceConnectionState, Input, Output, Rtc,
    change::{SdpAnswer, SdpOffer},
    channel::ChannelId,
    net::{Protocol, Receive},
};

/// A shell variable wins over `.env`, so overriding for a remote server works.
pub struct Server {
    pub host: String,
    pub sdp_port: u16,
    pub control_port: u16,
    pub api_key: String,
}

impl Default for Server {
    fn default() -> Self {
        dotenvy::dotenv().ok();

        let env =
            |key: &str, fallback: &str| std::env::var(key).unwrap_or_else(|_| fallback.to_string());

        Server {
            host: env("STREAM_HOST", "127.0.0.1"),
            sdp_port: env("STREAM_SDP_PORT", "8443").parse().expect("sdp port"),
            control_port: env("STREAM_CONTROL_PORT", "8080")
                .parse()
                .expect("control port"),
            api_key: std::env::var("API_KEY")
                .expect("API_KEY is not set in .env or the environment"),
        }
    }
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct CreatedRoom {
    pub room_id: String,
    pub stream_key: String,
}

impl Server {
    fn sdp_url(&self, path: &str) -> String {
        format!("http://{}:{}{}", self.host, self.sdp_port, path)
    }

    fn control_url(&self, path: &str) -> String {
        format!("http://{}:{}{}", self.host, self.control_port, path)
    }

    /// Viewers get 404 until the control plane promotes the room to `Live`.
    pub async fn create_live_room(&self, client: &reqwest::Client) -> CreatedRoom {
        let room: CreatedRoom = client
            .post(self.control_url("/rooms"))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .expect("POST /rooms")
            .error_for_status()
            .expect("POST /rooms status")
            .json()
            .await
            .expect("room json");

        client
            .patch(self.control_url(&format!("/rooms/{}", room.room_id)))
            .bearer_auth(&self.api_key)
            .json(&json!({ "state": "Live" }))
            .send()
            .await
            .expect("PATCH room")
            .error_for_status()
            .expect("PATCH room status");

        room
    }

    /// The offer carries only a data channel. The server adds media later by
    /// sending its own offer over that channel.
    pub async fn connect_viewer(
        &self,
        client: &reqwest::Client,
        room_id: &str,
    ) -> (Rtc, UdpSocket) {
        let socket = UdpSocket::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
            .expect("bind viewer socket");
        let local = socket.local_addr().expect("viewer local address");

        let mut rtc = Rtc::new(Instant::now());
        rtc.add_local_candidate(Candidate::host(local, "udp").expect("host candidate"))
            .expect("add local candidate");

        let mut change = rtc.sdp_api();
        change.add_channel("data".into());
        let (offer, pending) = change.apply().expect("offer has changes");

        let answer: SdpAnswer = client
            .post(self.sdp_url("/offer"))
            .json(&json!({
                "type": "offer",
                "sdp": offer.to_sdp_string(),
                "room_id": room_id,
            }))
            .send()
            .await
            .expect("POST /offer")
            .error_for_status()
            .expect("POST /offer status")
            .json()
            .await
            .expect("answer json");

        rtc.sdp_api()
            .accept_answer(pending, answer)
            .expect("accept answer");

        (rtc, socket)
    }
}

/// Answers the offers the server sends over the data channel, which is the only
/// way a viewer ever receives media: its own offer asks for no tracks.
pub fn run_until(
    rtc: &mut Rtc,
    socket: &UdpSocket,
    deadline: Duration,
    mut stop: impl FnMut(&Event) -> bool,
) -> bool {
    let give_up_at = Instant::now() + deadline;
    let mut buf = vec![0u8; 2000];
    let mut channel: Option<ChannelId> = None;

    while Instant::now() < give_up_at {
        let mut offers = Vec::new();

        let timeout = loop {
            match rtc.poll_output().expect("poll_output") {
                Output::Timeout(at) => break at,
                Output::Transmit(t) => {
                    socket.send_to(&t.contents, t.destination).expect("send_to");
                }
                Output::Event(event) => {
                    match &event {
                        Event::ChannelOpen(id, _) => channel = Some(*id),
                        Event::ChannelData(data) => {
                            if let Ok(offer) = serde_json::from_slice::<SdpOffer>(&data.data) {
                                offers.push(offer);
                            }
                        }
                        _ => (),
                    }

                    if stop(&event) {
                        return true;
                    }
                }
            }
        };

        for offer in offers {
            let answer = rtc.sdp_api().accept_offer(offer).expect("accept offer");
            let json = serde_json::to_string(&answer).expect("answer json");

            let mut channel = channel
                .and_then(|id| rtc.channel(id))
                .expect("data channel open");

            channel.write(false, json.as_bytes()).expect("write answer");
        }

        // Capped, or a silent server holds the test for as long as str0m asked to sleep.
        let wait = timeout
            .saturating_duration_since(Instant::now())
            .min(give_up_at.saturating_duration_since(Instant::now()));

        if wait.is_zero() {
            rtc.handle_input(Input::Timeout(Instant::now()))
                .expect("handle timeout");
            continue;
        }

        socket.set_read_timeout(Some(wait)).expect("read timeout");

        let input = match socket.recv_from(&mut buf) {
            Ok((n, source)) => Input::Receive(
                Instant::now(),
                Receive {
                    proto: Protocol::Udp,
                    source,
                    destination: socket.local_addr().expect("local address"),
                    contents: buf[..n].try_into().expect("datagram contents"),
                },
            ),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                Input::Timeout(Instant::now())
            }
            Err(e) => panic!("recv_from: {e}"),
        };

        rtc.handle_input(input).expect("handle input");
    }

    false
}

pub fn is_connected(event: &Event) -> bool {
    matches!(
        event,
        Event::IceConnectionStateChange(
            IceConnectionState::Connected | IceConnectionState::Completed
        )
    )
}
