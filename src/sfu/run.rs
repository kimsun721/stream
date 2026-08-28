use std::{
    net::UdpSocket,
    rc::Rc,
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};

use str0m::Input;
use tracing::{debug, error};

use crate::{
    sfu::{error::SfuResult, socket::read_socket_input},
    types::{
        ClientId, ClientRole, LayerMode, LinkState, PollResult, RelayStatus, Rooms, SfuMessage,
        TrackIn, TrackOut, TrackOutState, UploadProbeResult,
    },
};

pub fn run(rx: Receiver<SfuMessage>, socket: UdpSocket) -> SfuResult<()> {
    let mut buf: Vec<u8> = vec![0; 2000];

    let mut rooms = Rooms::new();

    loop {
        while let Ok(message) = rx.try_recv() {
            match message {
                SfuMessage::RegisterClient {
                    rtc,
                    role,
                    room_id,
                    reply,
                } => rooms.register_client(*rtc, role, room_id, reply),
                SfuMessage::CreateRoom { room_id, reply } => rooms.create(room_id, reply),
                SfuMessage::GetViews { room_id, reply } => rooms.get_views(room_id, reply),
                SfuMessage::UpdateRoomState {
                    room_id,
                    state,
                    reply,
                } => rooms.update_state(room_id, state, reply),
                SfuMessage::DeleteRoom { room_id, reply } => rooms.delete(room_id, reply),
            };
        }

        let mut timeout = Instant::now() + Duration::from_millis(100);

        for room in rooms.values_mut() {
            let mut to_remove = Vec::new();
            let mut new_tracks: Vec<Rc<TrackIn>> = Vec::new();
            let mut media_datas = Vec::new();
            let mut keyframe_requests = Vec::new();
            let mut p2p_sdps = Vec::new();
            let mut disconnected_relays = Vec::new();
            let mut should_reevaluate = false;

            let mut fallback_leafs = Vec::new();

            for (idx, client) in room.clients.iter_mut().enumerate() {
                match client.tick(
                    &socket,
                    &mut new_tracks,
                    &mut media_datas,
                    &mut keyframe_requests,
                    &mut p2p_sdps,
                    &mut disconnected_relays,
                    &mut should_reevaluate,
                ) {
                    Ok(PollResult::Timeout(v)) => timeout = timeout.min(v),
                    Ok(PollResult::Disconnected) => {
                        match &client.relay_status {
                            Some(RelayStatus::Relay { leaf }) => {
                                fallback_leafs.push(*leaf);
                            }
                            Some(RelayStatus::Leaf { relay, .. }) => {
                                disconnected_relays.push(*relay);
                            }
                            _ => (),
                        }
                        to_remove.push(idx);
                    }
                    Err(e) => {
                        to_remove.push(idx);
                        error!("client tick failed: {}", e);
                    }
                };
            }

            for track in &new_tracks {
                for client in room.viewers_mut() {
                    client.tracks_out.push(TrackOut {
                        track_in: Rc::downgrade(track),
                        state: TrackOutState::ToOpen,
                        layer_mode: LayerMode::Auto,
                        chosen_rid: track.default_rid(),
                    });
                }
            }

            if !new_tracks.is_empty() {
                for client in room.viewers_mut() {
                    if let Some(desired_bitrate) = client.calc_desired_bitrate() {
                        client.rtc.bwe().set_desired_bitrate(desired_bitrate);
                    }
                }
            }

            if should_reevaluate {
                for client in room
                    .clients
                    .iter_mut()
                    .filter(|c| matches!(c.role, ClientRole::Viewer))
                {
                    let mut is_auto_layer = false;
                    for track_out in client.tracks_out.iter() {
                        if matches!(track_out.layer_mode, LayerMode::Auto) {
                            is_auto_layer = true;
                            break;
                        }
                    }

                    if is_auto_layer {
                        if let Some(desired_bitrate) = client.calc_desired_bitrate() {
                            client.rtc.bwe().set_desired_bitrate(desired_bitrate);
                        }

                        client.reevaluate_auto_layers(&mut keyframe_requests)?;
                    }
                }
            }

            for idx in to_remove.iter().rev() {
                room.clients.remove(*idx);
            }

            for leaf_id in fallback_leafs {
                if let Some(leaf) = room.clients.iter_mut().find(|c| c.id == leaf_id)
                    && let Ok(leaf_keyframe_requests) = leaf.demote_leaf()
                {
                    for request in leaf_keyframe_requests {
                        keyframe_requests.push(request);
                    }
                };
            }

            for c in room.clients.iter_mut().filter(|c| {
                c.role == ClientRole::Viewer
                    && !matches!(
                        c.relay_status,
                        Some(RelayStatus::Leaf {
                            link_state: LinkState::Connected,
                            ..
                        })
                    )
            }) {
                if let Err(e) = c.handle_media_datas(&media_datas) {
                    error!("handle_media_datas failed: {}", e);
                };
            }

            if let Some(streamer) = room
                .clients
                .iter_mut()
                .find(|c| c.role == ClientRole::Streamer)
                && let Err(e) = streamer.handle_keyframe_requests(keyframe_requests)
            {
                error!("handle_keyframe_requests failed: {}", e);
            };

            for offer in p2p_sdps {
                let (target_id, payload) = offer;

                if let Some(target) = room.clients.iter_mut().find(|c| c.id == target_id)
                    && let Some(mut channel) = target.cid.and_then(|id| target.rtc.channel(id))
                {
                    let Ok(json) = serde_json::to_string(&payload) else {
                        error!("serde_json to_string failed: relay_id={target_id}");
                        continue;
                    };

                    if let Err(e) = channel.write(false, json.as_bytes()) {
                        error!("send p2p_offer via dc failed: {e}");
                    };
                }
            }

            for relay_id in disconnected_relays {
                if let Some(relay) = room.clients.iter_mut().find(|c| c.id == relay_id)
                    && let Err(e) = relay.demote_relay()
                {
                    error!("demote relay failed error={e}")
                };
            }

            const MAX_PROBE_PENDING: Duration = Duration::from_secs(60);
            const MIN_PROBE_INTERVAL: Duration = Duration::from_secs(120);

            for c in room.clients.iter_mut().filter(|c| {
                matches!(c.role, ClientRole::Viewer)
                    && !c.perf.is_empty()
                    && c.relay_status.is_none()
            }) {
                match c.available_upload {
                    Some(UploadProbeResult::Failed { at }) => {
                        if at.elapsed() > MIN_PROBE_INTERVAL && c.is_perf_healthy() {
                            c.probe_available_upload()
                        }
                    }
                    None => {
                        if c.is_perf_healthy() {
                            c.probe_available_upload();
                        }
                    }
                    Some(UploadProbeResult::Probing { probed_at })
                        if probed_at.elapsed() > MAX_PROBE_PENDING =>
                    {
                        c.available_upload = Some(UploadProbeResult::Failed { at: Instant::now() });
                    }

                    _ => (),
                }
            }

            const AVAILABLE_UPLOAD_CUTOFF: u32 = 13000;
            const MIN_CONNECTION_AGE: Duration = Duration::from_secs(300);

            let relay_ids: Vec<ClientId> = room
                .clients
                .iter()
                .filter_map(|c| {
                    let Some(UploadProbeResult::Probed {
                        available_upload_kbps,
                    }) = c.available_upload
                    else {
                        return None;
                    };

                    if available_upload_kbps > AVAILABLE_UPLOAD_CUTOFF
                        && c.relay_status.is_none()
                        && c.connected_at.elapsed() >= MIN_CONNECTION_AGE
                        && c.is_perf_healthy()
                    {
                        return Some(c.id);
                    };

                    None
                })
                .collect();

            for relay_id in relay_ids {
                room.promote(relay_id);
            }

            const RELAY_OUTGOING_KBPS_CUTOFF: u32 = 7000;

            let leaf_relay_ids_to_demote: Vec<(ClientId, ClientId)> = room
                .clients
                .iter()
                .filter_map(|c| {
                    let Some(RelayStatus::Relay { leaf }) = c.relay_status else {
                        return None;
                    };

                    let relay_outgoing_kbps = c.relay_outgoing_kbps?;

                    if relay_outgoing_kbps < RELAY_OUTGOING_KBPS_CUTOFF || !c.is_perf_healthy() {
                        return Some((c.id, leaf));
                    };

                    None
                })
                .collect();

            for (relay_id, leaf_id) in leaf_relay_ids_to_demote {
                room.demote(relay_id, leaf_id);
            }
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
        for room in rooms.values_mut() {
            for client in room.clients.iter_mut() {
                client.handle_input(Input::Timeout(now));
            }
        }
    }
}
