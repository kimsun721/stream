use std::{
    collections::hash_map::Entry,
    net::UdpSocket,
    sync::{Arc, mpsc::Receiver},
    time::{Duration, Instant},
};

use axum::http::StatusCode;
use str0m::{Input, Rtc, media::Direction};
use tracing::{debug, error, warn};

use crate::{
    sfu::{
        client::register_client,
        error::{ClientResult, SfuResult},
        socket::read_socket_input,
    },
    types::{
        ClientRole, PollResult, Room, RoomId, RoomState, Rooms, SfuMessage, TrackIn, TrackOut,
        TrackOutState,
    },
};

pub fn run(rx: Receiver<SfuMessage>, socket: UdpSocket, rooms_arc: Rooms) -> SfuResult<()> {
    let mut buf: Vec<u8> = vec![0; 2000];

    loop {
        while let Ok(message) = rx.try_recv() {
            match message {
                SfuMessage::RegisterClient { rtc, role, room_id } => {
                    register_client(rtc, role, room_id, &rooms_arc);
                }
                SfuMessage::CreateRoom { room_id } => {
                    match rooms_arc.lock().unwrap().entry(room_id.0) {
                        Entry::Occupied(_) => {}
                        Entry::Vacant(e) => {
                            e.insert(Room {
                                streamer_id: None,
                                clients: vec![],
                                state: RoomState::IDLE,
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        let mut rooms = rooms_arc.lock().unwrap();
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

        let now = Instant::now();
        let mut rooms = rooms_arc.lock().unwrap();
        for (_, room) in rooms.iter_mut() {
            for client in room.clients.iter_mut() {
                client.handle_input(Input::Timeout(now));
            }
        }
    }
}
