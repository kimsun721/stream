use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Weak};
use std::time::Duration;
use std::{io::ErrorKind, sync::mpsc::Receiver, time::Instant};

use str0m::media::{Direction, MediaAdded, MediaData, MediaKind, Mid};
use str0m::{
    Event, IceConnectionState, Input, Output, Rtc,
    net::{Protocol, Receive},
};
use tracing::{debug, warn};

use crate::types::{
    Client, ClientId, ClientRole, PollResult, RoomId, RoomState, Rooms, TrackIn, TrackInEntry,
    TrackOut, TrackOutState,
};

use crate::utils;

pub fn bind_udp_socket() -> (SocketAddr, UdpSocket) {
    let host_addr = utils::addr::select_host_address();
    let socket = UdpSocket::bind(format!("{host_addr}:0")).expect("binding a random UDP port");
    let addr = socket.local_addr().expect("a local socket address");

    (addr, socket)
}

pub fn run(
    rx: Receiver<(Rtc, ClientRole, RoomId)>,
    socket: UdpSocket,
    rooms_arc: Rooms,
) -> anyhow::Result<()> {
    let mut buf: Vec<u8> = vec![0; 2000];

    loop {
        register_client(&rx, &rooms_arc);

        let mut rooms = rooms_arc.lock().unwrap();
        let mut timeout = Instant::now() + Duration::from_millis(100);

        for (_, room) in rooms.iter_mut() {
            let mut to_remove = Vec::new();
            let mut new_tracks: Vec<Arc<TrackIn>> = Vec::new();

            for (idx, client) in room.clients.iter_mut().enumerate() {
                let mut change = client.rtc.sdp_api();

                for track_out in client.tracks_out.iter_mut() {
                    if track_out.state == TrackOutState::ToOpen && client.cid.is_some() {
                        if let Some(track_in) = track_out.track_in.upgrade() {
                            let stream_id = track_in.origin.to_string();
                            let mid = change.add_media(
                                track_in.kind,
                                Direction::RecvOnly,
                                Some(stream_id),
                                None,
                                None,
                            );

                            track_out.state = TrackOutState::Negotiating(mid);
                        }
                    }
                }

                if !change.has_changes() {
                    continue;
                }

                let Some((offer, pending)) = change.apply() else {
                    warn!("add_media returned None");
                    continue;
                };

                let Some(mut channel) = client.cid.and_then(|id| client.rtc.channel(id)) else {
                    warn!("channel not found");
                    continue;
                };

                let json = serde_json::to_string(&offer)?;

                channel.write(false, json.as_bytes())?;

                client.pending = Some(pending);

                let t = client.poll_output(&socket, &mut new_tracks)?;
                match t {
                    PollResult::Timeout(v) => timeout = timeout.min(v),
                    PollResult::Disconnected => to_remove.push(idx),
                }
            }

            for track in &new_tracks {
                for client in room
                    .clients
                    .iter_mut()
                    .filter(|c| c.role == ClientRole::Viewer)
                {
                    client.tracks_out.push(TrackOut {
                        track_in: Arc::downgrade(&track),
                        state: TrackOutState::ToOpen,
                    });
                }
            }

            for idx in to_remove.iter().rev() {
                room.clients.remove(*idx);
            }
        }

        drop(rooms);

        let timeout_duration = (timeout - Instant::now()).max(Duration::from_millis(1));

        socket
            .set_read_timeout(Some(timeout_duration))
            .expect("setting socket read timeout");

        if let Ok(Some(input)) = read_socket_input(&socket, &mut buf) {
            let mut rooms = rooms_arc.lock().unwrap();
            let client = rooms
                .iter_mut()
                .flat_map(|(_, room)| room.clients.iter_mut())
                .find(|c| c.rtc.accepts(&input));

            if let Some(client) = client {
                client.handle_input(input);
            } else {
                debug!("No client accepts UDP input");
            };
        };
    }
}

