//! OAuth bootstrap flow — `/auth/spotify/login` + `/callback`.
//! Criteria 1b, 2, 3, 4, 23, 25.

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
use sqlx::SqlitePool;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OWNER: &str = "yudhyapw";
const USER: &str = "owner";
const PASS: &str = "s3cret";

fn cfg() -> Config {
    Config {
        owner_spotify_user_id: OWNER.into(),
        auth_basic_username: USER.into(),
        auth_basic_password: PASS.into(),
        spotify_client_id: "test-client".into(),
        spotify_client_secret: "secret".into(),
        spotify_redirect_uri: "http://127.0.0.1:8080/auth/spotify/callback".into(),
        database_url: "sqlite::memory:".into(),
        mock_data: false,
    }
}

fn basic(user: &str, pass: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{user}:{pass}")))
}

async fn fresh_pool() -> SqlitePool {
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();
    run_migrations(&pool).await.unwrap();
    pool
}

/// Build an AppState pointed at `server`. Returns the repo + state_store so
/// tests can seed states and inspect persisted tokens.
async fn build(server: &MockServer) -> (AppState, Arc<dyn TokenRepository>, Arc<StateStore>) {
    let repo: Arc<dyn TokenRepository> = Arc::new(SqliteTokenRepository::new(fresh_pool().await));
    let store = Arc::new(StateStore::new());
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
        Arc::new(cfg()),
        repo.clone(),
        spotify,
        oauth,
        Arc::new(AuthState::new()),
        store.clone(),
    );
    (state, repo, store)
}

async fn send(state: AppState, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = music_api::app(state).oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, headers, String::from_utf8(body.to_vec()).unwrap())
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn get_auth(uri: &str, authz: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("authorization", authz)
        .body(Body::empty())
        .unwrap()
}

// ---- /login (criteria 1b, 23) -----------------------------------------

#[tokio::test]
async fn login_without_basic_auth_is_401_with_challenge() {
    let server = MockServer::start().await;
    let (state, _, _) = build(&server).await;
    let (status, headers, _) = send(state, get("/auth/spotify/login")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers.get("www-authenticate").unwrap(),
        "Basic realm=\"music-api\"",
    );
}

#[tokio::test]
async fn login_with_wrong_password_is_401() {
    let server = MockServer::start().await;
    let (state, _, _) = build(&server).await;
    let (status, _, _) =
        send(state, get_auth("/auth/spotify/login", &basic(USER, "wrong"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_with_correct_creds_redirects_to_spotify() {
    let server = MockServer::start().await;
    let (state, _, store) = build(&server).await;
    let (status, headers, _) =
        send(state, get_auth("/auth/spotify/login", &basic(USER, PASS))).await;

    assert_eq!(status, StatusCode::FOUND);
    let loc = headers.get("location").unwrap().to_str().unwrap();
    assert!(loc.contains("accounts.spotify.com/authorize"), "loc: {loc}");
    assert!(loc.contains("client_id=test-client"), "loc: {loc}");
    assert!(loc.contains("response_type=code"), "loc: {loc}");
    assert!(loc.contains("state="), "loc must carry a state: {loc}");

    let cookie = headers.get("set-cookie").unwrap().to_str().unwrap();
    assert!(cookie.starts_with("oauth_state="), "cookie: {cookie}");
    // Issuing a login must NOT consume the live state (consume happens at
    // the callback). The store should still hold exactly one live state.
    let issued = loc.split("state=").nth(1).unwrap();
    assert!(store.consume(issued), "the login-issued state must be live");
}

// ---- /callback (criteria 2, 3, 4, 25) ---------------------------------

#[tokio::test]
async fn callback_with_unknown_state_is_400_state_mismatch() {
    let server = MockServer::start().await;
    let (state, repo, _) = build(&server).await;
    let (status, _, body) =
        send(state, get("/auth/spotify/callback?code=abc&state=never-issued")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, "state mismatch");
    assert!(repo.get().await.unwrap().is_none(), "no token written");
}

#[tokio::test]
async fn callback_ignores_basic_auth_and_still_checks_state() {
    // criterion 25: even WITH valid Basic creds, an unknown state is 400.
    let server = MockServer::start().await;
    let (state, _, _) = build(&server).await;
    let (status, _, body) = send(
        state,
        get_auth(
            "/auth/spotify/callback?code=abc&state=never-issued",
            &basic(USER, PASS),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, "state mismatch");
}

#[tokio::test]
async fn callback_with_wrong_owner_is_403_and_writes_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "A", "token_type": "Bearer", "expires_in": 3600, "refresh_token": "R"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "intruder"})))
        .mount(&server)
        .await;

    let (state, repo, store) = build(&server).await;
    let s = store.issue();
    let (status, _, body) =
        send(state, get(&format!("/auth/spotify/callback?code=abc&state={s}"))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, "not the owner");
    assert!(repo.get().await.unwrap().is_none(), "wrong owner writes no token");
}

#[tokio::test]
async fn callback_valid_owner_upserts_tokens_and_returns_200() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ACCESS_1", "token_type": "Bearer", "expires_in": 3600,
            "refresh_token": "REFRESH_1", "scope": "user-read-private"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": OWNER})))
        .expect(1)
        .mount(&server)
        .await;

    let (state, repo, store) = build(&server).await;
    let s = store.issue();
    let (status, _, body) =
        send(state, get(&format!("/auth/spotify/callback?code=abc&state={s}"))).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "Spotify linked. You can close this tab.");
    let stored = repo.get().await.unwrap().expect("tokens persisted");
    assert_eq!(stored.access_token, "ACCESS_1");
    assert_eq!(stored.refresh_token, "REFRESH_1");
    assert_eq!(stored.owner_id, OWNER);
}
