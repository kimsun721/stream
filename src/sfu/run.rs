use std::{
    net::UdpSocket,
    sync::{Arc, mpsc::Receiver},
    time::{Duration, Instant},
};

use str0m::Input;
use tracing::{debug, error};

use crate::{
    sfu::{client::register_client, error::SfuResult, socket::read_socket_input},
    types::{
        ClientRole, PollResult, Room, RoomState, Rooms, SfuMessage, TrackIn, TrackOut,
        TrackOutState,
    },
};

pub fn run(rx: Receiver<SfuMessage>, socket: UdpSocket) -> SfuResult<()> {
    let mut buf: Vec<u8> = vec![0; 2000];

    let mut rooms = Rooms::new();

    loop {
        while let Ok(message) = rx.try_recv() {
            match message {
                SfuMessage::RegisterClient { rtc, role, room_id } => {
                    register_client(rtc, role, room_id, &mut rooms);
                }
                SfuMessage::CreateRoom { room_id, reply } => {
                    if rooms.contains_key(&room_id.0) {
                        reply.send(None).ok();
                    } else {
                        rooms.insert(
                            room_id.0,
                            Room {
                                streamer_id: None,
                                clients: vec![],
                                state: RoomState::IDLE,
                            },
                        );

                        reply.send(Some(())).ok();
                    }
                }
                SfuMessage::GetViews { room_id, reply } => {
                    if let Some(room) = rooms.get(&room_id.0) {
                        let views = room
                            .clients
                            .iter()
                            .filter(|c| c.role == ClientRole::Viewer)
                            .count();

                        reply.send(Some(views)).ok();
                    } else {
                        reply.send(None).ok();
                    };
                }
                SfuMessage::UpdateRoomState {
                    room_id,
                    state,
                    reply,
                } => {
                    if let Some(room) = rooms.get_mut(&room_id.0) {
                        room.state = state;

                        reply.send(Some(())).ok();
                    } else {
                        reply.send(None).ok();
                    }
                }
                SfuMessage::DeleteRoom { room_id, reply } => {
                    match rooms.remove(&room_id.0) {
                        Some(_) => reply.send(Some(())).ok(),
                        None => reply.send(None).ok(),
                    };
                }
            }
        }

        let mut timeout = Instant::now() + Duration::from_millis(100);

        for (_, room) in rooms.iter_mut() {
            let mut to_remove = Vec::new();
            let mut new_tracks: Vec<Arc<TrackIn>> = Vec::new();
            let mut media_datas = Vec::new();
            let mut keyframe_requests = Vec::new();

            for (idx, client) in room.clients.iter_mut().enumerate() {
                match client.tick(
                    &socket,
                    &mut new_tracks,
                    &mut media_datas,
                    &mut keyframe_requests,
                ) {
                    Ok(PollResult::Timeout(v)) => timeout = timeout.min(v),
                    Ok(PollResult::Disconnected) => to_remove.push(idx),
                    Err(e) => {
                        to_remove.push(idx);
                        error!("client tick failed: {}", e);
                    }
                };
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

            for c in room
                .clients
                .iter_mut()
                .filter(|c| c.role == ClientRole::Viewer)
            {
                if let Err(e) = c.handle_media_datas(&media_datas) {
                    error!("handle_media_datas failed: {}", e);
                };
            }

            if let Some(streamer) = room
                .clients
                .iter_mut()
                .find(|c| c.role == ClientRole::Streamer)
            {
                if let Err(e) = streamer.handle_keyframe_requests(keyframe_requests) {
                    error!("handle_keyframe_requests failed: {}", e);
                };
            };
        }

        let timeout_duration = (timeout - Instant::now()).max(Duration::from_millis(1));

        socket
            .set_read_timeout(Some(timeout_duration))
            .expect("setting socket read timeout");

        if let Ok(Some(input)) = read_socket_input(&socket, &mut buf) {
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

        let now = Instant::now();
        for (_, room) in rooms.iter_mut() {
            for client in room.clients.iter_mut() {
                client.handle_input(Input::Timeout(now));
            }
        }
    }
}
