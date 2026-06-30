//! Wire contract for the `request_id_layer` middleware:
//!
//! 1. A request with no `X-Request-Id` header gets one generated and
//!    echoed back. The generated value is 16 lowercase-hex chars.
//! 2. A request with a valid inbound `X-Request-Id` (alnum / `-` / `_`,
//!    1..=128 chars) gets that exact value echoed back — the FE / a
//!    load balancer / a curl-based ops session can correlate.
//! 3. A request with an invalid inbound header (control chars, too
//!    long, empty) gets a generated id back, not the hostile input.
//! 4. The header lands on every endpoint — `/healthz`, `/auth/*`,
//!    `/admin/*` (under its own auth gate), `/v1/*`. We exercise the
//!    public endpoints; `/admin/*` is covered by the auth-required
//!    case (the 401 response itself carries the id).

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderValue, Request, StatusCode};
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

fn build() -> axum::Router {
    let state = AppState::new_for_test(
        Arc::new(cfg()),
        Arc::new(MemRepo),
        Arc::new(NoopSpotify),
        Arc::new(UnusedExchanger),
        Arc::new(AuthState::new()),
        Arc::new(StateStore::new()),
    );
    app(state)
}

async fn send(
    router: &axum::Router,
    path: &str,
    inbound_id: Option<&str>,
) -> (StatusCode, Option<HeaderValue>) {
    let mut req = Request::builder().method("GET").uri(path);
    if let Some(id) = inbound_id {
        req = req.header("x-request-id", id);
    }
    let resp = router
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let header = resp.headers().get("x-request-id").cloned();
    (status, header)
}

fn is_16_hex(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

// ---- generation -------------------------------------------------------

#[tokio::test]
async fn missing_header_generates_16_hex_id() {
    let router = build();
    let (status, id) = send(&router, "/healthz", None).await;
    assert_eq!(status, StatusCode::OK);
    let id = id.expect("x-request-id must be set on response");
    let id_str = id.to_str().expect("ascii");
    assert!(
        is_16_hex(id_str),
        "generated id must be 16 lowercase hex chars, got {id_str:?}",
    );
}

#[tokio::test]
async fn each_request_gets_a_distinct_generated_id() {
    let router = build();
    let (_, a) = send(&router, "/healthz", None).await;
    let (_, b) = send(&router, "/healthz", None).await;
    assert_ne!(
        a, b,
        "two consecutive missing-header requests must not collide on the generated id",
    );
}

// ---- echo ------------------------------------------------------------

#[tokio::test]
async fn valid_inbound_id_is_echoed_back_verbatim() {
    let router = build();
    let (_, id) = send(&router, "/healthz", Some("ops-trace-abc_123")).await;
    assert_eq!(
        id.unwrap().to_str().unwrap(),
        "ops-trace-abc_123",
        "valid inbound id must be echoed back so the caller can correlate",
    );
}

#[tokio::test]
async fn echo_applies_to_v1_endpoints_too() {
    let router = build();
    let (_, id) = send(&router, "/v1/now", Some("FE-trace-42")).await;
    assert_eq!(id.unwrap().to_str().unwrap(), "FE-trace-42");
}

#[tokio::test]
async fn echo_applies_to_unauthorized_admin_response() {
    // The 401 from /admin/* still flows through the request-id layer, so
    // ops can grep their failed admin attempt out of the log.
    let router = build();
    let (status, id) = send(&router, "/admin/spotify", Some("ops-flip-7")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(id.unwrap().to_str().unwrap(), "ops-flip-7");
}

// ---- hostile input regenerates ---------------------------------------

#[tokio::test]
async fn header_with_disallowed_chars_is_dropped_and_regenerated() {
    // Our `is_valid_request_id` rejects anything outside [A-Za-z0-9_-].
    // Space is a plain non-allowed character that's still a legal HTTP
    // header byte, so it reaches the middleware (true control chars like
    // \n / \r get rejected by `http` itself before we see them — that's
    // belt + suspenders, not a regression risk for this layer).
    let router = build();
    let (_, id) = send(&router, "/healthz", Some("with space")).await;
    let id_str = id.unwrap().to_str().unwrap().to_string();
    assert_ne!(
        id_str, "with space",
        "spaces are not in the allowed charset; must regenerate",
    );
    assert!(
        is_16_hex(&id_str),
        "regenerated id must be 16 hex chars, got {id_str:?}",
    );
}

#[tokio::test]
async fn empty_inbound_id_regenerates() {
    let router = build();
    let (_, id) = send(&router, "/healthz", Some("")).await;
    let id_str = id.unwrap().to_str().unwrap().to_string();
    assert!(
        is_16_hex(&id_str),
        "empty inbound id must regenerate to a 16-hex value, got {id_str:?}",
    );
}

#[tokio::test]
async fn over_length_inbound_id_regenerates() {
    let router = build();
    let too_long = "a".repeat(200);
    let (_, id) = send(&router, "/healthz", Some(&too_long)).await;
    let id_str = id.unwrap().to_str().unwrap().to_string();
    assert!(
        is_16_hex(&id_str),
        "over-length inbound id must regenerate, got len={}",
        id_str.len(),
    );
}
