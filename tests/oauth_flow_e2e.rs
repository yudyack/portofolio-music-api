//! QA acceptance — full OAuth round-trip through HTTP (login issues the
//! state, callback consumes the same one) + the token-endpoint-failure
//! mapping. Complements the coder's store-injected `tests/oauth_flow.rs`.

use std::num::NonZeroU32;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use governor::Quota;
use music_api::app::state_store::StateStore;
use music_api::config::Config;
use music_api::domain::auth_state::AuthState;
use music_api::domain::oauth_client::TokenExchanger;
use music_api::domain::spotify::SpotifyClient;
use music_api::domain::tokens::TokenRepository;
use music_api::infra::run_migrations;
use music_api::infra::spotify_client::ReqwestSpotifyClient;
use music_api::infra::sqlite_token_repo::SqliteTokenRepository;
use music_api::infra::token_exchanger::ReqwestTokenExchanger;
use music_api::AppState;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OWNER: &str = "yudhyapw";

async fn build(server: &MockServer) -> (AppState, Arc<dyn TokenRepository>) {
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();
    run_migrations(&pool).await.unwrap();
    let repo: Arc<dyn TokenRepository> = Arc::new(SqliteTokenRepository::new(pool));
    let config = Config {
        owner_spotify_user_id: OWNER.into(),
        auth_basic_username: "owner".into(),
        auth_basic_password: "s3cret".into(),
        spotify_client_id: "test-client".into(),
        spotify_client_secret: "secret".into(),
        spotify_redirect_uri: "http://127.0.0.1:8080/auth/spotify/callback".into(),
        database_url: "sqlite::memory:".into(),
        mock_data: false,
        scheduler: Default::default(),
    };
    let loose = Quota::with_period(Duration::from_millis(1))
        .unwrap()
        .allow_burst(NonZeroU32::new(1000).unwrap());
    let spotify: Arc<dyn SpotifyClient> =
        Arc::new(ReqwestSpotifyClient::with_quota(server.uri(), loose).unwrap());
    let oauth: Arc<dyn TokenExchanger> = Arc::new(
        ReqwestTokenExchanger::new(
            format!("{}/api/token", server.uri()),
            "test-client".into(),
            "secret".into(),
        )
        .unwrap(),
    );
    let state = AppState::new_for_test(
        Arc::new(config),
        repo.clone(),
        spotify,
        oauth,
        Arc::new(AuthState::new()),
        Arc::new(StateStore::new()),
    );
    (state, repo)
}

#[tokio::test]
async fn login_then_callback_round_trip_links_the_account() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ACCESS_RT", "token_type": "Bearer", "expires_in": 3600,
            "refresh_token": "REFRESH_RT", "scope": "user-read-private"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": OWNER})))
        .mount(&server)
        .await;

    // State is shared because AppState clones share the same Arc<StateStore>.
    let (state, repo) = build(&server).await;

    // 1. /login with correct creds → 302, capture the issued state.
    let login = music_api::app(state.clone())
        .oneshot(
            Request::builder()
                .uri("/auth/spotify/login")
                .header(
                    "authorization",
                    format!("Basic {}", STANDARD.encode("owner:s3cret")),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::FOUND);
    let loc = login.headers().get("location").unwrap().to_str().unwrap();
    let issued_state = loc.split("state=").nth(1).unwrap().to_string();

    // 2. /callback with that exact state → 200, tokens stored.
    let cb = music_api::app(state)
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/auth/spotify/callback?code=AUTH&state={issued_state}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cb.status(), StatusCode::OK);
    let body = to_bytes(cb.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], b"Spotify linked. You can close this tab.");

    let stored = repo.get().await.unwrap().expect("tokens persisted");
    assert_eq!(stored.access_token, "ACCESS_RT");
    assert_eq!(stored.owner_id, OWNER);
}

#[tokio::test]
async fn callback_token_endpoint_failure_maps_to_502() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/token"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let (state, repo) = build(&server).await;
    let s = state.clone(); // keep a handle to issue a state via the shared store
                           // (issue through a login would also work)
                           // Issue a valid state by driving a login first.
    let login = music_api::app(s)
        .oneshot(
            Request::builder()
                .uri("/auth/spotify/login")
                .header(
                    "authorization",
                    format!("Basic {}", STANDARD.encode("owner:s3cret")),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let loc = login.headers().get("location").unwrap().to_str().unwrap();
    let issued = loc.split("state=").nth(1).unwrap().to_string();

    let cb = music_api::app(state)
        .oneshot(
            Request::builder()
                .uri(format!("/auth/spotify/callback?code=AUTH&state={issued}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cb.status(), StatusCode::BAD_GATEWAY);
    assert!(
        repo.get().await.unwrap().is_none(),
        "failed exchange writes nothing"
    );
}
