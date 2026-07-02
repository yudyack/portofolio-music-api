use tracing_subscriber::EnvFilter;

/// Timestamp formatter that emits `Asia/Jakarta` (WIB, UTC+7) on every log
/// line — matches the ISO 8601 shape tracing-subscriber's default emits,
/// swapping the `Z` suffix for `+07:00`. Uses `chrono-tz`'s bundled IANA
/// data so no `TZ` env var / tzdata package in the container is required.
struct WibTimer;

impl tracing_subscriber::fmt::time::FormatTime for WibTimer {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        let now = chrono::Utc::now().with_timezone(&chrono_tz::Asia::Jakarta);
        write!(w, "{}", now.format("%Y-%m-%dT%H:%M:%S%.6f%:z"))
    }
}

#[tokio::main]
async fn main() {
    // Load a local .env if present (dev convenience; no-op in prod).
    let _ = dotenvy::dotenv();

    // Log filter:
    //   - Base is RUST_LOG if set, else `info`.
    //   - WIRE_BODIES=1 ADDITIVELY appends `music_api::wire=debug` to the base
    //     so the body-bearing wire logs (Spotify response bodies, FE response
    //     bodies) surface without having to remember the tracing target
    //     syntax. Toggle is additive so a custom RUST_LOG isn't clobbered.
    let wire_bodies = std::env::var("WIRE_BODIES")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let base = std::env::var("RUST_LOG")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "info".to_string());
    let filter_str = if wire_bodies {
        format!("{base},music_api::wire=debug")
    } else {
        base
    };
    let filter = EnvFilter::try_new(&filter_str).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_timer(WibTimer)
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
