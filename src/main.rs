use music_api::config::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Criteria 22, 24: required env vars are validated at startup; a missing
    // OWNER_SPOTIFY_USER_ID or AUTH_BASIC_PASSWORD exits non-zero with a clear
    // message before the listener binds. The Display impl on ConfigError names
    // the missing var.
    let _config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "startup config invalid");
            eprintln!("music-api: {e}");
            std::process::exit(1);
        }
    };

    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .expect("bind");
    tracing::info!(%bind_addr, "music-api listening");
    axum::serve(listener, music_api::app())
        .await
        .expect("serve");
}
