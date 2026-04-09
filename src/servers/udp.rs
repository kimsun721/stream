use std::net::{SocketAddr, UdpSocket};
use std::{io::ErrorKind, sync::mpsc::Receiver, time::Instant};

use str0m::{
    Event, IceConnectionState, Input, Output, Rtc,
    net::{Protocol, Receive},
};
use tracing::warn;

use crate::types::{Client, ClientRole, RoomId, RoomState, Rooms};

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
    let mut buf: Vec<u8> = Vec::new();

    loop {
        register_client(&rx, &rooms_arc);

        let mut rooms = rooms_arc.lock().unwrap();

        for (_, room) in rooms.iter_mut() {
            let mut to_remove = Vec::new();

            for (idx, client) in room.clients.iter_mut().enumerate() {
                if poll_client(&mut client.rtc, &socket, &mut buf)? {
                    to_remove.push(idx);
                }
            }

            for idx in to_remove.iter().rev() {
                room.clients.remove(*idx);
            }
        }
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
                        warn!("Streamer already connected in room : {:?}", &room);
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

fn poll_client(rtc: &mut Rtc, socket: &UdpSocket, buf: &mut Vec<u8>) -> anyhow::Result<bool> {
    let timeout = loop {
        match rtc.poll_output()? {
            Output::Timeout(v) => break v,

            Output::Transmit(v) => {
                socket.send_to(&v.contents, v.destination)?;
            }

            Output::Event(v) => {
                if v == Event::IceConnectionStateChange(IceConnectionState::Disconnected) {
                    rtc.disconnect();
                    return Ok(false);
                }
            }
        };
    };

    let timeout = timeout - Instant::now();

    if timeout.is_zero() {
        rtc.handle_input(Input::Timeout(Instant::now()))?;
        return Ok(true);
    }

    socket.set_read_timeout(Some(timeout))?;
    buf.resize(2000, 0);

    let input = match socket.recv_from(buf) {
        Ok((n, source)) => {
            buf.truncate(n);
            Input::Receive(
                Instant::now(),
                Receive {
                    proto: Protocol::Udp,
                    source,
                    destination: socket.local_addr().unwrap(),
                    contents: buf.as_slice().try_into()?,
                },
            )
        }

        Err(e) => match e.kind() {
            ErrorKind::WouldBlock | ErrorKind::TimedOut => Input::Timeout(Instant::now()),
            _ => return Err(e.into()),
        },
    };

    rtc.handle_input(input)?;
    Ok(true)
}
