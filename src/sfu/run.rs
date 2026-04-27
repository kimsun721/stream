use std::{
    net::UdpSocket,
    sync::{Arc, mpsc::Receiver},
    time::{Duration, Instant},
};

use str0m::{Input, Rtc, media::Direction};
use tracing::{debug, warn};

use crate::{
    sfu::{client::register_client, error::ClientResult, socket::read_socket_input},
    types::{ClientRole, PollResult, RoomId, Rooms, TrackIn, TrackOut, TrackOutState},
};

pub fn run(
    rx: Receiver<(Rtc, ClientRole, RoomId)>,
    socket: UdpSocket,
    rooms_arc: Rooms,
) -> ClientResult<()> {
    let mut buf: Vec<u8> = vec![0; 2000];

    loop {
        register_client(&rx, &rooms_arc);

        let mut rooms = rooms_arc.lock().unwrap();
        let mut timeout = Instant::now() + Duration::from_millis(100);

        for (_, room) in rooms.iter_mut() {
            let mut to_remove = Vec::new();
            let mut new_tracks: Vec<Arc<TrackIn>> = Vec::new();
            let mut media_datas = Vec::new();
            let mut keyframe_requests = Vec::new();

            for (idx, client) in room.clients.iter_mut().enumerate() {
                let mut change = client.rtc.sdp_api();

                for track_out in client.tracks_out.iter_mut() {
                    if track_out.state == TrackOutState::ToOpen && client.cid.is_some() {
                        if let Some(track_in) = track_out.track_in.upgrade() {
                            let stream_id = track_in.origin.to_string();
                            let mid = change.add_media(
                                track_in.kind,
                                Direction::SendOnly,
                                Some(stream_id),
                                None,
                                None,
                            );

                            track_out.state = TrackOutState::Negotiating(mid);
                        }
                    }
                }

                if change.has_changes() {
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
                }

                let t = client.poll_output(
                    &socket,
                    &mut new_tracks,
                    &mut media_datas,
                    &mut keyframe_requests,
                )?;

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

            for c in room
                .clients
                .iter_mut()
                .filter(|c| c.role == ClientRole::Viewer)
            {
                c.handle_media_datas(&media_datas)?;
            }

            if let Some(streamer) = room
                .clients
                .iter_mut()
                .find(|c| c.role == ClientRole::Streamer)
            {
                streamer.handle_keyframe_requests(keyframe_requests)?;
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
