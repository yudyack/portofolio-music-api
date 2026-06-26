//! QA acceptance — criterion 26 through the REAL HTTP stack (the spec's
//! literal test). Five concurrent `get()` calls share one `SpotifyService`
//! wired to a real `ReqwestSpotifyClient` + `ReqwestTokenExchanger` +
//! `SqliteTokenRepository`. The data endpoint 401s the OLD bearer and 200s
//! the NEW one; the token endpoint is mounted `.expect(1)`. Single-flight
//! must dispatch exactly ONE refresh POST despite five concurrent 401s.
//!
//! The coder's `tests/single_flight_refresh.rs` pins the latch against
//! stubs; this proves it through the concrete reqwest path and SQLite.

use std::num::NonZeroU32;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use governor::Quota;
use music_api::app::spotify_service::SpotifyService;
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
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn fresh_pool() -> SqlitePool {
    // In-memory sqlite MUST stay at one connection (each extra connection is
    // a separate :memory: database).
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

fn seed() -> TokenRecord {
    TokenRecord {
        access_token: "OLD_ACCESS".to_string(),
        refresh_token: "OLD_REFRESH".to_string(),
        expires_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        scope: "user-read-private".to_string(),
        owner_id: "yudhyapw".to_string(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn five_concurrent_401s_dispatch_exactly_one_refresh_over_real_http() {
    let server = MockServer::start().await;
    // Data endpoint: 401 for the stale bearer, 200 for the refreshed one.
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .and(header("authorization", "Bearer OLD_ACCESS"))
        .respond_with(ResponseTemplate::new(401).set_body_string(""))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .and(header("authorization", "Bearer NEW_ACCESS"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;
    // Token endpoint: exactly ONE refresh POST is the criterion-26 assertion.
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
    repo.upsert(seed()).await.expect("seed");

    // Loose quota so the governor never blocks the concurrent calls.
    let spotify: Arc<dyn SpotifyClient> = Arc::new(
        ReqwestSpotifyClient::with_quota(
            server.uri(),
            Quota::with_period(Duration::from_millis(1))
                .unwrap()
                .allow_burst(NonZeroU32::new(1000).unwrap()),
        )
        .expect("spotify client builds"),
    );
    let oauth: Arc<dyn TokenExchanger> = Arc::new(
        ReqwestTokenExchanger::new(
            format!("{}/api/token", server.uri()),
            "client-abc".to_string(),
            "secret-xyz".to_string(),
        )
        .expect("token exchanger builds"),
    );
    let svc = Arc::new(SpotifyService::new(
        repo.clone(),
        spotify,
        oauth,
        Arc::new(AuthState::new()),
    ));

    // Five concurrent callers.
    let mut handles = Vec::new();
    for _ in 0..5 {
        let s = svc.clone();
        handles.push(tokio::spawn(async move { s.get("/v1/me").await }));
    }
    for h in handles {
        let v = h
            .await
            .expect("task did not panic")
            .expect("each concurrent call must succeed after the shared refresh");
        assert_eq!(v, Some(json!({"ok": true})));
    }

    // Criterion 26: exactly one refresh POST reached the token endpoint.
    let token_posts = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method == http::Method::POST && r.url.path() == "/api/token")
        .count();
    assert_eq!(
        token_posts, 1,
        "single-flight: 5 concurrent 401s must produce exactly ONE refresh POST, got {token_posts}",
    );

    // The rotated token-set landed in SQLite once.
    let stored = repo.get().await.unwrap().expect("token row present");
    assert_eq!(stored.access_token, "NEW_ACCESS");
    assert_eq!(stored.refresh_token, "NEW_REFRESH");
}
