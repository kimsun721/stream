use std::{fs, sync::OnceLock, time::Duration};

use axum_server::tls_rustls::RustlsConfig;
use serde::Deserialize;
use tracing::warn;

pub struct WebConfig {
    pub api_key: String,
    pub certificate: RustlsConfig,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(default, deny_unknown_fields)]
pub struct PerfConfig {
    pub window_max_age_secs: u64,
    pub window_max_len: usize,
    pub rtt_ms_cutoff: u32,
    pub loss_pct_cutoff: f32,
    pub min_samples_len: u32,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(default, deny_unknown_fields)]
pub struct RelayConfig {
    pub probe_timeout_secs: u64,
    pub probe_interval_secs: u64,
    pub min_connection_age_secs: u64,
    pub available_upload_cutoff_kbps: u32,
    pub outgoing_cutoff_kbps: u32,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(default, deny_unknown_fields)]
pub struct BitrateConfig {
    pub initial_mbps: u64,
    pub desired_mbps: u64,
    pub headroom_margin_percent: u128,
    pub upswitch_margin_percent: u128,
    pub estimate_secs: u64,
}

#[derive(Deserialize, Clone, Copy, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Tuning {
    pub perf: PerfConfig,
    pub relay: RelayConfig,
    pub bitrate: BitrateConfig,
}

static CONFIG: OnceLock<Tuning> = OnceLock::new();

pub fn tuning() -> &'static Tuning {
    CONFIG.get_or_init(|| match fs::read_to_string("config.toml") {
        Ok(content) => toml::from_str(&content).expect("toml value wrong"),
        Err(_) => {
            warn!("Could not find config.toml; falling back to default configuration");
            Tuning::default()
        }
    })
}

impl RelayConfig {
    pub fn probe_timeout(&self) -> Duration {
        Duration::from_secs(self.probe_timeout_secs)
    }

    pub fn probe_interval(&self) -> Duration {
        Duration::from_secs(self.probe_interval_secs)
    }

    pub fn min_connection_age(&self) -> Duration {
        Duration::from_secs(self.min_connection_age_secs)
    }
}

impl PerfConfig {
    pub fn window_max_age(&self) -> Duration {
        Duration::from_secs(self.window_max_age_secs)
    }
}

impl BitrateConfig {
    pub fn estimation_interval(&self) -> Duration {
        Duration::from_secs(self.estimate_secs)
    }
}

pub fn init_tuning() {
    tuning();
}

pub async fn load_web_config() -> WebConfig {
    dotenvy::dotenv().ok();

    let load_from_env = |var: &str| -> String {
        std::env::var(var).unwrap_or_else(|_| {
            panic!("{var} is not set in the .env");
        })
    };

    let api_key = load_from_env("API_KEY");
    let cert_path = load_from_env("CERT_PATH");
    let key_path = load_from_env("KEY_PATH");

    if api_key.len() < 16 {
        panic!(
            "API_KEY too short: expected at least 16 characters, got {}",
            api_key.len()
        );
    }

    let certificate = RustlsConfig::from_pem_file(cert_path, key_path)
        .await
        .unwrap_or_else(|e| {
            panic!("failed to load certificate: {e}");
        });

    WebConfig {
        api_key,
        certificate,
    }
}

impl Default for PerfConfig {
    fn default() -> Self {
        PerfConfig {
            window_max_age_secs: 300,
            window_max_len: 1000,
            rtt_ms_cutoff: 30,
            loss_pct_cutoff: 10.0,
            min_samples_len: 200,
        }
    }
}

impl Default for RelayConfig {
    fn default() -> Self {
        RelayConfig {
            probe_timeout_secs: 60,
            probe_interval_secs: 120,
            min_connection_age_secs: 300,
            available_upload_cutoff_kbps: 13000,
            outgoing_cutoff_kbps: 7000,
        }
    }
}

impl Default for BitrateConfig {
    fn default() -> Self {
        BitrateConfig {
            initial_mbps: 4,
            desired_mbps: 10,
            headroom_margin_percent: 10,
            upswitch_margin_percent: 10,
            estimate_secs: 3,
        }
    }
}
