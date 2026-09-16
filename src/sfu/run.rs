use std::{
    net::{SocketAddr, UdpSocket},
    rc::Rc,
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};

use str0m::Input;
use tracing::{debug, error};

use crate::{
    config::tuning,
    metrics::{self, Gauges, Phase},
    sfu::{error::SfuResult, socket::read_socket_input},
    types::{
        ClientId, ClientRole, LayerMode, LinkState, PollResult, RelayPotential, RelayStatus,
        RoomState, Rooms, SfuMessage, TrackIn, TrackOut, TrackOutState, UploadProbeResult,
    },
};

const MAX_SOCKET_READ_COUNT: u64 = 100;
const P2P_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// `advertised_addr` is the address handed to clients in ICE candidates, which
/// is not the bind address: the socket listens on every interface so a
/// container can receive, but str0m matches an incoming datagram to a local
/// candidate by its destination, and `0.0.0.0` matches none of them.
pub fn run(
    rx: Receiver<SfuMessage>,
    socket: UdpSocket,
    advertised_addr: SocketAddr,
) -> SfuResult<()> {
    let mut buf: Vec<u8> = vec![0; 2000];
    let mut rooms = Rooms::new();
    let mut woken_for: Option<Instant> = None;

    loop {
        let _lap = metrics::time(Phase::Lap);
        metrics::lap();

        if let Some(deadline) = woken_for {
            metrics::lap_lateness(Instant::now().saturating_duration_since(deadline));
        }

        let mut handled = 0;
        let control_drain = metrics::time(Phase::ControlDrain);

        while let Ok(message) = rx.try_recv() {
            handled += 1;

            match message {
                SfuMessage::RegisterClient {
                    rtc,
                    role,
                    room_id,
                    session_id,
                    reply,
                } => rooms.register_client(*rtc, role, room_id, session_id, reply),
                SfuMessage::CreateRoom { reply } => rooms.create(reply),
                SfuMessage::GetRoom { room_id, reply } => rooms.get_room(room_id, reply),
                SfuMessage::UpdateRoomState {
                    room_id,
                    state,
                    reply,
                } => rooms.update_state(room_id, state, reply),
                SfuMessage::DeleteRoom { room_id, reply } => rooms.delete(room_id, reply),
                SfuMessage::ResolveStreamKey {
                    hashed_stream_key,
                    reply,
                } => rooms.resolve_stream_key(hashed_stream_key, reply),
                SfuMessage::TerminateSession {
                    room_id,
                    session_id,
                    reply,
                } => rooms.terminate_session(room_id, session_id, reply),
                SfuMessage::ReissueStreamKey { room_id, reply } => {
                    rooms.reissue_stream_key(room_id, reply)
                }
            };
        }

        drop(control_drain);
        metrics::control_messages(handled);

        let mut timeout = Instant::now() + Duration::from_millis(100);

        for room in rooms.values_mut() {
            if matches!(room.state, RoomState::Idle) && room.clients.is_empty() {
                metrics::room_skipped();
                continue;
            };

            metrics::room_visited();
            let _room = metrics::time(Phase::RoomIteration);

            let mut to_remove = Vec::new();
            let mut new_tracks: Vec<Rc<TrackIn>> = Vec::new();
            let mut media_datas = Vec::new();
            let mut keyframe_requests = Vec::new();
            let mut p2p_sdps = Vec::new();
            let mut connected_relays = Vec::new();
            let mut disconnected_relays = Vec::new();
            let mut should_reevaluate = false;

            let mut fallback_leafs = Vec::new();
            let mut needs_init = false;

            let client_tick = metrics::time(Phase::ClientTick);

            for client in room.clients.values_mut() {
                match client.tick(
                    &socket,
                    &mut new_tracks,
                    &mut media_datas,
                    &mut keyframe_requests,
                    &mut p2p_sdps,
                    &mut connected_relays,
                    &mut disconnected_relays,
                    &mut should_reevaluate,
                ) {
                    Ok(PollResult::Timeout(v)) => timeout = timeout.min(v),
                    Ok(PollResult::Disconnected) => {
                        metrics::client_disconnected();

                        if client.role == ClientRole::Streamer {
                            needs_init = true;
                        }

                        match &client.relay_status {
                            Some(RelayStatus::Relay { leaf, .. }) => {
                                fallback_leafs.push(*leaf);
                            }
                            Some(RelayStatus::Leaf { relay, .. }) => {
                                disconnected_relays.push(*relay);
                            }
                            _ => (),
                        }
                        to_remove.push(client.id);
                    }
                    Err(e) => {
                        metrics::client_tick_error();
                        to_remove.push(client.id);
                        error!("client tick failed: {}", e);
                    }
                };
            }

            drop(client_tick);

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
                    .values_mut()
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

            for id in to_remove.iter() {
                room.clients.remove(id);
            }

            for leaf_id in fallback_leafs {
                if let Some(leaf) = room.clients.get_mut(&leaf_id)
                    && let Ok(leaf_keyframe_requests) = leaf.demote_leaf()
                {
                    for request in leaf_keyframe_requests {
                        keyframe_requests.push(request);
                    }
                };
            }

            let media_fanout = metrics::time(Phase::MediaFanout);

            for c in room
                .clients
                .values_mut()
                .filter(|c| c.role == ClientRole::Viewer)
            {
                if matches!(
                    c.relay_status,
                    Some(RelayStatus::Leaf {
                        link_state: LinkState::Connected,
                        ..
                    })
                ) {
                    c.count_relay_saved(&media_datas);
                    continue;
                }

                if let Err(e) = c.handle_media_datas(&media_datas) {
                    error!("handle_media_datas failed: {}", e);
                };
            }

            drop(media_fanout);

            if let Some(streamer) = room
                .clients
                .values_mut()
                .find(|c| c.role == ClientRole::Streamer)
                && let Err(e) = streamer.handle_keyframe_requests(keyframe_requests)
            {
                error!("handle_keyframe_requests failed: {}", e);
            };

            for offer in p2p_sdps {
                let (target_id, payload) = offer;

                if let Some(target) = room.clients.get_mut(&target_id)
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
                if let Some(relay) = room.clients.get_mut(&relay_id)
                    && let Err(e) = relay.demote_relay()
                {
                    error!("demote relay failed error={e}")
                };
            }

            let relay_policy = metrics::time(Phase::RelayPolicy);

            for c in room.clients.values_mut().filter(|c| {
                matches!(c.role, ClientRole::Viewer)
                    && !c.perf.is_empty()
                    && c.relay_status.is_none()
            }) {
                match c.available_upload {
                    Some(UploadProbeResult::Failed { at }) => {
                        if at.elapsed() > tuning().relay.probe_interval() && c.is_perf_healthy() {
                            c.probe_available_upload()
                        }
                    }
                    None => {
                        if c.is_perf_healthy() {
                            c.probe_available_upload();
                        }
                    }
                    Some(UploadProbeResult::Probing { probed_at })
                        if probed_at.elapsed() > tuning().relay.probe_timeout() =>
                    {
                        c.available_upload = Some(UploadProbeResult::Failed { at: Instant::now() });
                    }

                    _ => (),
                }
            }

            let relay_ids: Vec<ClientId> = room
                .clients
                .values()
                .filter_map(|c| {
                    if c.relay_potential() == RelayPotential::Qualified
                        && c.relay_status.is_none()
                        && c.connected_at.elapsed() >= tuning().relay.min_connection_age()
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

            for relay_id in connected_relays {
                if let Some(relay) = room.clients.get_mut(&relay_id)
                    && let Some(RelayStatus::Relay {
                        p2p_connected_at, ..
                    }) = &mut relay.relay_status
                {
                    *p2p_connected_at = Some(Instant::now());
                };
            }

            let mut leaf_relay_ids_to_demote: Vec<(ClientId, ClientId)> = room
                .clients
                .values()
                .filter_map(|c| {
                    let Some(RelayStatus::Relay {
                        leaf,
                        p2p_connected_at,
                    }) = c.relay_status
                    else {
                        return None;
                    };

                    let relay_outgoing_kbps = c.relay_outgoing_kbps? as u64;

                    let relay_outgoing_cutoff_kbps = c
                        .tracks_out
                        .iter()
                        .filter_map(|t| {
                            let chosen_rid = t.chosen_rid?;

                            t.track_in
                                .upgrade()?
                                .available_simulcast_layers
                                .iter()
                                .find(|layer| layer.rid == chosen_rid)?
                                .estimate_bps()
                                .map(|bps| bps / 1000)
                        })
                        .sum::<u64>()
                        .max(tuning().relay.min_outgoing_kbps as u64)
                        * (100 - tuning().relay.traffic_headroom_percent)
                        / 100;

                    if p2p_connected_at?.elapsed() < tuning().relay.min_p2p_connection_age() {
                        return None;
                    };

                    if relay_outgoing_kbps < relay_outgoing_cutoff_kbps || !c.is_perf_healthy() {
                        return Some((c.id, leaf));
                    };

                    None
                })
                .collect();

            for client in room.clients.values() {
                if let Some(RelayStatus::Leaf {
                    relay,
                    link_state: LinkState::Connecting { at },
                }) = client.relay_status
                    && at.elapsed() >= P2P_CONNECT_TIMEOUT
                {
                    leaf_relay_ids_to_demote.push((relay, client.id));
                }
            }

            for (relay_id, leaf_id) in leaf_relay_ids_to_demote {
                room.demote(relay_id, leaf_id);
            }

            drop(relay_policy);

            if needs_init {
                room.init();
            }
        }

        metrics::gauges(count_gauges(&rooms));

        woken_for = Some(timeout);

        let timeout_duration = (timeout - Instant::now()).max(Duration::from_millis(1));
        let mut socket_read_count = 0;

        socket
            .set_read_timeout(Some(timeout_duration))
            .expect("setting socket read timeout");

        if let Err(e) = socket.set_nonblocking(false) {
            error!("socket set_nonblocking error={e}");
        } else {
            if let Ok(Some(input)) = read_socket_input(&socket, &mut buf, advertised_addr) {
                route_socket_input(&mut rooms, input);
                socket_read_count += 1;
            }
        }

        if let Err(e) = socket.set_nonblocking(true) {
            error!("socket set_nonblocking error={e}");
        } else {
            loop {
                if socket_read_count > MAX_SOCKET_READ_COUNT {
                    break;
                }

                match read_socket_input(&socket, &mut buf, advertised_addr) {
                    Ok(Some(input)) => {
                        route_socket_input(&mut rooms, input);
                        socket_read_count += 1;
                    }
                    _ => break,
                }
            }
        };

        if socket_read_count > MAX_SOCKET_READ_COUNT {
            metrics::socket_drain_full();
        }

        let now = Instant::now();
        for room in rooms.values_mut() {
            for client in room.clients.values_mut() {
                client.handle_input(Input::Timeout(now));
            }
        }
    }
}

/// Recounted every lap rather than tracked by increments, because clients leave
/// through several paths and a missed decrement would never correct itself.
fn count_gauges(rooms: &Rooms) -> Gauges {
    let mut gauges = Gauges {
        rooms: rooms.len() as u64,
        clients: 0,
        viewers: 0,
        relays: 0,
        connected_leaves: 0,
    };

    for room in rooms.values() {
        gauges.clients += room.clients.len() as u64;

        for client in room.clients.values() {
            if client.role == ClientRole::Viewer {
                gauges.viewers += 1;
            }

            match client.relay_status {
                Some(RelayStatus::Relay { .. }) => gauges.relays += 1,
                Some(RelayStatus::Leaf {
                    link_state: LinkState::Connected,
                    ..
                }) => gauges.connected_leaves += 1,
                _ => (),
            }
        }
    }

    gauges
}

fn route_socket_input(rooms: &mut Rooms, input: Input<'_>) {
    let client = rooms
        .values_mut()
        .flat_map(|room| room.clients.values_mut())
        .find(|client| client.rtc.accepts(&input));

    if let Some(client) = client {
        client.handle_input(input);
    } else {
        debug!("No client accepts UDP input");
    };
}
