//! Talks to the server over HTTP and UDP only, so it links nothing from the
//! binary and can point at a remote host.
#![allow(dead_code)]

use std::{
    io::ErrorKind,
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket},
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use str0m::{
    Candidate, Event, IceConnectionState, Input, Output, Rtc,
    change::{SdpAnswer, SdpOffer},
    channel::ChannelId,
    media::{Direction, MediaKind, Simulcast, SimulcastLayer},
    net::{Protocol, Receive},
};

pub struct Server {
    pub host: String,
    pub sdp_port: u16,
    pub control_port: u16,
    pub api_key: String,
}

/// One server per test binary. Rooms carry random ids, so tests never collide
/// and there is no reason to pay for a process each.
static SHARED: OnceLock<Server> = OnceLock::new();

impl Default for Server {
    fn default() -> Self {
        let server = SHARED.get_or_init(spawn_server);

        Server {
            host: server.host.clone(),
            sdp_port: server.sdp_port,
            control_port: server.control_port,
            api_key: server.api_key.clone(),
        }
    }
}

fn spawn_server() -> Server {
    let sdp_port = free_port();
    let control_port = free_port();
    let media_port = free_port();
    let api_key = "integration-test-api-key".to_string();

    // The server reads config.toml from its working directory, so give it one
    // of its own rather than picking up the repository's.
    let dir = std::env::temp_dir().join(format!("stream-it-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "[server]\n\
             public_ip = \"127.0.0.1\"\n\
             media_port = {media_port}\n\
             https_sdp_server_port = {sdp_port}\n\
             http_rest_server_port = {control_port}\n"
        ),
    )
    .expect("write config");

    let log = dir.join("server.log");

    // PR_SET_PDEATHSIG fires when the *thread* that spawned the child exits, not
    // the process, and cargo gives every test its own thread. Spawn from a
    // thread that lives as long as the test binary instead, so the server dies
    // with it rather than with whichever test happened to start it.
    let spawn_dir = dir.clone();
    let spawn_key = api_key.clone();
    thread::spawn(move || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_stream"));
        command
            .current_dir(&spawn_dir)
            .env("API_KEY", &spawn_key)
            .stdout(Stdio::from(std::fs::File::create(log).expect("server log")))
            .stderr(Stdio::null());

        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }

        let mut child = command.spawn().expect("spawn server");
        let _ = child.wait();
    });

    let server = Server {
        host: "127.0.0.1".to_string(),
        sdp_port,
        control_port,
        api_key,
    };

    wait_until_listening(control_port);

    server
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind for a free port")
        .local_addr()
        .expect("local address")
        .port()
}

fn wait_until_listening(port: u16) {
    let give_up_at = Instant::now() + Duration::from_secs(10);

    while Instant::now() < give_up_at {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }

    panic!("server never started listening on {port}");
}

#[derive(Deserialize)]
pub struct CreatedRoom {
    pub room_id: String,
    pub stream_key: String,
}

#[derive(Deserialize, Debug, PartialEq)]
pub struct RoomView {
    pub views: usize,
    pub state: String,
}

impl Server {
    pub fn sdp_url(&self, path: &str) -> String {
        format!("http://{}:{}{}", self.host, self.sdp_port, path)
    }

    pub fn control_url(&self, path: &str) -> String {
        format!("http://{}:{}{}", self.host, self.control_port, path)
    }

    pub async fn create_room(&self, client: &reqwest::Client) -> CreatedRoom {
        client
            .post(self.control_url("/rooms"))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .expect("POST /rooms")
            .error_for_status()
            .expect("POST /rooms status")
            .json()
            .await
            .expect("room json")
    }

    /// Viewers get 404 until the control plane promotes the room to `Live`.
    pub async fn set_live(&self, client: &reqwest::Client, room_id: &str) {
        client
            .patch(self.control_url(&format!("/rooms/{room_id}")))
            .bearer_auth(&self.api_key)
            .json(&json!({ "state": "Live" }))
            .send()
            .await
            .expect("PATCH room")
            .error_for_status()
            .expect("PATCH room status");
    }

    pub async fn set_idle(&self, client: &reqwest::Client, room_id: &str) -> StatusCode {
        client
            .patch(self.control_url(&format!("/rooms/{room_id}")))
            .bearer_auth(&self.api_key)
            .json(&json!({ "state": "Idle" }))
            .send()
            .await
            .expect("PATCH room")
            .status()
    }

