pub mod app;
pub mod config;
pub mod domain;
pub mod infra;
pub mod oauth;
pub mod state;

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::{routing::get, Json, Router};
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use thiserror::Error;

use crate::config::{Config, ConfigError};
use crate::domain::spotify::SpotifyClient;
use crate::domain::tokens::TokenRepository;
use crate::infra::run_migrations;
use crate::infra::spotify_client::ReqwestSpotifyClient;
use crate::infra::sqlite_token_repo::SqliteTokenRepository;
pub use crate::state::AppState;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Startup failure surface. Each variant maps 1:1 to a step in `init()` so
/// the operator-facing eprintln message names exactly the step that broke.
#[derive(Debug, Error)]
pub enum InitError {
    #[error("config invalid: {0}")]
    Config(#[from] ConfigError),
    #[error("invalid DATABASE_URL: {0}")]
    InvalidDatabaseUrl(String),
    #[error("sqlite connect failed: {0}")]
    SqliteConnect(String),
    #[error("migrations failed: {0}")]
    Migrate(String),
    #[error("spotify client init failed: {0}")]
    SpotifyClient(String),
}

/// Build the AppState and resolve the bind address. Called once at startup
/// before the listener binds (criterion 21). Returns the bind_addr alongside
/// AppState because BIND_ADDR is not part of Config and the listener-bind
/// itself lives in `main` (so the &listener reference doesn't escape).
///
/// Order is load-bearing — see the within-lib.rs byte-position guard in
/// tests/sqlite_pool_options.rs.
pub async fn init() -> Result<(AppState, String), InitError> {
    let config = Config::from_env()?;

    // WAL + busy_timeout pre-arm cycle 7+'s single-flight refresher.
    // Strictly more permissive than defaults; existing single-writer paths
    // see no behavior change.
    let opts = SqliteConnectOptions::from_str(&config.database_url)
        .map_err(|e| InitError::InvalidDatabaseUrl(format!("{e}")))?
        .create_if_missing(true)
        .busy_timeout(Duration::from_secs(5))
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .connect_with(opts)
        .await
        .map_err(|e| InitError::SqliteConnect(format!("{e}")))?;

    // Criterion 21: migrations apply BEFORE the listener binds. The bind
    // call lives in main and runs after this function returns.
    run_migrations(&pool)
        .await
        .map_err(|e| InitError::Migrate(format!("{e}")))?;

    let repo: Arc<dyn TokenRepository> = Arc::new(SqliteTokenRepository::new(pool));
    // Cycle 8: SpotifyClient is constructed once at startup. base_url
    // excludes the `/v1` segment per the trait contract — callers pass
    // the full path including the version. No handler reads `state.spotify`
    // yet; cycle 10 wires the first `/v1/*` caller.
    let spotify: Arc<dyn SpotifyClient> =
        Arc::new(ReqwestSpotifyClient::new("https://api.spotify.com".to_string())
            .map_err(|e| InitError::SpotifyClient(format!("{e}")))?);
    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    Ok((AppState::new(repo, spotify), bind_addr))
}

/// Operational health snapshot. Always returns 200 (criterion 15) so the
/// Cloudflare tunnel doesn't flap when the upstream link is unhealthy —
/// the body carries the state instead.
///
/// Cycle 7 derives `token_state` from the repository; `status` stays
/// hardcoded `"ok"` until cycle 10's refresher introduces real degradation
/// signals. The `status`/`token_state` fields are `&'static str` because
/// every value they take today is a literal — cycle 10+ widens to enums.
#[derive(Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
    token_state: &'static str,
    last_fetch_ts: Option<String>,
}

async fn healthz(State(state): State<AppState>) -> Json<Health> {
    let token_state: &'static str = match state.tokens.get().await {
        Ok(Some(_)) => "authorized",
        Ok(None) => "uninitialized",
        Err(e) => {
            // Log the chain, do not leak it to the wire (criterion 13).
            tracing::warn!(error = %e, "healthz repo lookup failed");
            "unknown"
        }
    };
    Json(Health {
        status: "ok",
        version: VERSION,
        token_state,
        last_fetch_ts: None,
    })
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .with_state(state)
}
