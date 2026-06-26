//! Criterion 17 — `GET /v1/now` returns `{playing:false}` (200, not 500)
//! when Spotify returns 204 (no active device). Also pins the active-device
//! shape and the cache + needs_reauth behaviors.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use music_api::app::state_store::StateStore;
use music_api::config::Config;
use music_api::domain::auth_state::AuthState;
use music_api::domain::oauth_client::{RefreshedTokens, TokenExchangeError, TokenExchanger};
use music_api::domain::spotify::{SpotifyClient, SpotifyError};
use music_api::domain::tokens::{RepoError, TokenRecord, TokenRepository};
use music_api::{app, AppState};
use serde_json::{json, Value};
use tower::util::ServiceExt;

// ---- fixtures ----------------------------------------------------------

/// Returns a programmed Option<Value> on each call and counts calls.
struct ProgrammedSpotify {
    response: Option<Value>,
    calls: AtomicUsize,
}

impl ProgrammedSpotify {
    fn playing(payload: Value) -> Self {
        Self {
            response: Some(payload),
            calls: AtomicUsize::new(0),
        }
    }
    fn nothing() -> Self {
        Self {
            response: None,
            calls: AtomicUsize::new(0),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SpotifyClient for ProgrammedSpotify {
    async fn get_json(&self, _path: &str, _token: &str) -> Result<Option<Value>, SpotifyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.response.clone())
    }
}

struct MemRepo {
    rec: Mutex<Option<TokenRecord>>,
}

#[async_trait]
impl TokenRepository for MemRepo {
    async fn get(&self) -> Result<Option<TokenRecord>, RepoError> {
        Ok(self.rec.lock().unwrap().clone())
    }
    async fn upsert(&self, t: TokenRecord) -> Result<(), RepoError> {
        *self.rec.lock().unwrap() = Some(t);
        Ok(())
    }
    async fn delete(&self) -> Result<(), RepoError> {
        *self.rec.lock().unwrap() = None;
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

fn seed() -> TokenRecord {
    TokenRecord {
        access_token: "ACCESS".into(),
        refresh_token: "REFRESH".into(),
        expires_at: Utc::now() + Duration::seconds(3600),
        scope: "user-read-playback-state".into(),
        owner_id: "yudhyapw".into(),
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
    }
}

fn player_payload() -> Value {
    json!({
        "is_playing": true,
        "progress_ms": 12345,
        "item": {
            "name": "Track Name",
            "duration_ms": 240000,
            "artists": [{"name": "Artist One"}, {"name": "Artist Two"}],
            "album": {
                "name": "Album Name",
                "images": [
                    {"url": "https://i.scdn.co/image/big",   "height": 640, "width": 640},
                    {"url": "https://i.scdn.co/image/small", "height": 64,  "width": 64}
                ]
            }
        },
        "device": {"name": "Yudhya's MacBook", "type": "Computer"}
    })
}

fn build_app(
    spotify: Arc<ProgrammedSpotify>,
    auth_state: Arc<AuthState>,
) -> (axum::Router, Arc<ProgrammedSpotify>) {
    let tokens: Arc<dyn TokenRepository> = Arc::new(MemRepo {
        rec: Mutex::new(Some(seed())),
    });
    let spotify_dyn: Arc<dyn SpotifyClient> = spotify.clone();
    let oauth: Arc<dyn TokenExchanger> = Arc::new(UnusedExchanger);
    let state = AppState::new_for_test(
        Arc::new(cfg()),
        tokens,
        spotify_dyn,
        oauth,
        auth_state,
        Arc::new(StateStore::new()),
    );
    (app(state), spotify)
}

async fn get(router: &axum::Router, path: &str) -> (StatusCode, Value) {
    let resp = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

// ---- tests -------------------------------------------------------------

#[tokio::test]
async fn now_with_204_returns_playing_false_200() {
    let (router, _) = build_app(
        Arc::new(ProgrammedSpotify::nothing()),
        Arc::new(AuthState::new()),
    );
    let (status, body) = get(&router, "/v1/now").await;
    assert_eq!(status, StatusCode::OK, "criterion 17: 204 must NOT be 500");
    assert_eq!(body, json!({"playing": false}));
}

#[tokio::test]
async fn now_with_active_device_maps_spec_shape_from_me_player() {
    let (router, _) = build_app(
        Arc::new(ProgrammedSpotify::playing(player_payload())),
        Arc::new(AuthState::new()),
    );
    let (status, body) = get(&router, "/v1/now").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["playing"], json!(true));
    assert_eq!(body["track"], json!("Track Name"));
    assert_eq!(body["artist"], json!("Artist One, Artist Two"));
    assert_eq!(body["album"], json!("Album Name"));
    assert_eq!(body["cover"], json!("https://i.scdn.co/image/big"));
    assert_eq!(body["progress_ms"], json!(12345));
    assert_eq!(body["duration_ms"], json!(240000));
    assert_eq!(body["device"], json!("Yudhya's MacBook"));
}

#[tokio::test]
async fn now_second_request_hits_cache_no_second_spotify_call() {
    let (router, counter) = build_app(
        Arc::new(ProgrammedSpotify::playing(player_payload())),
        Arc::new(AuthState::new()),
    );
    let (s1, _) = get(&router, "/v1/now").await;
    let (s2, _) = get(&router, "/v1/now").await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(counter.calls(), 1, "criterion 11: 10s TTL — 2nd request from cache");
}

#[tokio::test]
async fn now_returns_503_needs_reauth_when_auth_state_set() {
    let auth_state = Arc::new(AuthState::new());
    auth_state.set_needs_reauth();
    let (router, counter) = build_app(
        Arc::new(ProgrammedSpotify::playing(player_payload())),
        auth_state,
    );
    let (status, body) = get(&router, "/v1/now").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, json!({"error": "needs_reauth"}));
    assert_eq!(counter.calls(), 0, "needs_reauth must short-circuit BEFORE the Spotify call");
}
