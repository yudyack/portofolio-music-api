//! Mock-mode (MOCK_DATA=1) acceptance.
//!
//! Verifies the dev seam end-to-end: when `config.mock_data` is true,
//! - /v1/* responses carry `_mock: true` (so the leptos frontend can
//!   render a "MOCK DATA" banner without a separate request),
//! - /healthz body also reports `mock_mode: true`,
//! - MockSpotifyClient returns the embedded fixtures so panel shapes
//!   match the real wire contract.
//!
//! Real-mode (`mock_data: false`) must NOT carry `_mock` at all — checked
//! here as a regression guard against accidental always-on tagging.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use music_api::app::state_store::StateStore;
use music_api::config::Config;
use music_api::domain::auth_state::AuthState;
use music_api::domain::oauth_client::{RefreshedTokens, TokenExchangeError, TokenExchanger};
use music_api::domain::spotify::SpotifyClient;
use music_api::domain::tokens::{RepoError, TokenRecord, TokenRepository};
use music_api::infra::mock_spotify_client::MockSpotifyClient;
use music_api::{app, AppState};
use serde_json::Value;
use std::sync::Mutex;
use tower::util::ServiceExt;

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

fn token() -> TokenRecord {
    TokenRecord {
        access_token: "ACCESS".into(),
        refresh_token: "REFRESH".into(),
        expires_at: Utc::now() + Duration::seconds(3600),
        scope: "user-read-private".into(),
        owner_id: "yudyack_dev".into(),
    }
}

fn cfg(mock: bool) -> Config {
    Config {
        spotify_client_id: "cid".into(),
        spotify_client_secret: "secret".into(),
        spotify_redirect_uri: "https://x/callback".into(),
        owner_spotify_user_id: "yudyack_dev".into(),
        auth_basic_username: "owner".into(),
        auth_basic_password: "pw".into(),
        database_url: "sqlite::memory:".into(),
        mock_data: mock,
    }
}

fn build(mock: bool) -> axum::Router {
    let tokens: Arc<dyn TokenRepository> = Arc::new(MemRepo {
        rec: Mutex::new(Some(token())),
    });
    let spotify: Arc<dyn SpotifyClient> = Arc::new(MockSpotifyClient::new());
    let state = AppState::new_for_test(
        Arc::new(cfg(mock)),
        tokens,
        spotify,
        Arc::new(UnusedExchanger),
        Arc::new(AuthState::new()),
        Arc::new(StateStore::new()),
    );
    app(state)
}

async fn get(router: &axum::Router, path: &str) -> (StatusCode, Value) {
    let resp = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

// ---- mock-mode tag --------------------------------------------------------

#[tokio::test]
async fn every_v1_response_carries_underscore_mock_true_in_mock_mode() {
    let app = build(true);
    for path in [
        "/v1/profile",
        "/v1/now",
        "/v1/recent",
        "/v1/top/tracks",
        "/v1/playlists",
    ] {
        let (status, body) = get(&app, path).await;
        assert_eq!(status, StatusCode::OK, "{path} should succeed in mock mode");
        assert_eq!(
            body.get("_mock"),
            Some(&Value::Bool(true)),
            "{path}: _mock:true must be present in mock mode",
        );
    }
}

#[tokio::test]
async fn no_v1_response_carries_underscore_mock_in_real_mode() {
    let app = build(false);
    for path in [
        "/v1/profile",
        "/v1/now",
        "/v1/recent",
        "/v1/top/tracks",
        "/v1/playlists",
    ] {
        let (status, body) = get(&app, path).await;
        assert_eq!(status, StatusCode::OK, "{path} should succeed");
        assert!(
            body.get("_mock").is_none(),
            "{path}: _mock must NOT appear in real mode (got {body})",
        );
    }
}

// ---- mock fixtures shape (real Spotify data shapes) -----------------------

#[tokio::test]
async fn mock_profile_renders_full_shape_from_fixtures() {
    let app = build(true);
    let (status, body) = get(&app, "/v1/profile").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["handle"], "yudyack_dev");
    assert_eq!(body["display_name"], "Yudhya (mock)");
    assert_eq!(body["followers"], 64);
    assert_eq!(body["following"], 17, "from me_following.json artists.total");
    assert_eq!(body["playlists_count"], 9, "from me_playlists.json total");
    assert!(
        body["avatar"].as_str().unwrap().starts_with("https://i.scdn.co/"),
        "avatar should be a Spotify-CDN-shaped URL",
    );
    assert_eq!(body["_mock"], Value::Bool(true));
}

#[tokio::test]
async fn mock_now_renders_playing_track() {
    let app = build(true);
    let (status, body) = get(&app, "/v1/now").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["playing"], Value::Bool(true));
    assert_eq!(body["track"], "Midnight City");
    assert_eq!(body["artist"], "M83");
    assert_eq!(body["album"], "Hurry Up, We're Dreaming");
    assert_eq!(body["device"], "Yudhya's MacBook (mock)");
}

#[tokio::test]
async fn mock_top_tracks_has_one_indexed_rank_and_ten_items() {
    let app = build(true);
    let (status, body) = get(&app, "/v1/top/tracks").await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 10);
    assert_eq!(items[0]["rank"], 1);
    assert_eq!(items[9]["rank"], 10);
}

// ---- healthz mock_mode indicator -----------------------------------------

#[tokio::test]
async fn healthz_reports_mock_mode_true_when_mock_data_set() {
    let app = build(true);
    let (status, body) = get(&app, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mock_mode"], Value::Bool(true));
}

#[tokio::test]
async fn healthz_reports_mock_mode_false_in_real_mode() {
    let app = build(false);
    let (status, body) = get(&app, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mock_mode"], Value::Bool(false));
}
