//! /admin/spotify endpoint tests. Three things to lock in:
//! 1. Missing / wrong Basic auth → 401 with WWW-Authenticate, toggle not flipped.
//! 2. Owner-authenticated POST flips the in-memory toggle state.
//! 3. GET reports the current state without flipping anything.
//!
//! We do NOT re-prove constant-time comparison here — `tests/auth_constant_time.rs`
//! covers that for the same shared helper.

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::Utc;
use music_api::app::state_store::StateStore;
use music_api::config::Config;
use music_api::domain::auth_state::AuthState;
use music_api::domain::oauth_client::{RefreshedTokens, TokenExchangeError, TokenExchanger};
use music_api::domain::spotify::{SpotifyClient, SpotifyError};
use music_api::domain::tokens::{RepoError, TokenRecord, TokenRepository};
use music_api::{app, AppState};
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

struct NoopSpotify;
#[async_trait]
impl SpotifyClient for NoopSpotify {
    async fn get_json(&self, _: &str, _: &str) -> Result<Option<Value>, SpotifyError> {
        Err(SpotifyError::Transport("unused".into()))
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

struct NoopExchanger;
#[async_trait]
impl TokenExchanger for NoopExchanger {
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

fn build() -> (axum::Router, AppState) {
    let state = AppState::new_for_test(
        Arc::new(cfg()),
        Arc::new(MemRepo),
        Arc::new(NoopSpotify),
        Arc::new(NoopExchanger),
        Arc::new(AuthState::new()),
        Arc::new(StateStore::new()),
    );
    (app(state.clone()), state)
}

fn basic_header(user: &str, pw: &str) -> String {
    format!("Basic {}", B64.encode(format!("{user}:{pw}")))
}

async fn send(
    router: &axum::Router,
    method: Method,
    path: &str,
    auth: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(a) = auth {
        req = req.header(header::AUTHORIZATION, a);
    }
    let resp = router
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

// ---- auth ---------------------------------------------------------------

#[tokio::test]
async fn admin_spotify_without_auth_is_401_and_does_not_flip() {
    let (router, state) = build();
    assert!(state.spotify_toggle.is_enabled(), "precondition: enabled");

    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/admin/spotify/disable")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(
        resp.headers().contains_key(header::WWW_AUTHENTICATE),
        "401 must carry WWW-Authenticate so curl/browser can re-challenge",
    );
    assert!(
        state.spotify_toggle.is_enabled(),
        "unauthorized POST must NOT flip the toggle",
    );
}

#[tokio::test]
async fn admin_spotify_with_wrong_password_is_401() {
    let (router, _state) = build();
    let (status, _) = send(
        &router,
        Method::GET,
        "/admin/spotify",
        Some(&basic_header("owner", "WRONG")),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ---- happy path --------------------------------------------------------

#[tokio::test]
async fn get_admin_spotify_reports_current_state() {
    let (router, state) = build();
    let auth = basic_header("owner", "pw");

    let (status, body) = send(&router, Method::GET, "/admin/spotify", Some(&auth)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], Value::Bool(true));

    state.spotify_toggle.disable();
    let (_, body) = send(&router, Method::GET, "/admin/spotify", Some(&auth)).await;
    assert_eq!(
        body["enabled"],
        Value::Bool(false),
        "GET must reflect the new state after a flip"
    );
    assert!(
        !state.spotify_toggle.is_enabled(),
        "GET must not flip on its own",
    );
}

#[tokio::test]
async fn post_disable_then_enable_flips_toggle() {
    let (router, state) = build();
    let auth = basic_header("owner", "pw");

    let (status, body) = send(&router, Method::POST, "/admin/spotify/disable", Some(&auth)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], Value::Bool(false));
    assert!(!state.spotify_toggle.is_enabled());

    let (status, body) = send(&router, Method::POST, "/admin/spotify/enable", Some(&auth)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], Value::Bool(true));
    assert!(state.spotify_toggle.is_enabled());
}

#[tokio::test]
async fn post_disable_is_idempotent() {
    let (router, state) = build();
    let auth = basic_header("owner", "pw");
    for _ in 0..3 {
        let (status, body) =
            send(&router, Method::POST, "/admin/spotify/disable", Some(&auth)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["enabled"], Value::Bool(false));
    }
    assert!(!state.spotify_toggle.is_enabled());
}

// ---- healthz surface ---------------------------------------------------

#[tokio::test]
async fn healthz_surfaces_spotify_enabled_flag() {
    let (router, state) = build();
    let (_, body) = send(&router, Method::GET, "/healthz", None).await;
    assert_eq!(body["spotify_enabled"], Value::Bool(true));

    state.spotify_toggle.disable();
    let (_, body) = send(&router, Method::GET, "/healthz", None).await;
    assert_eq!(body["spotify_enabled"], Value::Bool(false));
}
