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
        ClientId, LayerMode, LinkState, RelayPotential, RelayStatus, RoomState, Rooms, SfuMessage,
        StreamerEffects, TickResult, TrackOut, TrackOutState, UploadProbeResult, ViewerEffects,
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
                SfuMessage::RegisterViewer {
                    rtc,
                    room_id,
                    session_id,
                    reply,
                } => rooms.register_viewer(*rtc, room_id, session_id, reply),
                SfuMessage::RegisterStreamer {
                    rtc,
                    room_id,
                    session_id,
                    reply,
                } => rooms.register_streamer(*rtc, room_id, session_id, reply),
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
            if matches!(room.state, RoomState::Idle)
                && room.viewers.is_empty()
                && room.streamer.is_none()
            {
                metrics::room_skipped();
                continue;
            };

            metrics::room_visited();
            let _room = metrics::time(Phase::RoomIteration);
            let client_tick = metrics::time(Phase::ClientTick);

            let mut to_remove = Vec::new();
            let mut fallback_leafs = Vec::new();
            let mut needs_init = false;

            let mut viewer_effects = ViewerEffects {
                keyframe_requests: Vec::new(),
                p2p_sdps: Vec::new(),
                connected_relays: Vec::new(),
                disconnected_relays: Vec::new(),
            };

            let mut streamer_effects = StreamerEffects {
                new_tracks: Vec::new(),
                media_datas: Vec::new(),
                should_reevaluate: false,
            };

            if let Some(streamer) = room.streamer.as_mut() {
                match streamer.tick(&mut streamer_effects, &socket) {
                    Ok(TickResult::Timeout(v)) => timeout = timeout.min(v),
                    Ok(TickResult::Disconnected) => {
                        metrics::client_disconnected();
                        needs_init = true;
                    }
                    Err(e) => {
                        metrics::client_tick_error();
                        needs_init = true;
                        error!("client tick failed: {}", e);
                    }
                }
            }

            for viewer in room.viewers.values_mut() {
                match viewer.tick(&mut viewer_effects, &socket) {
                    Ok(TickResult::Timeout(v)) => timeout = timeout.min(v),
                    Ok(TickResult::Disconnected) => {
                        metrics::client_disconnected();

                        match &viewer.relay_state.relay_status {
                            Some(RelayStatus::Relay { leaf, .. }) => {
                                fallback_leafs.push(*leaf);
                            }
                            Some(RelayStatus::Leaf { relay, .. }) => {
                                viewer_effects.disconnected_relays.push(*relay);
                            }
                            _ => (),
                        }
                        to_remove.push(viewer.id);
                    }
                    Err(e) => {
                        metrics::client_tick_error();
                        to_remove.push(viewer.id);
                        error!("client tick failed: {}", e);
                    }
                };
            }

            drop(client_tick);

            for track in &streamer_effects.new_tracks {
                for client in room.viewers.values_mut() {
                    client.tracks_out.push(TrackOut {
                        track_in: Rc::downgrade(track),
                        state: TrackOutState::ToOpen,
                        layer_mode: LayerMode::Auto,
                        chosen_rid: track.default_rid(),
                    });
                }
            }

            if !streamer_effects.new_tracks.is_empty() {
                for client in room.viewers.values_mut() {
                    if let Some(desired_bitrate) = client.calc_desired_bitrate() {
                        client.peer.rtc.bwe().set_desired_bitrate(desired_bitrate);
                    }
                }
            }

            if streamer_effects.should_reevaluate {
                for client in room.viewers.values_mut() {
                    let mut is_auto_layer = false;
                    for track_out in client.tracks_out.iter() {
                        if matches!(track_out.layer_mode, LayerMode::Auto) {
                            is_auto_layer = true;
                            break;
                        }
                    }

                    if is_auto_layer {
                        if let Some(desired_bitrate) = client.calc_desired_bitrate() {
                            client.peer.rtc.bwe().set_desired_bitrate(desired_bitrate);
                        }

                        client.reevaluate_auto_layers(&mut viewer_effects.keyframe_requests)?;
                    }
                }
            }

            for id in to_remove.iter() {
                room.viewers.remove(id);
            }

            for leaf_id in fallback_leafs {
                if let Some(leaf) = room.viewers.get_mut(&leaf_id)
                    && let Ok(leaf_keyframe_requests) = leaf.demote_leaf()
                {
                    for request in leaf_keyframe_requests {
                        viewer_effects.keyframe_requests.push(request);
                    }
                };
            }

            let media_fanout = metrics::time(Phase::MediaFanout);

            for c in room.viewers.values_mut() {
                if matches!(
                    c.relay_state.relay_status,
                    Some(RelayStatus::Leaf {
                        link_state: LinkState::Connected,
                        ..
                    })
                ) {
                    c.count_relay_saved(&streamer_effects.media_datas);
                    continue;
                }

                if let Err(e) = c.handle_media_datas(&streamer_effects.media_datas) {
                    error!("handle_media_datas failed: {}", e);
                };
            }

            drop(media_fanout);

            if let Some(streamer) = room.streamer.as_mut()
                && let Err(e) = streamer.handle_keyframe_requests(viewer_effects.keyframe_requests)
            {
                error!("handle_keyframe_requests failed: {}", e);
            };

            for offer in viewer_effects.p2p_sdps {
                let (target_id, payload) = offer;

                if let Some(target) = room.viewers.get_mut(&target_id)
                    && let Some(mut channel) =
                        target.peer.cid.and_then(|id| target.peer.rtc.channel(id))
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

            for relay_id in viewer_effects.disconnected_relays {
                if let Some(relay) = room.viewers.get_mut(&relay_id)
                    && let Err(e) = relay.demote_relay()
                {
                    error!("demote relay failed error={e}")
                };
            }

            let relay_policy = metrics::time(Phase::RelayPolicy);

            for c in room
                .viewers
                .values_mut()
                .filter(|c| !c.relay_state.perf.is_empty() && c.relay_state.relay_status.is_none())
            {
                match c.relay_state.available_upload {
                    Some(UploadProbeResult::Failed { at }) => {
                        if at.elapsed() > tuning().relay.probe_interval()
                            && c.relay_state.is_perf_healthy()
                        {
                            c.probe_available_upload()
                        }
                    }
                    None => {
                        if c.relay_state.is_perf_healthy() {
                            c.probe_available_upload();
                        }
                    }
                    Some(UploadProbeResult::Probing { probed_at })
                        if probed_at.elapsed() > tuning().relay.probe_timeout() =>
                    {
                        c.relay_state.available_upload =
                            Some(UploadProbeResult::Failed { at: Instant::now() });
                    }

                    _ => (),
                }
            }

            let relay_ids: Vec<ClientId> = room
                .viewers
                .values()
                .filter_map(|c| {
                    if c.relay_state.relay_potential() == RelayPotential::Qualified
                        && c.relay_state.relay_status.is_none()
                        && c.peer.connected_at.elapsed() >= tuning().relay.min_connection_age()
                        && c.relay_state.is_perf_healthy()
                    {
                        return Some(c.id);
                    };

                    None
                })
                .collect();

            for relay_id in relay_ids {
                room.promote(relay_id);
            }

            for relay_id in viewer_effects.connected_relays {
                if let Some(relay) = room.viewers.get_mut(&relay_id)
                    && let Some(RelayStatus::Relay {
                        p2p_connected_at, ..
                    }) = &mut relay.relay_state.relay_status
                {
                    *p2p_connected_at = Some(Instant::now());
                };
            }

            let mut leaf_relay_ids_to_demote: Vec<(ClientId, ClientId)> = room
                .viewers
                .values()
                .filter_map(|c| {
                    let Some(RelayStatus::Relay {
                        leaf,
                        p2p_connected_at,
                    }) = c.relay_state.relay_status
                    else {
                        return None;
                    };

                    let relay_outgoing_kbps = c.relay_state.relay_outgoing_kbps? as u64;

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

                    if relay_outgoing_kbps < relay_outgoing_cutoff_kbps
                        || !c.relay_state.is_perf_healthy()
                    {
                        return Some((c.id, leaf));
                    };

                    None
                })
                .collect();

            for client in room.viewers.values() {
                if let Some(RelayStatus::Leaf {
                    relay,
                    link_state: LinkState::Connecting { at },
                }) = client.relay_state.relay_status
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
            if let Some(streamer) = &mut room.streamer {
                streamer.peer.handle_input(Input::Timeout(now));
            }
            for viewer in room.viewers.values_mut() {
                viewer.peer.handle_input(Input::Timeout(now));
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
        gauges.clients += (room.viewers.len() + usize::from(room.streamer.is_some())) as u64;
        gauges.viewers += room.viewers.len() as u64;

        for client in room.viewers.values() {
            match client.relay_state.relay_status {
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
    let streamer = rooms
        .values_mut()
        .filter_map(|room| room.streamer.as_mut())
        .find(|streamer| streamer.peer.rtc.accepts(&input));

    if let Some(streamer) = streamer {
        streamer.peer.handle_input(input);
        return;
    };

    let viewer = rooms
        .values_mut()
        .flat_map(|room| room.viewers.values_mut())
        .find(|viewer| viewer.peer.rtc.accepts(&input));

    if let Some(viewer) = viewer {
        viewer.peer.handle_input(input);
    } else {
        debug!("No client accepts UDP input");
    };
}
