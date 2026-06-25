use music_api::config::Config;
use music_api::infra::run_migrations;
use music_api::infra::sqlite_token_repo::SqliteTokenRepository;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Criteria 22, 24: required env vars validated before anything else.
    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "startup config invalid");
            eprintln!("music-api: {e}");
            std::process::exit(1);
        }
    };

    // Criterion 21: migrations run before the listener binds. Done by parsing
    // DATABASE_URL, opening a pool with create_if_missing, applying pending
    // migrations, and only then constructing the repository + serving.
    // WAL + busy_timeout pre-arm cycle 7's single-flight refresher: WAL gives
    // readers and writers concurrent access; busy_timeout makes a contended
    // writer wait instead of returning SQLITE_BUSY. Strictly more permissive
    // than defaults; existing single-writer paths see no behavior change.
    let pool_opts = match SqliteConnectOptions::from_str(&config.database_url) {
        Ok(o) => o
            .create_if_missing(true)
            .busy_timeout(Duration::from_secs(5))
            .journal_mode(SqliteJournalMode::Wal),
        Err(e) => {
            tracing::error!(error = %e, "invalid DATABASE_URL");
            eprintln!("music-api: invalid DATABASE_URL: {e}");
            std::process::exit(1);
        }
    };
    let pool = match SqlitePoolOptions::new().connect_with(pool_opts).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "sqlite connect failed");
            eprintln!("music-api: sqlite connect failed: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = run_migrations(&pool).await {
        tracing::error!(error = %e, "migrations failed");
        eprintln!("music-api: migrations failed: {e}");
        std::process::exit(1);
    }
    let _repo: Arc<dyn music_api::domain::tokens::TokenRepository> =
        Arc::new(SqliteTokenRepository::new(pool));

    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, %bind_addr, "failed to bind listener");
            eprintln!("music-api: failed to bind {bind_addr}: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!(%bind_addr, "music-api listening");
    if let Err(e) = axum::serve(listener, music_api::app()).await {
        tracing::error!(error = %e, "serve loop crashed");
        eprintln!("music-api: serve loop crashed: {e}");
        std::process::exit(1);
    }
}
