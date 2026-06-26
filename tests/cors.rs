//! Criterion 14 — CORS on /v1/* only.
//!
//! Allowlist: https://yudhyapw.com, https://www.yudhyapw.com, plus
//! http://127.0.0.1:* for dev. /auth/* and /healthz emit no CORS headers
//! (the bootstrap is browser-redirect-only; healthz is operational).

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN;
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

struct DummySpotify;
#[async_trait]
impl SpotifyClient for DummySpotify {
    async fn get_json(&self, _path: &str, _t: &str) -> Result<Option<Value>, SpotifyError> {
        Ok(Some(json!({
            "id": "yudhyapw",
            "display_name": "Yudhya",
            "followers": {"total": 1},
            "images": [{"url": "https://i/img", "height": 640, "width": 640}],
            "external_urls": {"spotify": "https://open.spotify.com/user/yudhyapw"}
        })))
    }
}

struct MemRepo;
#[async_trait]
impl TokenRepository for MemRepo {
    async fn get(&self) -> Result<Option<TokenRecord>, RepoError> {
        Ok(Some(TokenRecord {
            access_token: "ACCESS".into(),
            refresh_token: "REFRESH".into(),
            expires_at: Utc::now() + Duration::seconds(3600),
            scope: "user-read-private".into(),
            owner_id: "yudhyapw".into(),
        }))
    }
    async fn upsert(&self, _: TokenRecord) -> Result<(), RepoError> { Ok(()) }
    async fn delete(&self) -> Result<(), RepoError> { Ok(()) }
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
    }
}

fn build_app() -> axum::Router {
    let tokens: Arc<dyn TokenRepository> = Arc::new(MemRepo);
    let spotify: Arc<dyn SpotifyClient> = Arc::new(DummySpotify);
    let oauth: Arc<dyn TokenExchanger> = Arc::new(UnusedExchanger);
    let state = AppState::new_for_test(
        Arc::new(cfg()),
        tokens,
        spotify,
        oauth,
        Arc::new(AuthState::new()),
        Arc::new(StateStore::new()),
    );
    app(state)
}

async fn send(router: &axum::Router, req: Request<Body>) -> axum::http::Response<Body> {
    router.clone().oneshot(req).await.unwrap()
}

fn get_with_origin(path: &str, origin: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .header("origin", origin)
        .body(Body::empty())
        .unwrap()
}

fn options_preflight(path: &str, origin: &str) -> Request<Body> {
    Request::builder()
        .method("OPTIONS")
        .uri(path)
        .header("origin", origin)
        .header("access-control-request-method", "GET")
        .body(Body::empty())
        .unwrap()
}

// ---- tests -------------------------------------------------------------

#[tokio::test]
async fn v1_endpoint_allows_yudhyapw_origin() {
    let app = build_app();
    let resp = send(&app, get_with_origin("/v1/profile", "https://yudhyapw.com")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let cors_header = resp.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN);
    assert_eq!(
        cors_header.and_then(|h| h.to_str().ok()),
        Some("https://yudhyapw.com"),
        "ACAO must echo the allowed origin",
    );
}

#[tokio::test]
async fn v1_endpoint_allows_www_yudhyapw_origin() {
    let app = build_app();
    let resp = send(&app, get_with_origin("/v1/profile", "https://www.yudhyapw.com")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|h| h.to_str().ok()),
        Some("https://www.yudhyapw.com"),
    );
}

#[tokio::test]
async fn v1_endpoint_blocks_evil_origin() {
    let app = build_app();
    let resp = send(&app, get_with_origin("/v1/profile", "https://evil.example")).await;
    // Request itself still served (CORS is a browser-side enforcement) but
    // ACAO is absent so the browser blocks the response.
    assert!(
        resp.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN).is_none(),
        "evil origin must NOT receive ACAO",
    );
}

#[tokio::test]
async fn v1_endpoint_handles_preflight_options() {
    let app = build_app();
    let resp = send(&app, options_preflight("/v1/profile", "https://yudhyapw.com")).await;
    // Preflight returns 200/204 with ACAO header.
    assert!(resp.status().is_success() || resp.status() == StatusCode::NO_CONTENT);
    assert_eq!(
        resp.headers()
            .get(ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|h| h.to_str().ok()),
        Some("https://yudhyapw.com"),
    );
}

#[tokio::test]
async fn healthz_does_not_emit_cors_headers() {
    let app = build_app();
    let resp = send(&app, get_with_origin("/healthz", "https://yudhyapw.com")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN).is_none(),
        "spec §5.7: /healthz must NOT emit CORS headers",
    );
}

#[tokio::test]
async fn auth_login_does_not_emit_cors_headers() {
    let app = build_app();
    // Hit /auth/spotify/login WITHOUT Basic auth → 401, but the CORS header
    // is what we're checking (not the auth outcome).
    let resp = send(
        &app,
        get_with_origin("/auth/spotify/login", "https://yudhyapw.com"),
    )
    .await;
    assert!(
        resp.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN).is_none(),
        "spec §5.7: /auth/* must NOT emit CORS headers",
    );
}
