use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    // Load a local .env if present (dev convenience; no-op in prod).
    let _ = dotenvy::dotenv();

    // Default filter is `info`. Wire-level body logs (FE responses,
    // Spotify response bodies) are at `debug` — set
    // `RUST_LOG=music_api::wire=debug` to capture payloads, or
    // `RUST_LOG=music_api::wire::spotify_oauth=debug` for OAuth only.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Cycle 7: all startup work lives in music_api::init() — config parse,
    // sqlite connect (WAL + busy_timeout), migrations (criterion 21),
    // Arc<dyn TokenRepository> construction. main() owns only the bind +
    // serve calls and the eprintln+exit(1) error pattern.
    let (state, bind_addr) = match music_api::init().await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "startup failed");
            eprintln!("music-api: {e}");
            std::process::exit(1);
        }
    };

    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, %bind_addr, "failed to bind listener");
            eprintln!("music-api: failed to bind {bind_addr}: {e}");
            std::process::exit(1);
        }
    };

    tracing::info!(%bind_addr, "music-api listening");

    // Spawn the per-endpoint scheduler-push tasks. They park on the
    // activity gate until the first /v1/* visitor lands, then tick on the
    // intervals in `config.scheduler.intervals`. Live for the process
    // lifetime — no JoinHandle is kept because the runtime tears them
    // down at shutdown.
    music_api::app::scheduler::spawn_schedulers(state.clone());

    if let Err(e) = axum::serve(listener, music_api::app(state)).await {
        tracing::error!(error = %e, "serve loop crashed");
        eprintln!("music-api: serve loop crashed: {e}");
        std::process::exit(1);
    }
}