    pub async fn room(&self, client: &reqwest::Client, room_id: &str) -> RoomView {
        client
            .get(self.control_url(&format!("/rooms/{room_id}")))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .expect("GET room")
            .error_for_status()
            .expect("GET room status")
            .json()
            .await
            .expect("room json")
    }

    /// `location` is the path the WHIP response handed back.
    pub async fn delete_session(
        &self,
        client: &reqwest::Client,
        stream_key: &str,
        location: &str,
    ) -> StatusCode {
        client
            .delete(self.sdp_url(location))
            .bearer_auth(stream_key)
            .send()
            .await
            .expect("DELETE session")
            .status()
    }

    /// Returns the replacement key. The old one stops working at the same time.
    pub async fn reissue_stream_key(&self, client: &reqwest::Client, room_id: &str) -> String {
        let body: Value = client
            .post(self.control_url(&format!("/rooms/{room_id}/stream-key")))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .expect("POST stream-key")
            .error_for_status()
            .expect("POST stream-key status")
            .json()
            .await
            .expect("stream key json");

        body["stream_key"]
            .as_str()
            .expect("stream key is text")
            .to_string()
    }

    pub async fn create_live_room(&self, client: &reqwest::Client) -> CreatedRoom {
        let room = self.create_room(client).await;
        self.set_live(client, &room.room_id).await;
        room
    }

    /// Declares simulcast without ever sending RTP. The server builds its layer
    /// list from the SDP alone, which is enough to exercise layer selection.
    /// ffmpeg's WHIP muxer cannot send simulcast, so the alternative is OBS by
    /// hand.
    pub async fn connect_publisher(
        &self,
        client: &reqwest::Client,
        stream_key: &str,
        rids: &[&str],
    ) -> Peer {
        self.try_connect_publisher(client, stream_key, rids)
            .await
            .expect("POST /whip status")
    }

    pub async fn try_connect_publisher(
        &self,
        client: &reqwest::Client,
        stream_key: &str,
        rids: &[&str],
    ) -> Result<Peer, StatusCode> {
        let (mut rtc, socket) = new_peer();

        let mut simulcast = Simulcast::new();
        for rid in rids {
            simulcast.add_send_layer(SimulcastLayer::new(rid));
        }

        let mut change = rtc.sdp_api();
        change.add_media(
            MediaKind::Video,
            Direction::SendOnly,
            None,
            None,
            Some(simulcast),
        );
        let (offer, pending) = change.apply().expect("offer has changes");

        let response = client
            .post(self.sdp_url("/whip"))
            .bearer_auth(stream_key)
            .header("Content-Type", "application/sdp")
            .body(offer.to_sdp_string())
            .send()
            .await
            .expect("POST /whip");

        if !response.status().is_success() {
            return Err(response.status());
        }

        let location = response
            .headers()
            .get("location")
            .expect("Location header")
            .to_str()
            .expect("Location is text")
            .to_string();

        let body = response.text().await.expect("answer body");

        rtc.sdp_api()
            .accept_answer(
                pending,
                SdpAnswer::from_sdp_string(&body).expect("parse answer"),
            )
            .expect("accept answer");

        let mut peer = Peer::new(rtc, socket);
        peer.session_location = Some(location);
        Ok(peer)
    }

    /// The offer carries only a data channel. The server adds media later by
    /// sending its own offer over that channel.
    pub async fn connect_viewer(&self, client: &reqwest::Client, room_id: &str) -> Peer {
        self.try_connect_viewer(client, room_id)
            .await
            .expect("POST /offer status")
    }

    pub async fn try_connect_viewer(
        &self,
        client: &reqwest::Client,
        room_id: &str,
    ) -> Result<Peer, StatusCode> {
        let (mut rtc, socket) = new_peer();

        let mut change = rtc.sdp_api();
        change.add_channel("data".into());
        let (offer, pending) = change.apply().expect("offer has changes");

        let response = client
            .post(self.sdp_url("/offer"))
            .json(&json!({
                "type": "offer",
                "sdp": offer.to_sdp_string(),
                "room_id": room_id,
            }))
            .send()
            .await
            .expect("POST /offer");

        if !response.status().is_success() {
            return Err(response.status());
        }

        let answer: SdpAnswer = response.json().await.expect("answer json");

        rtc.sdp_api()
            .accept_answer(pending, answer)
            .expect("accept answer");

        Ok(Peer::new(rtc, socket))
    }
}

