//! Criteria 11 + 18 (profile subset) + 6 (read-side guard) for `/v1/profile`.
//!
//! Pinned by behavior, no network:
//! - shape: `{display_name, handle, avatar, followers, profile_url}` mapped
//!   from `/me`. (`following`/`playlists_count` need separate calls; deferred.)
//! - cache: second request within TTL does NOT call Spotify (call counter).
//! - needs_reauth: handler returns 503 + `{error:"needs_reauth"}` and Spotify
//!   is NOT called when `AuthState::needs_reauth()` is set.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

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
use std::sync::Mutex;
use tower::util::ServiceExt;

// ---- fixtures ----------------------------------------------------------

struct CountingSpotify {
    calls: AtomicUsize,
    response: Value,
}

impl CountingSpotify {
    fn new(response: Value) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            response,
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SpotifyClient for CountingSpotify {
    async fn get_json(&self, _path: &str, _token: &str) -> Result<Option<Value>, SpotifyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Some(self.response.clone()))
    }
}

struct MemRepo {
    rec: Mutex<Option<TokenRecord>>,
}

impl MemRepo {
    fn with(rec: TokenRecord) -> Self {
        Self {
            rec: Mutex::new(Some(rec)),
        }
    }
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

/// Stub exchanger never used by these tests (no 401 path), but required by
/// AppState wiring.
struct UnusedExchanger;

#[async_trait]
impl TokenExchanger for UnusedExchanger {
    async fn refresh(&self, _: &str) -> Result<RefreshedTokens, TokenExchangeError> {
        unimplemented!("not exercised: profile tests do not 401")
    }
    async fn exchange_code(&self, _: &str, _: &str) -> Result<RefreshedTokens, TokenExchangeError> {
        unimplemented!("not exercised")
    }
}

fn seed_token() -> TokenRecord {
    TokenRecord {
        access_token: "ACCESS".into(),
        refresh_token: "REFRESH".into(),
        expires_at: Utc::now() + Duration::seconds(3600),
        scope: "user-read-private".into(),
        owner_id: "yudhyapw".into(),
    }
}

fn me_payload() -> Value {
    json!({
        "id": "yudhyapw",
        "display_name": "Yudhya",
        "followers": { "total": 42 },
        "images": [
            { "url": "https://i.scdn.co/image/large", "height": 640, "width": 640 },
            { "url": "https://i.scdn.co/image/small", "height": 64, "width": 64 }
        ],
        "external_urls": { "spotify": "https://open.spotify.com/user/yudhyapw" }
    })
}

fn test_config() -> Config {
    Config {
        spotify_client_id: "cid".into(),
        spotify_client_secret: "secret".into(),
        spotify_redirect_uri: "https://musicapi.yudhyapw.com/auth/spotify/callback".into(),
        owner_spotify_user_id: "yudhyapw".into(),
        auth_basic_username: "owner".into(),
        auth_basic_password: "pw".into(),
        database_url: "sqlite::memory:".into(),
    }
}

fn build_app(
    spotify: Arc<CountingSpotify>,
    auth_state: Arc<AuthState>,
) -> (axum::Router, Arc<CountingSpotify>) {
    let tokens: Arc<dyn TokenRepository> = Arc::new(MemRepo::with(seed_token()));
    let spotify_dyn: Arc<dyn SpotifyClient> = spotify.clone();
    let oauth: Arc<dyn TokenExchanger> = Arc::new(UnusedExchanger);
    let state = AppState::new_for_test(
        Arc::new(test_config()),
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
async fn profile_returns_spec_shape_mapped_from_me() {
    let spotify = Arc::new(CountingSpotify::new(me_payload()));
    let (router, _) = build_app(spotify, Arc::new(AuthState::new()));

    let (status, body) = get(&router, "/v1/profile").await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(body["display_name"], json!("Yudhya"));
    assert_eq!(body["handle"], json!("yudhyapw"));
    assert_eq!(body["avatar"], json!("https://i.scdn.co/image/large"));
    assert_eq!(body["followers"], json!(42));
    assert_eq!(
        body["profile_url"],
        json!("https://open.spotify.com/user/yudhyapw")
    );
}

#[tokio::test]
async fn profile_second_request_hits_cache_no_second_spotify_call() {
    let spotify = Arc::new(CountingSpotify::new(me_payload()));
    let (router, counter) = build_app(spotify, Arc::new(AuthState::new()));

    let (s1, _) = get(&router, "/v1/profile").await;
    let after_first = counter.calls();
    let (s2, _) = get(&router, "/v1/profile").await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    // First request fires multiple Spotify calls (me + following + playlists);
    // exact count varies as the aggregation evolves. The cache invariant is
    // that the SECOND request adds zero.
    assert_eq!(
        counter.calls(),
        after_first,
        "second request must serve from cache (no new Spotify calls)",
    );
}

#[tokio::test]
async fn profile_returns_503_needs_reauth_when_auth_state_set() {
    let spotify = Arc::new(CountingSpotify::new(me_payload()));
    let auth_state = Arc::new(AuthState::new());
    auth_state.set_needs_reauth();
    let (router, counter) = build_app(spotify, auth_state);

    let (status, body) = get(&router, "/v1/profile").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, json!({"error": "needs_reauth"}));
    assert_eq!(
        counter.calls(),
        0,
        "needs_reauth must short-circuit BEFORE the Spotify call",
    );
}

#[tokio::test]
async fn healthz_reports_needs_reauth_status_when_auth_state_set() {
    let spotify = Arc::new(CountingSpotify::new(me_payload()));
    let auth_state = Arc::new(AuthState::new());
    auth_state.set_needs_reauth();
    let (router, _) = build_app(spotify, auth_state);

    let (status, body) = get(&router, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], json!("needs_reauth"));
}
