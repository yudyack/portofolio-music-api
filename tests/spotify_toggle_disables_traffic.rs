//! Gate-side tests for the SpotifyToggle kill switch.
//!
//! Three behaviors to prove:
//! 1. /v1/* with a populated snapshot returns the snapshot even when paused
//!    (no Spotify call).
//! 2. /v1/* with an empty snapshot returns 503 `{error:"spotify_paused"}`
//!    when paused (no cold-start fetch).
//! 3. The scheduler tick loop skips its fetch while paused, but resumes
//!    within one interval after `enable()`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use music_api::app::scheduler::spawn_one;
use music_api::app::snapshots::EndpointKind;
use music_api::app::state_store::StateStore;
use music_api::config::Config;
use music_api::domain::auth_state::AuthState;
use music_api::domain::oauth_client::{RefreshedTokens, TokenExchangeError, TokenExchanger};
use music_api::domain::spotify::{SpotifyClient, SpotifyError};
use music_api::domain::tokens::{RepoError, TokenRecord, TokenRepository};
use music_api::{app, AppState};
use serde_json::{json, Value};
use tower::util::ServiceExt;

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
        scheduler: Default::default(),
    }
}

fn build(routes: Vec<(&str, Value)>) -> (axum::Router, Arc<RoutedSpotify>, AppState) {
    let spotify = Arc::new(RoutedSpotify::new(routes));
    let spotify_dyn: Arc<dyn SpotifyClient> = spotify.clone();
    let state = AppState::new_for_test(
        Arc::new(cfg()),
        Arc::new(MemRepo),
        spotify_dyn,
        Arc::new(UnusedExchanger),
        Arc::new(AuthState::new()),
        Arc::new(StateStore::new()),
    );
    (app(state.clone()), spotify, state)
}

async fn get_body(router: &axum::Router, path: &str) -> (StatusCode, Value) {
    let resp = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

// (1) paused + snapshot present → serve snapshot, no Spotify call
#[tokio::test]
async fn paused_with_snapshot_serves_cached_without_calling_spotify() {
    let (router, spotify, state) = build(vec![]);
    state.snapshots.set(
        EndpointKind::Now,
        Some(json!({"playing": false, "_seed": true})),
    );
    state.spotify_toggle.disable();

    let (status, body) = get_body(&router, "/v1/now").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["_seed"], json!(true));
    assert_eq!(
        spotify.calls.load(Ordering::SeqCst),
        0,
        "paused state must not trigger any Spotify call",
    );
}

// (2) paused + snapshot absent → 503 spotify_paused, no cold-start fetch
#[tokio::test]
async fn paused_without_snapshot_returns_503_spotify_paused() {
    let (router, spotify, state) = build(vec![("/v1/me/player", json!({"is_playing": false}))]);
    state.spotify_toggle.disable();

    let (status, body) = get_body(&router, "/v1/now").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], json!("spotify_paused"));
    assert_eq!(
        spotify.calls.load(Ordering::SeqCst),
        0,
        "paused state must skip the cold-start fetch entirely",
    );
}

// (3) scheduler tick skips fetch while paused; resumes after enable()
#[tokio::test]
async fn scheduler_tick_skips_fetch_while_paused_then_resumes() {
    let (_, spotify, state) = build(vec![("/v1/me/player", json!({"is_playing": false}))]);
    // Keep the activity gate open across the whole test so we're isolating
    // the toggle gate, not racing the idle-park branch.
    state.activity.touch();
    state.spotify_toggle.disable();

    spawn_one(state.clone(), EndpointKind::Now, Duration::from_millis(5));

    // Several intervals worth of wall-clock. Paused → no fetches.
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        spotify.calls.load(Ordering::SeqCst),
        0,
        "paused loop must skip Spotify; got {}",
        spotify.calls.load(Ordering::SeqCst),
    );

    // Flip back on — within ~one interval the loop ticks and stores.
    state.spotify_toggle.enable();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        spotify.calls.load(Ordering::SeqCst) >= 1,
        "re-enable must resume Spotify calls; got {}",
        spotify.calls.load(Ordering::SeqCst),
    );
    assert!(
        state.snapshots.get(EndpointKind::Now).is_some(),
        "resumed loop must populate snapshot",
    );
}