fn new_peer() -> (Rtc, UdpSocket) {
    let socket =
        UdpSocket::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0)).expect("bind peer socket");
    let local = socket.local_addr().expect("peer local address");

    let mut rtc = Rtc::new(Instant::now());
    rtc.add_local_candidate(Candidate::host(local, "udp").expect("host candidate"))
        .expect("add local candidate");

    (rtc, socket)
}

pub struct Peer {
    rtc: Rtc,
    socket: UdpSocket,
    channel: Option<ChannelId>,
    /// Set for publishers: the WHIP session resource to delete.
    pub session_location: Option<String>,
}

impl Peer {
    fn new(rtc: Rtc, socket: UdpSocket) -> Self {
        Peer {
            rtc,
            socket,
            channel: None,
            session_location: None,
        }
    }

    /// Answers the offers the server sends over the data channel, which is the
    /// only way a viewer ever receives media: its own offer asks for no tracks.
    pub fn run_until(&mut self, deadline: Duration, mut stop: impl FnMut(&Event) -> bool) -> bool {
        let give_up_at = Instant::now() + deadline;
        let mut buf = vec![0u8; 2000];

        while Instant::now() < give_up_at {
            let mut offers = Vec::new();

            let timeout = loop {
                match self.rtc.poll_output().expect("poll_output") {
                    Output::Timeout(at) => break at,
                    Output::Transmit(t) => {
                        self.socket
                            .send_to(&t.contents, t.destination)
                            .expect("send_to");
                    }
                    Output::Event(event) => {
                        match &event {
                            Event::ChannelOpen(id, _) => self.channel = Some(*id),
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
                let answer = self
                    .rtc
                    .sdp_api()
                    .accept_offer(offer)
                    .expect("accept offer");

                self.send(&serde_json::to_value(&answer).expect("answer value"));
            }

            // Capped, or a silent server holds the test for as long as str0m asked to sleep.
            let wait = timeout
                .saturating_duration_since(Instant::now())
                .min(give_up_at.saturating_duration_since(Instant::now()));

            if wait.is_zero() {
                self.rtc
                    .handle_input(Input::Timeout(Instant::now()))
                    .expect("handle timeout");
                continue;
            }

            self.socket
                .set_read_timeout(Some(wait))
                .expect("read timeout");

            let input = match self.socket.recv_from(&mut buf) {
                Ok((n, source)) => Input::Receive(
                    Instant::now(),
                    Receive {
                        proto: Protocol::Udp,
                        source,
                        destination: self.socket.local_addr().expect("local address"),
                        contents: buf[..n].try_into().expect("datagram contents"),
                    },
                ),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    Input::Timeout(Instant::now())
                }
                Err(e) => panic!("recv_from: {e}"),
            };

            self.rtc.handle_input(input).expect("handle input");
        }

        false
    }

    pub fn send(&mut self, payload: &Value) {
        let id = self.channel.expect("data channel open");
        let json = serde_json::to_string(payload).expect("payload json");

        self.rtc
            .channel(id)
            .expect("channel")
            .write(false, json.as_bytes())
            .expect("write");
    }

    /// Drives the peer on its own thread so it stays connected while the test
    /// works with another one. Stops when dropped.
    pub fn detach(mut self) -> Detached {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();

        let handle = thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                self.run_until(Duration::from_millis(200), |_| false);
            }
        });

        Detached {
            stop,
            handle: Some(handle),
        }
    }
}

pub struct Detached {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Detached {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn is_connected(event: &Event) -> bool {
    matches!(
        event,
        Event::IceConnectionStateChange(
            IceConnectionState::Connected | IceConnectionState::Completed
        )
    )
}

/// Server to client data channel messages are tagged objects, so a test can
/// pick out the one it is waiting for.
pub fn dc_message(event: &Event, tag: &str) -> Option<Value> {
    let Event::ChannelData(data) = event else {
        return None;
    };

    let value: Value = serde_json::from_slice(&data.data).ok()?;

    (value.get("type")?.as_str()? == tag).then_some(value)
}
