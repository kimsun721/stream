use std::{
    cell::RefCell,
    collections::HashMap,
    net::UdpSocket,
    rc::Rc,
    time::{Duration, Instant},
};

use str0m::{
    Event, IceConnectionState, Rtc,
    media::{KeyframeRequest, MediaKind, Mid, Rid},
};
use uuid::Uuid;

use crate::{
    metrics,
    sfu::error::ClientResult,
    types::{
        BitrateEstimator, ClientId, Peer, PollResult, PushOutcome, SimulcastLayer, Streamer,
        StreamerEffects, TickResult, TrackIn, TrackInEntry,
    },
};

impl Streamer {
    pub fn new(rtc: Rtc, session_id: Uuid) -> Streamer {
        Streamer {
            id: ClientId::next(),
            peer: Peer::new(rtc, session_id),
            tracks_in: Vec::new(),
        }
    }

    pub fn tick(
        &mut self,
        effects: &mut StreamerEffects,
        socket: &UdpSocket,
    ) -> ClientResult<TickResult> {
        let StreamerEffects {
            new_tracks,
            media_datas,
            should_reevaluate,
        } = effects;

        loop {
            match self.peer.poll(socket)? {
                PollResult::Timeout(at) => return Ok(TickResult::Timeout(at)),
                PollResult::Event(event) => match event {
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                        self.peer.rtc.disconnect();
                        return Ok(TickResult::Disconnected);
                    }
                    Event::MediaData(data) => {
                        metrics::media_data();

                        if let Some(rid) = data.rid
                            && let Some(track_in) =
                                self.tracks_in.iter_mut().find(|t| t.id.mid == data.mid)
                            && let Some(simulcast_layer) = track_in
                                .id
                                .available_simulcast_layers
                                .iter()
                                .find(|l| l.rid == rid)
                        {
                            let push_outcome = simulcast_layer
                                .bitrate_estimator
                                .borrow_mut()
                                .push(data.data.len(), Instant::now());

                            if matches!(push_outcome, PushOutcome::EstimateBecameAvailable) {
                                *should_reevaluate = true;
                            }
                        }

                        media_datas.push(data);
                    }
                    Event::MediaAdded(m) => {
                        let mut layers: Vec<SimulcastLayer> = Vec::new();

                        let rids: Vec<Rid> = m
                            .simulcast
                            .map(|simulcast| simulcast.recv.iter().map(|l| l.rid).collect())
                            .unwrap_or_default();

                        for rid in rids {
                            layers.push(SimulcastLayer {
                                rid,
                                bitrate_estimator: RefCell::new(BitrateEstimator::new()),
                            });
                        }

                        let track_in = self.handle_media_added(m.mid, m.kind, layers);
                        new_tracks.push(track_in);
                    }
                    _ => {}
                },
            }
        }
    }

    fn handle_media_added(
        &mut self,
        mid: Mid,
        kind: MediaKind,
        simulcast_layers: Vec<SimulcastLayer>,
    ) -> Rc<TrackIn> {
        let track_in = Rc::new(TrackIn {
            origin: self.id,
            mid,
            kind,
            available_simulcast_layers: simulcast_layers,
        });

        let track_in_entry = TrackInEntry {
            id: track_in.clone(),
            last_keyframe_requested_at: HashMap::new(),
        };

        self.tracks_in.push(track_in_entry);
        track_in
    }

    pub fn handle_keyframe_requests(
        &mut self,
        keyframe_requests: Vec<KeyframeRequest>,
    ) -> ClientResult<()> {
        for req in keyframe_requests {
            if let Some(track_entry) = self.tracks_in.iter_mut().find(|t| t.id.mid == req.mid) {
                let should_request = track_entry
                    .last_keyframe_requested_at
                    .get(&req.rid)
                    .is_none_or(|at| at.elapsed() >= Duration::from_millis(1000));

                if should_request && let Some(mut writer) = self.peer.rtc.writer(req.mid) {
                    writer.request_keyframe(req.rid, req.kind)?;
                    track_entry
                        .last_keyframe_requested_at
                        .insert(req.rid, Instant::now());
                };
            };
        }

        Ok(())
    }
}
