use std::time::Instant;

use crate::{
    config::tuning,
    sfu::error::ClientResult,
    types::{PerfSample, RelayPotential, RelayState, RelayStatus, UploadProbeResult},
};

impl RelayState {
    pub fn relay_potential(&self) -> RelayPotential {
        let cutoff = tuning().relay.available_upload_cutoff_kbps;

        match self.available_upload {
            Some(UploadProbeResult::Probed {
                available_upload_kbps,
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

    pub fn is_perf_healthy(&self) -> bool {
        if self.perf.is_empty() {
            return false;
        }

        let mut rtt_ms_avg = 0;
        let mut loss_pct_avg = 0.0;
        let len = self.perf.len() as u32;

        for p in self.perf.iter() {
            rtt_ms_avg += p.rtt_ms;
            loss_pct_avg += p.loss_pct;
        }

        rtt_ms_avg /= len;
        loss_pct_avg /= len as f32;

        rtt_ms_avg < tuning().perf.rtt_ms_cutoff
            && loss_pct_avg < tuning().perf.loss_pct_cutoff
            && len > tuning().perf.min_samples_len
    }

    pub fn perf_report(&mut self, rtt_ms: u32, loss_pct: f32) -> ClientResult<()> {
        let now = Instant::now();

        self.perf.push_back(PerfSample {
            rtt_ms,
            loss_pct,
            timestamp: now,
        });

        while self.perf.front().is_some_and(|p| {
            now.duration_since(p.timestamp) > tuning().perf.window_max_age()
                || self.perf.len() > tuning().perf.window_max_len
        }) {
            self.perf.pop_front();
        }

        Ok(())
    }

    pub fn handle_available_upload(&mut self, available_upload_kbps: u32) -> ClientResult<()> {
        if let Some(UploadProbeResult::Probing { .. }) = self.available_upload {
            self.available_upload = Some(UploadProbeResult::Probed {
                available_upload_kbps,
            });
        }

        Ok(())
    }

    pub fn handle_relay_outgoing(&mut self, kbps: u32) -> ClientResult<()> {
        if let Some(RelayStatus::Relay { .. }) = self.relay_status {
            self.relay_outgoing_kbps = Some(kbps);
        }

        Ok(())
    }
}
