pub mod app;
pub mod config;
pub mod domain;
pub mod infra;
pub mod oauth;
pub mod routes;
pub mod state;

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderValue, Method};
use axum::{routing::get, Json, Router};
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use thiserror::Error;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::app::state_store::StateStore;
use crate::config::{Config, ConfigError};
use crate::domain::auth_state::AuthState;
use crate::domain::oauth_client::TokenExchanger;
use crate::domain::spotify::SpotifyClient;
use crate::domain::tokens::TokenRepository;
use crate::infra::run_migrations;
use crate::infra::spotify_client::ReqwestSpotifyClient;
use crate::infra::sqlite_token_repo::SqliteTokenRepository;
use crate::infra::token_exchanger::ReqwestTokenExchanger;
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
    #[error("oauth client init failed: {0}")]
    OAuthClient(String),
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
    // base_url excludes `/v1`; callers pass the full path (e.g. `/v1/me`).
    let spotify: Arc<dyn SpotifyClient> =
        Arc::new(ReqwestSpotifyClient::new("https://api.spotify.com".to_string())
            .map_err(|e| InitError::SpotifyClient(format!("{e}")))?);
    let oauth: Arc<dyn TokenExchanger> = Arc::new(
        ReqwestTokenExchanger::new(
            "https://accounts.spotify.com/api/token".to_string(),
            config.spotify_client_id.clone(),
            config.spotify_client_secret.clone(),
        )
        .map_err(|e| InitError::OAuthClient(format!("{e}")))?,
    );
    let auth_state = Arc::new(AuthState::new());
    let state_store = Arc::new(StateStore::new());
    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    Ok((
        AppState::new(
            Arc::new(config),
            repo,
            spotify,
            oauth,
            auth_state,
            state_store,
        ),
        bind_addr,
    ))
}

/// Operational health snapshot. Always returns 200 (criterion 15) so the
/// Cloudflare tunnel doesn't flap when the upstream link is unhealthy —
/// the body carries the state instead.
///
/// `status` is derived from `AuthState` (criterion 6 read-side): a flipped
/// `NeedsReauth` flag surfaces as `status:"needs_reauth"` for the frontend
/// to render a banner. `token_state` reports raw repo presence;
/// `last_fetch_ts` is still TODO (needs a refresher signal).
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
            tracing::warn!(error = %e, "healthz repo lookup failed");
            "unknown"
        }
    };
    let status: &'static str = if state.auth_state.needs_reauth() {
        "needs_reauth"
    } else {
        "ok"
    };
    Json(Health {
        status,
        version: VERSION,
        token_state,
        last_fetch_ts: None,
    })
}

pub fn app(state: AppState) -> Router {
    // Criterion 14: CORS layered ONLY on /v1/* (the public data plane the
    // leptos frontend calls cross-origin). /auth/* is browser-redirect-only
    // and /healthz is operational — neither emits CORS headers per spec §5.7.
    let v1 = Router::new()
        .route("/v1/profile", get(routes::v1::profile))
        .route("/v1/now", get(routes::v1::now))
        .route("/v1/recent", get(routes::v1::recent))
        .route("/v1/top/tracks", get(routes::v1::top_tracks))
        .route("/v1/playlists", get(routes::v1::playlists))
        .layer(cors_layer());

    Router::new()
        .route("/healthz", get(healthz))
        .route("/auth/spotify/login", get(routes::auth::login))
        .route("/auth/spotify/callback", get(routes::auth::callback))
        .merge(v1)
        .with_state(state)
}

fn cors_layer() -> CorsLayer {
    // Allowlist mirrors spec §5.7: production origins + localhost for dev.
    // Wildcard `127.0.0.1:*` is approximated by a predicate so any dev port
    // works (vite/leptos default ports vary).
    let allow_origin = AllowOrigin::predicate(|origin: &HeaderValue, _req| {
        let Ok(s) = origin.to_str() else { return false };
        matches!(s, "https://yudhyapw.com" | "https://www.yudhyapw.com")
            || s.starts_with("http://127.0.0.1:")
            || s.starts_with("http://localhost:")
    });
    CorsLayer::new()
        .allow_origin(allow_origin)
        .allow_methods([Method::GET])
}
