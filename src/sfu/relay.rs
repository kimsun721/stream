use std::time::Instant;

use crate::{
    config::tuning,
    sfu::error::ClientResult,
    types::{RelayPotential, RelayState, RelayStatus, UploadProbeResult},
};

impl RelayState {
    pub fn relay_potential(&self) -> RelayPotential {
        let cutoff = tuning().relay.available_upload_cutoff_kbps;

        match self.available_upload {
            Some(UploadProbeResult::Probed {
                available_upload_kbps,
                ..
            }) => {
                if available_upload_kbps > cutoff {
                    RelayPotential::Qualified
                } else {
                    RelayPotential::Never
                }
            }
            _ => RelayPotential::Unknown,
        }
    }

    pub fn handle_available_upload(&mut self, available_upload_kbps: u32) -> ClientResult<()> {
        if let Some(UploadProbeResult::Probing { .. }) = self.available_upload {
            self.available_upload = Some(UploadProbeResult::Probed {
                available_upload_kbps,
                probed_at: Instant::now(),
            });
        }

        Ok(())
    }

    pub fn handle_relay_outgoing(&mut self, kbps: u32) -> ClientResult<()> {
        if let Some(RelayStatus::Relay { outgoing_kbps, .. }) = &mut self.role {
            *outgoing_kbps = Some(kbps);
        }

        Ok(())
    }
}
