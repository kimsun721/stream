use axum_server::tls_rustls::RustlsConfig;

pub struct WebConfig {
    pub api_key: String,
    pub certificate: RustlsConfig,
}

pub async fn load() -> WebConfig {
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
