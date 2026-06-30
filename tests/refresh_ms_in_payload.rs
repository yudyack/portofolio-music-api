//! Wire contract: every successful `/v1/*` response carries `refresh_ms`
//! — the scheduler's per-endpoint tick interval in milliseconds — so the
//! leptos frontend can match its polling cadence and extrapolate values
//! client-side (e.g. interpolate the now-playing progress bar between
//! snapshots).
//!
//! Pinned for both the snapshot-hit path (scheduler bakes the field in)
//! and the cold-start path (handler's synchronous fetch goes through the
//! same `fetch_and_map`, so the same field appears).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use chrono::Utc;
use music_api::app::state_store::StateStore;
use music_api::config::{Config, SchedulerConfig, SchedulerIntervals};
use music_api::domain::auth_state::AuthState;
use music_api::domain::oauth_client::{RefreshedTokens, TokenExchangeError, TokenExchanger};
use music_api::domain::spotify::{SpotifyClient, SpotifyError};
use music_api::domain::tokens::{RepoError, TokenRecord, TokenRepository};
use music_api::{app, AppState};
use serde_json::{json, Value};
use tower::ServiceExt;

struct RoutedSpotify {
    by_path: Mutex<HashMap<String, Value>>,
    calls: AtomicUsize,
}

impl RoutedSpotify {
    fn new(routes: Vec<(&str, Value)>) -> Self {
        let mut map = HashMap::new();
        for (k, v) in routes {
            map.insert(k.to_string(), v);
        }
        Self {
            by_path: Mutex::new(map),
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl SpotifyClient for RoutedSpotify {
    async fn get_json(&self, path: &str, _t: &str) -> Result<Option<Value>, SpotifyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.by_path.lock().unwrap().get(path).cloned() {
            Some(v) => Ok(Some(v)),
            None => Err(SpotifyError::Status(404)),
        }
    }
}

struct MemRepo;
#[async_trait]
impl TokenRepository for MemRepo {
    async fn get(&self) -> Result<Option<TokenRecord>, RepoError> {
        Ok(Some(TokenRecord {
            access_token: "ACCESS".into(),
            refresh_token: "REFRESH".into(),
            expires_at: Utc::now() + chrono::Duration::seconds(3600),
            scope: "user-read-private".into(),
            owner_id: "yudhyapw".into(),
        }))
    }
    async fn upsert(&self, _: TokenRecord) -> Result<(), RepoError> {
        Ok(())
    }
    async fn delete(&self) -> Result<(), RepoError> {
        Ok(())
    }
}

struct UnusedExchanger;
#[async_trait]
impl TokenExchanger for UnusedExchanger {
    async fn refresh(&self, _: &str) -> Result<RefreshedTokens, TokenExchangeError> {
        unimplemented!()
    }
    async fn exchange_code(&self, _: &str, _: &str) -> Result<RefreshedTokens, TokenExchangeError> {
        unimplemented!()
    }
}

/// Non-default intervals so the test is sensitive to the kind → field
/// mapping (a swap between `now` and `recent` would silently pass under
/// the default config).
fn intervals() -> SchedulerIntervals {
    SchedulerIntervals {
        now: Duration::from_millis(1_000),
        recent: Duration::from_millis(2_500),
        top: Duration::from_millis(7_000),
        profile: Duration::from_millis(11_000),
        playlists: Duration::from_millis(13_000),
    }
}

fn cfg() -> Config {
    Config {
        spotify_client_id: "cid".into(),
        spotify_client_secret: "secret".into(),
        spotify_redirect_uri: "https://x/callback".into(),
        owner_spotify_user_id: "yudhyapw".into(),
        auth_basic_username: "owner".into(),
        auth_basic_password: "pw".into(),
        database_url: "sqlite::memory:".into(),
        mock_data: false,
        scheduler: SchedulerConfig {
            intervals: intervals(),
            idle_threshold: Duration::from_secs(60),
        },
    }
}

fn build(routes: Vec<(&str, Value)>) -> (axum::Router, AppState, Arc<AuthState>) {
    let spotify = Arc::new(RoutedSpotify::new(routes));
    let spotify_dyn: Arc<dyn SpotifyClient> = spotify;
    let auth_state = Arc::new(AuthState::new());
    let state = AppState::new_for_test(
        Arc::new(cfg()),
        Arc::new(MemRepo),
        spotify_dyn,
        Arc::new(UnusedExchanger),
        auth_state.clone(),
        Arc::new(StateStore::new()),
    );
    (app(state.clone()), state, auth_state)
}

async fn get_body(router: &axum::Router, path: &str) -> (StatusCode, Value) {
    let resp = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

// ---- cold-start path (FE's first request after boot) ------------------
//
// fetch_and_map is the single bake-in site — both the scheduler tick and
// the handler's cold-start fallback flow through it. Exercising the
// cold-start path therefore pins the field for the scheduler-stored
// path too (every snapshot the scheduler writes goes through the same
// helper, so what the cold-start serves is the same shape the scheduler
// would store).

#[tokio::test]
async fn cold_start_now_response_carries_refresh_ms() {
    let now = json!({
        "is_playing": true,
        "item": {
            "name": "T", "duration_ms": 1000,
            "artists": [{"name": "A"}],
            "album": {"name": "AL", "images": [{"url": "https://i/c.jpg"}]},
        },
        "progress_ms": 0,
    });
    let (router, _state, _auth) = build(vec![("/v1/me/player", now)]);
    let (status, body) = get_body(&router, "/v1/now").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["refresh_ms"],
        json!(1_000),
        "cold-start /v1/now must carry refresh_ms matching SchedulerIntervals.now",
    );
}

#[tokio::test]
async fn cold_start_recent_response_carries_refresh_ms() {
    let recent = json!({"items": []});
    let (router, _state, _auth) = build(vec![("/v1/me/player/recently-played?limit=20", recent)]);
    let (status, body) = get_body(&router, "/v1/recent").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["refresh_ms"], json!(2_500));
}

#[tokio::test]
async fn cold_start_top_response_carries_refresh_ms() {
    let top = json!({"items": []});
    let (router, _state, _auth) = build(vec![(
        "/v1/me/top/tracks?time_range=short_term&limit=10",
        top,
    )]);
    let (status, body) = get_body(&router, "/v1/top/tracks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["refresh_ms"], json!(7_000));
}

#[tokio::test]
async fn cold_start_profile_response_carries_refresh_ms() {
    let me = json!({
        "id": "yudyack", "display_name": "Y",
        "followers": {"total": 0}, "images": [],
        "external_urls": {"spotify": ""},
    });
    let (router, _state, _auth) = build(vec![("/v1/me", me)]);
    let (status, body) = get_body(&router, "/v1/profile").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["refresh_ms"], json!(11_000));
}

#[tokio::test]
async fn cold_start_playlists_response_carries_refresh_ms() {
    let playlists = json!({"items": [], "total": 0});
    let (router, _state, _auth) = build(vec![("/v1/me/playlists?limit=20", playlists)]);
    let (status, body) = get_body(&router, "/v1/playlists").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["refresh_ms"], json!(13_000));
}

// ---- error paths do NOT carry refresh_ms ------------------------------

#[tokio::test]
async fn needs_reauth_503_does_not_carry_refresh_ms() {
    let (router, _state, auth_state) = build(vec![]);
    auth_state.set_needs_reauth();
    let (status, body) = get_body(&router, "/v1/now").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], json!("needs_reauth"));
    assert!(
        body.get("refresh_ms").is_none(),
        "error responses must not carry refresh_ms — no extrapolation applies",
    );
}

#[tokio::test]
async fn spotify_paused_503_does_not_carry_refresh_ms() {
    let (router, state, _auth) = build(vec![]);
    state.spotify_toggle.disable();
    let (status, body) = get_body(&router, "/v1/now").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], json!("spotify_paused"));
    assert!(body.get("refresh_ms").is_none());
}