fn register_client(rx: &Receiver<(Rtc, ClientRole, RoomId)>, rooms_arc: &Rooms) {
    if let Ok((rtc, role, room_id)) = rx.try_recv() {
        let mut rooms = rooms_arc.lock().unwrap();

        let mut client = Client::new(rtc, role);

        if let Some(room) = rooms.get_mut(&room_id.0) {
            match role {
                ClientRole::Streamer => {
                    if room
                        .clients
                        .iter()
                        .any(|c| matches!(c.role, ClientRole::Streamer))
                    {
                        warn!("Streamer already connected in room {:?}", &room);
                    } else {
                        room.streamer_id = Some(client.id);
                        room.clients.push(client);
                    }
                }
                ClientRole::Viewer => match room.state {
                    RoomState::IDLE | RoomState::PREVIEW => {
                        warn!("Client connected to an {:?} room", room.state);
                    }
                    RoomState::LIVE => {
                        let tracks: Vec<Weak<TrackIn>> = room
                            .clients
                            .iter()
                            .filter(|c| c.role == ClientRole::Streamer)
                            .flat_map(|c| c.tracks_in.iter().map(|t| Arc::downgrade(&t.id)))
                            .collect();

                        for track_in in tracks {
                            client.tracks_out.push(TrackOut {
                                track_in,
                                state: TrackOutState::ToOpen,
                            });
                        }

                        room.clients.push(client);
                    }
                },
            }
        } else {
            warn!("Room does not exist : {:?}", room_id);
        };
    };
}

fn read_socket_input<'a>(
    socket: &UdpSocket,
    buf: &'a mut Vec<u8>,
) -> anyhow::Result<Option<Input<'a>>> {
    buf.resize(2000, 0);
    let input = match socket.recv_from(buf) {
        Ok((n, source)) => {
            buf.truncate(n);
            Some(Input::Receive(
                Instant::now(),
                Receive {
                    proto: Protocol::Udp,
                    source,
                    destination: socket.local_addr()?,
                    contents: buf.as_slice().try_into()?,
                },
            ))
        }

        Err(e) => match e.kind() {
            ErrorKind::WouldBlock | ErrorKind::TimedOut => None,
            _ => return Err(e.into()),
        },
    };
    Ok(input)
}

impl Client {
    fn handle_input(&mut self, input: Input) {
        if !self.rtc.is_alive() {
            return;
        }
        if let Err(e) = self.rtc.handle_input(input) {
            warn!("Client ({:?}) disconnected: {:?}", self.id, e);
            self.rtc.disconnect();
        }
    }

    fn poll_output(
        self: &mut Client,
        socket: &UdpSocket,
        new_tracks: &mut Vec<Arc<TrackIn>>,
    ) -> anyhow::Result<PollResult> {
        let timeout = loop {
            match self.rtc.poll_output()? {
                Output::Timeout(v) => break v,
                Output::Transmit(v) => {
                    socket.send_to(&v.contents, v.destination)?;
                }

                Output::Event(e) => match e {
                    Event::IceConnectionStateChange(v) => {
                        if v == IceConnectionState::Disconnected {
                            self.rtc.disconnect();
                            return Ok(PollResult::Disconnected);
                        }
                    }
                    Event::MediaAdded(m) => {
                        if self.role == ClientRole::Streamer {
                            let track_in = self.handle_media_added(m.mid, m.kind);
                            new_tracks.push(track_in);
                        }
                    }
                    Event::MediaData(data) => {
                        self.handle_media_data(data);
                    }
                    Event::ChannelOpen(cid, _label) => self.cid = Some(cid),
                    _ => {}
                },
            };
        };

        Ok(PollResult::Timeout(timeout))
    }

    fn handle_media_added(&mut self, mid: Mid, kind: MediaKind) -> Arc<TrackIn> {
        let track_in = Arc::new(TrackIn {
            origin: self.id,
            mid,
            kind,
        });

        let track_in_entry = TrackInEntry {
            id: track_in.clone(),
            last_keyframe_request: None,
        };

        self.tracks_in.push(track_in_entry);
        track_in
    }

    fn handle_media_data(&mut self, data: MediaData) {}
}
