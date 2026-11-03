use std::time::Instant;

use crate::{
    metrics,
    types::{ClientId, LinkState, P2pDemoteReason, P2pEvent, P2pPair, RelayStatus, Room, Viewer},
};

use tracing::error;

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

        if let Some(relay) = self.relay_mut(pair)
            && let Err(e) = relay.demote_relay()
        {
            error!("demote relay failed: {e}");
        }

        if let Some(leaf) = self.leaf_mut(pair)
            && let Ok(keyframe_requests) = leaf.demote_leaf()
            && let Some(streamer) = &mut self.streamer
            && let Err(e) = streamer.handle_keyframe_requests(keyframe_requests)
        {
            error!("demote_leaf keyframe_requests failed: {e}");
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
