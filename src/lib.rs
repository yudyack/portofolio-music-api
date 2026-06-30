pub mod app;
pub mod config;
pub mod domain;
pub mod infra;
pub mod oauth;
pub mod routes;
pub mod state;

use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
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
use crate::domain::tokens::TokenRecord;
use crate::domain::tokens::TokenRepository;
use crate::infra::mock_spotify_client::MockSpotifyClient;
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

    // Mock-mode short-circuit: skip the real Spotify + OAuth wiring, return
    // canned fixtures via MockSpotifyClient, and seed a fake token row so
    // the data plane works without going through the OAuth bootstrap. The
    // real OAuth exchanger is still constructed (unused at runtime, but
    // wiring stays uniform across modes).
    if config.mock_data {
        seed_mock_token(&repo, &config.owner_spotify_user_id).await?;
    }
    let spotify: Arc<dyn SpotifyClient> = if config.mock_data {
        tracing::warn!("MOCK_DATA=1 — serving canned fixtures, NOT real Spotify");
        Arc::new(MockSpotifyClient::new())
    } else {
        Arc::new(
            ReqwestSpotifyClient::new("https://api.spotify.com".to_string())
                .map_err(|e| InitError::SpotifyClient(format!("{e}")))?,
        )
    };
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

    // (helper below — declared after init for ergonomics; mock-token seeding
    // upserts a long-lived fake so `tokens.get()` returns Some(_) and the
    // /v1/* handlers don't trip the `NeedsReauth` branch in MockSpotifyClient.)
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
    /// True iff `MOCK_DATA=1` is set at startup — the leptos frontend reads
    /// this and renders a "MOCK DATA" banner. Always serialised so the
    /// absence is unambiguously "real Spotify data, not just absent flag".
    mock_mode: bool,
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
        mock_mode: state.config.mock_data,
    })
}

/// Upsert a deterministic fake token so mock-mode `/v1/*` handlers see a
/// `Some(TokenRecord)` from the repo and don't hit `NeedsReauth`. The
/// access token here is never sent anywhere — MockSpotifyClient ignores
/// it. Expiry is far in the future so the refresher (when it lands) won't
/// trigger.
async fn seed_mock_token(repo: &Arc<dyn TokenRepository>, owner_id: &str) -> Result<(), InitError> {
    let record = TokenRecord {
        access_token: "MOCK_ACCESS_NEVER_SENT".to_string(),
        refresh_token: "MOCK_REFRESH_NEVER_SENT".to_string(),
        // Year-2099 — well past any realistic refresh window.
        expires_at: chrono::DateTime::<chrono::Utc>::from_timestamp(4102444800, 0)
            .unwrap_or_else(chrono::Utc::now),
        scope: "user-read-private user-read-playback-state user-read-recently-played \
                user-top-read playlist-read-private user-follow-read"
            .to_string(),
        owner_id: owner_id.to_string(),
    };
    repo.upsert(record)
        .await
        .map_err(|e| InitError::Migrate(format!("seed mock token: {e}")))
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
        // Activity gate sits INSIDE the CORS layer (so preflights are not
        // counted as user activity) — `from_fn_with_state` runs before the
        // handler resolves; every successful or failed /v1/* hit touches
        // the tracker.
        .layer(from_fn_with_state(state.clone(), v1_activity_layer))
        // Unified FE-side wire logging — symmetric to the outbound HTTP
        // LoggingMiddleware. Every /v1/* request and response is logged
        // here so handlers don't carry per-call `tracing::info!` macros.
        // Outside the activity gate so the inbound log fires before any
        // app-level effects (activity touch); inside CORS so preflights
        // are not logged.
        .layer(from_fn(wire_fe_layer))
        .layer(cors_layer());

    Router::new()
        .route("/healthz", get(healthz))
        .route("/auth/spotify/login", get(routes::auth::login))
        .route("/auth/spotify/callback", get(routes::auth::callback))
        .merge(v1)
        .with_state(state)
}

/// Middleware on `/v1/*` that records visitor activity for the scheduler
/// gate (`app::activity::ActivityTracker`). Runs on every request,
/// regardless of handler outcome — even a 503 `needs_reauth` counts as a
/// visitor for the purpose of waking the parked schedulers.
async fn v1_activity_layer(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    state.activity.touch();
    next.run(request).await
}

/// FE-side wire logging — symmetric to the outbound HTTP
/// `LoggingMiddleware`. Logs every `/v1/*` request at info, the response
/// body at debug (gated by `WIRE_BODIES=1`), and a status + bytes +
/// elapsed_ms summary at info. The body is buffered so the debug line
/// can carry it; for the JSON payloads this service returns (low-tens
/// of KB), buffering is cheap.
async fn wire_fe_layer(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    tracing::info!(
        target: "music_api::wire::fe",
        direction = "→",
        method = %method,
        path = %path,
        "frontend request",
    );

    let started = Instant::now();
    let response = next.run(request).await;
    let status = response.status().as_u16();
    let elapsed_ms = started.elapsed().as_millis() as u64;

    let (parts, body) = response.into_parts();
    // 10 MB ceiling — well above any legitimate /v1/* JSON payload and
    // tight enough that a runaway response can't pin the process. If a
    // future handler streams a large response, this layer is the wrong
    // tool and should be skipped for that route.
    let bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                target: "music_api::wire::fe",
                error = %e,
                method = %method,
                path = %path,
                "failed to buffer fe response body for logging",
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, "log buffer failed").into_response();
        }
    };

    tracing::debug!(
        target: "music_api::wire::fe",
        direction = "←",
        method = %method,
        path = %path,
        status = status,
        body = %String::from_utf8_lossy(&bytes),
        "frontend response (body)",
    );
    tracing::info!(
        target: "music_api::wire::fe",
        direction = "←",
        method = %method,
        path = %path,
        status = status,
        bytes = bytes.len(),
        elapsed_ms = elapsed_ms,
        "frontend response",
    );

    Response::from_parts(parts, Body::from(bytes))
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
