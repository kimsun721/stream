use std::time::Instant;

use crate::{
    config::tuning,
    metrics,
    types::{
        ClientId, LinkState, P2pDemoteReason, P2pEvent, P2pPair, RelayState, RelayStatus, Room,
        Viewer,
    },
};

use tracing::error;

enum UploadAction {
    Keep,
    Clear,
    Cooldown,
}

struct P2pDemotePolicy {
    upload_action: UploadAction,
    record_pair: bool,
}

impl P2pDemotePolicy {
    fn new(upload_action: UploadAction, record_pair: bool) -> Self {
        P2pDemotePolicy {
            upload_action,
            record_pair,
        }
    }
}

impl P2pDemoteReason {
    fn policy(self) -> P2pDemotePolicy {
        match self {
            Self::LeafAskedHigher
            | Self::RelayChangedLower
            | Self::Rebalance
            | Self::LinkLost
            | Self::RelayLeft
            | Self::LeafLeft => P2pDemotePolicy::new(UploadAction::Keep, false),
            Self::LinkTimeout | Self::LinkPoor => P2pDemotePolicy::new(UploadAction::Keep, true),
            Self::RelayCpu => P2pDemotePolicy::new(UploadAction::Cooldown, false),
            Self::RelayUnhealthy => P2pDemotePolicy::new(UploadAction::Clear, false),
            Self::RateShort => P2pDemotePolicy::new(UploadAction::Clear, true),
        }
    }
}

impl Room {
    pub fn relay(&self, pair: P2pPair) -> Option<&Viewer> {
        if let Some(relay) = self.viewers.get(&pair.relay)
            && relay.is_relay(&pair.leaf)
        {
            Some(relay)
        } else {
            None
        }
    }

    pub fn relay_mut(&mut self, pair: P2pPair) -> Option<&mut Viewer> {
        if let Some(relay) = self.viewers.get_mut(&pair.relay)
            && relay.is_relay(&pair.leaf)
        {
            Some(relay)
        } else {
            None
        }
    }

    pub fn leaf(&self, pair: P2pPair) -> Option<&Viewer> {
        if let Some(leaf) = self.viewers.get(&pair.leaf)
            && leaf.is_leaf(&pair.relay)
        {
            Some(leaf)
        } else {
            None
        }
    }

    pub fn leaf_mut(&mut self, pair: P2pPair) -> Option<&mut Viewer> {
        if let Some(leaf) = self.viewers.get_mut(&pair.leaf)
            && leaf.is_leaf(&pair.relay)
        {
            Some(leaf)
        } else {
            None
        }
    }

    pub fn demote_p2p_pair(&mut self, pair: P2pPair, reason: P2pDemoteReason) {
        if self.leaf(pair).is_none() && self.relay(pair).is_none() {
            return;
        };

        metrics::demoted();

        let P2pDemotePolicy {
            upload_action,
            record_pair,
        } = reason.policy();

        if let Some(relay) = self.relay_mut(pair) {
            match upload_action {
                UploadAction::Clear => {
                    relay.relay_state.available_upload = None;
                }
                UploadAction::Cooldown => {
                    relay.relay_state.cooldown_until =
                        Some(Instant::now() + tuning().relay.probe_interval());
                }
                UploadAction::Keep => {}
            }

            if let Err(e) = relay.demote_relay() {
                error!("demote relay failed: {e}");
            }
        }

        if let Some(leaf) = self.leaf_mut(pair)
            && let Ok(keyframe_requests) = leaf.demote_leaf()
            && let Some(streamer) = &mut self.streamer
            && let Err(e) = streamer.handle_keyframe_requests(keyframe_requests)
        {
            error!("demote_leaf keyframe_requests failed: {e}");
        }

        if record_pair {
            self.mismatched_pairs.0.insert(pair);
        }
    }

    pub fn apply_event(&mut self, event: P2pEvent) {
        match event {
            P2pEvent::Connected { pair } => {
                if let [Some(relay), Some(leaf)] =
                    self.viewers.get_disjoint_mut([&pair.relay, &pair.leaf])
                    && let Some(RelayStatus::Relay {
                        p2p_connected_at, ..
                    }) = &mut relay.relay_state.role
                    && let Some(RelayStatus::Leaf { link_state, .. }) = &mut leaf.relay_state.role
                {
                    *p2p_connected_at = Some(Instant::now());
                    *link_state = LinkState::Connected;
                }
            }
            P2pEvent::Disconnected { pair, reason } => {
                self.demote_p2p_pair(pair, reason);
            }
        }
    }
}

impl Viewer {
    pub fn is_relay(&self, leaf: &ClientId) -> bool {
        self.relay_state
            .role
            .as_ref()
            .is_some_and(|r| matches!(r, RelayStatus::Relay { leaf_id, .. } if leaf_id == leaf))
    }

    pub fn is_leaf(&self, relay: &ClientId) -> bool {
        self.relay_state
            .role
            .as_ref()
            .is_some_and(|r| matches!(r, RelayStatus::Leaf { relay_id, .. } if relay_id == relay))
    }
}

impl P2pEvent {
    pub fn connected(relay: ClientId, leaf: ClientId) -> P2pEvent {
        P2pEvent::Connected {
            pair: P2pPair::new(relay, leaf),
        }
    }

    pub fn disconnected(relay: ClientId, leaf: ClientId, reason: P2pDemoteReason) -> P2pEvent {
        P2pEvent::Disconnected {
            pair: P2pPair::new(relay, leaf),
            reason,
        }
    }
}

impl P2pPair {
    pub fn new(relay: ClientId, leaf: ClientId) -> P2pPair {
        P2pPair { relay, leaf }
    }
}

impl RelayState {
    pub fn is_cooldown_over(&self) -> bool {
        self.cooldown_until
            .is_none_or(|until| until <= Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::{P2pDemotePolicy, UploadAction};
    use crate::types::P2pDemoteReason::{self, *};

    fn policy(reason: P2pDemoteReason) -> (&'static str, bool) {
        let P2pDemotePolicy {
            upload_action,
            record_pair,
        } = reason.policy();

        let upload = match upload_action {
            UploadAction::Keep => "keep",
            UploadAction::Clear => "clear",
            UploadAction::Cooldown => "cooldown",
        };

        (upload, record_pair)
    }

    #[test]
    fn a_pair_ended_by_choice_or_departure_costs_the_relay_nothing() {
        for reason in [
            LeafAskedHigher,
            RelayChangedLower,
            Rebalance,
            LinkLost,
            RelayLeft,
            LeafLeft,
        ] {
            assert_eq!(policy(reason), ("keep", false), "{reason:?}");
        }
    }

    #[test]
    fn a_path_that_fails_between_two_viewers_is_remembered_against_that_pair() {
        assert_eq!(policy(LinkTimeout), ("keep", true));
        assert_eq!(policy(LinkPoor), ("keep", true));
    }

    #[test]
    fn a_relay_short_on_cpu_rests_with_its_upload_figure_kept() {
        assert_eq!(policy(RelayCpu), ("cooldown", false));
    }

    #[test]
    fn a_relay_whose_link_or_rate_gave_out_is_measured_again() {
        assert_eq!(policy(RelayUnhealthy), ("clear", false));
        assert_eq!(policy(RateShort), ("clear", true));
    }
}
