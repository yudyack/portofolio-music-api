//! Criterion 10 (refresh half) — the `accounts.spotify.com` token
//! endpoint client. `ReqwestTokenExchanger::refresh` POSTs
//! `grant_type=refresh_token` with the app's client credentials and
//! parses the rotated token-set. A `400 {"error":"invalid_grant"}`
//! (revoked refresh token) maps to the typed `TokenExchangeError::InvalidGrant`
//! so the caller can transition to `NeedsReauth` without string-matching.
//!
//! This is a separate trait from `SpotifyClient`: different host
//! (accounts vs api), different auth (Basic client creds vs Bearer), and
//! the callback's `authorization_code` grant will reuse it later. Lives
//! under `src/infra/` (criterion 20 / reqwest isolation).

use std::sync::Arc;

use music_api::domain::oauth_client::{TokenExchangeError, TokenExchanger};
use music_api::infra::token_exchanger::ReqwestTokenExchanger;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn exchanger(server: &MockServer) -> Arc<ReqwestTokenExchanger> {
    Arc::new(
        ReqwestTokenExchanger::new(
            format!("{}/api/token", server.uri()),
            "client-abc".to_string(),
            "secret-xyz".to_string(),
        )
        .expect("ReqwestTokenExchanger::new should build for a valid endpoint"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_posts_grant_type_and_parses_rotated_token_set() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "NEW_ACCESS",
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": "NEW_REFRESH",
            "scope": "user-read-private playlist-read-private"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ex = exchanger(&server);
    let refreshed = ex
        .refresh("OLD_REFRESH")
        .await
        .expect("refresh must succeed against the 200 arm");

    assert_eq!(refreshed.access_token, "NEW_ACCESS");
    assert_eq!(refreshed.refresh_token.as_deref(), Some("NEW_REFRESH"));
    assert_eq!(refreshed.expires_in, 3600);
    assert_eq!(
        refreshed.scope.as_deref(),
        Some("user-read-private playlist-read-private")
    );

    // The request must carry the refresh grant + the old refresh token, and
    // the client credentials via HTTP Basic (Spotify's accepted form).
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    let body = String::from_utf8(reqs[0].body.clone()).unwrap();
    assert!(
        body.contains("grant_type=refresh_token"),
        "body must request the refresh grant; got {body:?}",
    );
    assert!(
        body.contains("refresh_token=OLD_REFRESH"),
        "body must carry the old refresh token; got {body:?}",
    );
    let auth = reqs[0]
        .headers
        .get("authorization")
        .expect("request must carry an Authorization header")
        .to_str()
        .unwrap();
    assert!(
        auth.starts_with("Basic "),
        "client credentials must be sent via HTTP Basic; got {auth:?}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_maps_invalid_grant_to_typed_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "Refresh token revoked"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ex = exchanger(&server);
    let err = ex
        .refresh("REVOKED")
        .await
        .expect_err("a 400 invalid_grant must surface as an error");

    assert!(
        matches!(err, TokenExchangeError::InvalidGrant),
        "expected TokenExchangeError::InvalidGrant, got {err:?}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_without_rotated_refresh_token_yields_none() {
    let server = MockServer::start().await;
    // Spotify often omits refresh_token when it does NOT rotate it.
    Mock::given(method("POST"))
        .and(path("/api/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "NEW_ACCESS_ONLY",
            "token_type": "Bearer",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ex = exchanger(&server);
    let refreshed = ex.refresh("KEEP_ME").await.expect("refresh must succeed");

    assert_eq!(refreshed.access_token, "NEW_ACCESS_ONLY");
    assert_eq!(
        refreshed.refresh_token, None,
        "absent refresh_token must map to None so the caller keeps the old one",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exchange_code_posts_authorization_code_grant_and_parses_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ACCESS_1",
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": "REFRESH_1",
            "scope": "user-read-private"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ex = exchanger(&server);
    let tokens = ex
        .exchange_code("AUTH_CODE", "http://127.0.0.1:8080/auth/spotify/callback")
        .await
        .expect("exchange must succeed");
    assert_eq!(tokens.access_token, "ACCESS_1");
    assert_eq!(tokens.refresh_token.as_deref(), Some("REFRESH_1"));

    let body =
        String::from_utf8(server.received_requests().await.unwrap()[0].body.clone()).unwrap();
    assert!(
        body.contains("grant_type=authorization_code"),
        "body: {body}"
    );
    assert!(body.contains("code=AUTH_CODE"), "body: {body}");
    assert!(
        body.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A8080%2Fauth%2Fspotify%2Fcallback"),
        "body must carry the redirect_uri: {body}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exchange_code_maps_invalid_grant() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})))
        .expect(1)
        .mount(&server)
        .await;

    let ex = exchanger(&server);
    let err = ex
        .exchange_code("BAD", "http://127.0.0.1:8080/auth/spotify/callback")
        .await
        .expect_err("400 invalid_grant must surface");
    assert!(
        matches!(err, TokenExchangeError::InvalidGrant),
        "got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_maps_non_400_failure_to_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/token"))
        .respond_with(ResponseTemplate::new(503).set_body_string(""))
        .expect(1)
        .mount(&server)
        .await;

    let ex = exchanger(&server);
    let err = ex.refresh("whatever").await.expect_err("503 must surface");
    assert!(
        matches!(err, TokenExchangeError::Status(503)),
        "expected TokenExchangeError::Status(503), got {err:?}",
    );
}
