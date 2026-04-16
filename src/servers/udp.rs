use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;
use std::{io::ErrorKind, sync::mpsc::Receiver, time::Instant};

use str0m::{
    Event, IceConnectionState, Input, Output, Rtc,
    net::{Protocol, Receive},
};
use tracing::{debug, warn};

use crate::types::{Client, ClientRole, PollResult, RoomId, RoomState, Rooms};

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

            for (idx, client) in room.clients.iter_mut().enumerate() {
                let t = poll_client(&mut client.rtc, &socket)?;
                match t {
                    PollResult::Timeout(v) => timeout = timeout.min(v),
                    PollResult::Disconnected => to_remove.push(idx),
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
        let client = Client::new(rtc, role);

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
                        room.clients.push(client);
                    }
                },
            }
        } else {
            warn!("Room does not exist : {:?}", room_id);
        };
    };
}

fn poll_client(rtc: &mut Rtc, socket: &UdpSocket) -> anyhow::Result<PollResult> {
    let timeout = loop {
        match rtc.poll_output()? {
            Output::Timeout(v) => break v,

            Output::Transmit(v) => {
                socket.send_to(&v.contents, v.destination)?;
            }

            Output::Event(v) => {
                if v == Event::IceConnectionStateChange(IceConnectionState::Disconnected) {
                    rtc.disconnect();
                    return Ok(PollResult::Disconnected);
                }
            }
        };
    };

    Ok(PollResult::Timeout(timeout))
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
}
