//! QA acceptance — criterion 10 end-to-end with REAL components (no stubs):
//! `SpotifyService` → `ReqwestSpotifyClient` (full middleware chain) →
//! wiremock `/v1/me`, and → `ReqwestTokenExchanger` → wiremock `/api/token`,
//! persisting through a real `SqliteTokenRepository` (in-memory).
//!
//! The coder's `tests/refresh_on_401.rs` pins the orchestration against
//! trait stubs; `tests/token_exchanger.rs` pins the real refresh HTTP.
//! This file proves the two wire together through the actual concrete
//! types and SQLite — the seam an integration test exists to catch.
//!
//! Also closes a QA promise: the Client Secret never reaches the
//! data-plane (`api.spotify.com`) — only the token endpoint, via Basic.

use std::num::NonZeroU32;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use governor::Quota;
use music_api::app::spotify_service::{ServiceError, SpotifyService};
use music_api::domain::auth_state::AuthState;
use music_api::domain::oauth_client::TokenExchanger;
use music_api::domain::spotify::SpotifyClient;
use music_api::domain::tokens::{TokenRecord, TokenRepository};
use music_api::infra::run_migrations;
use music_api::infra::spotify_client::ReqwestSpotifyClient;
use music_api::infra::sqlite_token_repo::SqliteTokenRepository;
use music_api::infra::token_exchanger::ReqwestTokenExchanger;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLIENT_SECRET: &str = "super-secret-value-must-not-leak";

async fn fresh_pool() -> SqlitePool {
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")
        .expect("static URI")
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .expect("connect in-memory sqlite");
    run_migrations(&pool).await.expect("migrations apply");
    pool
}

fn seed_record() -> TokenRecord {
    TokenRecord {
        access_token: "OLD_ACCESS".to_string(),
        refresh_token: "OLD_REFRESH".to_string(),
        expires_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(), // long expired
        scope: "user-read-private".to_string(),
        owner_id: "yudhyapw".to_string(),
    }
}

fn loose_quota() -> Quota {
    Quota::with_period(Duration::from_millis(1))
        .unwrap()
        .allow_burst(NonZeroU32::new(1000).unwrap())
}

fn build_service(
    server: &MockServer,
    repo: Arc<dyn TokenRepository>,
    auth_state: Arc<AuthState>,
) -> SpotifyService {
    let spotify: Arc<dyn SpotifyClient> = Arc::new(
        ReqwestSpotifyClient::with_quota(server.uri(), loose_quota())
            .expect("spotify client builds"),
    );
    let oauth: Arc<dyn TokenExchanger> = Arc::new(
        ReqwestTokenExchanger::new(
            format!("{}/api/token", server.uri()),
            "client-abc".to_string(),
            CLIENT_SECRET.to_string(),
        )
        .expect("token exchanger builds"),
    );
    SpotifyService::new(repo, spotify, oauth, auth_state)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn end_to_end_401_refreshes_persists_rotated_set_and_retries() {
    let server = MockServer::start().await;
    // /v1/me: first attempt 401, retry 200.
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(401).set_body_string(""))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"playing": false})))
        .expect(1)
        .mount(&server)
        .await;
    // /api/token: rotated token-set.
    Mock::given(method("POST"))
        .and(path("/api/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "NEW_ACCESS",
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": "NEW_REFRESH",
            "scope": "user-read-private"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let pool = fresh_pool().await;
    let repo: Arc<dyn TokenRepository> = Arc::new(SqliteTokenRepository::new(pool));
    repo.upsert(seed_record()).await.expect("seed");
    let auth_state = Arc::new(AuthState::new());
    let svc = build_service(&server, repo.clone(), auth_state.clone());

    let v = svc.get("/v1/me").await.expect("must succeed after refresh+retry");
    assert_eq!(v, Some(json!({"playing": false})));

    // Rotated token-set persisted to SQLite.
    let stored = repo.get().await.unwrap().expect("token row present");
    assert_eq!(stored.access_token, "NEW_ACCESS");
    assert_eq!(stored.refresh_token, "NEW_REFRESH");
    assert!(stored.expires_at > Utc::now(), "expires_at moved into the future");
    assert!(!auth_state.needs_reauth());

    // Exactly: GET 401, POST refresh, GET 200.
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 3, "GET(401) + POST(token) + GET(200)");

    // The Client Secret must NEVER reach the data-plane (/v1/me): those
    // requests carry Bearer, not client credentials.
    for r in reqs.iter().filter(|r| r.url.path() == "/v1/me") {
        let auth = r.headers.get("authorization").map(|v| v.to_str().unwrap().to_string());
        assert!(
            auth.as_deref().map(|a| a.starts_with("Bearer ")).unwrap_or(false),
            "data-plane request must use Bearer; got {auth:?}",
        );
        let body = String::from_utf8_lossy(&r.body);
        assert!(
            !auth.unwrap_or_default().contains(CLIENT_SECRET) && !body.contains(CLIENT_SECRET),
            "Client Secret must not appear in a data-plane request",
        );
    }
    // The two /v1/me calls used OLD then NEW access tokens.
    let bearers: Vec<String> = reqs
        .iter()
        .filter(|r| r.url.path() == "/v1/me")
        .map(|r| r.headers.get("authorization").unwrap().to_str().unwrap().to_string())
        .collect();
    assert_eq!(bearers, vec!["Bearer OLD_ACCESS", "Bearer NEW_ACCESS"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn end_to_end_invalid_grant_flips_needs_reauth_and_preserves_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(401).set_body_string(""))
        .expect(1) // exactly one attempt — no retry after invalid_grant
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "Refresh token revoked"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let pool = fresh_pool().await;
    let repo: Arc<dyn TokenRepository> = Arc::new(SqliteTokenRepository::new(pool));
    repo.upsert(seed_record()).await.expect("seed");
    let auth_state = Arc::new(AuthState::new());
    let svc = build_service(&server, repo.clone(), auth_state.clone());

    let err = svc.get("/v1/me").await.expect_err("invalid_grant must surface");
    assert!(matches!(err, ServiceError::NeedsReauth), "got {err:?}");
    assert!(auth_state.needs_reauth(), "invalid_grant flips NeedsReauth");

    // Stored tokens untouched (criterion 6: owner reauth upserts over them).
    let stored = repo.get().await.unwrap().expect("token row still present");
    assert_eq!(stored.access_token, "OLD_ACCESS");
    assert_eq!(stored.refresh_token, "OLD_REFRESH");

    // No retry: exactly one GET + one POST.
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2, "GET(401) + POST(invalid_grant), no retry");
}
