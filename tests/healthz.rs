//! Cycle 7 RED+GREEN: prove the AppState seam exists by driving the handler
//! against two repo fixtures and asserting the body's `token_state` flips
//! between them. The cycle-6 hardcode (`"uninitialized"` baked into a
//! constant string) cannot satisfy the `"authorized"` assertion below —
//! the test can ONLY pass once healthz reads from `State(state)`.
//!
//! Two fixtures kept inline per coder.md; the architect-bounced
//! `tests/common/mod.rs` extraction is deferred to the first cycle that
//! adds a second handler-test that needs the same fixtures.

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use music_api::app::state_store::StateStore;
use music_api::config::Config;
use music_api::domain::auth_state::AuthState;
use music_api::domain::oauth_client::{RefreshedTokens, TokenExchangeError, TokenExchanger};
use music_api::domain::spotify::{SpotifyClient, SpotifyError};
use music_api::domain::tokens::{RepoError, TokenRecord, TokenRepository};
use music_api::AppState;
use std::sync::Arc;
use tower::ServiceExt;

/// healthz reads only `tokens`; the other AppState fields need stub values.
struct NoopExchanger;
#[async_trait]
impl TokenExchanger for NoopExchanger {
    async fn refresh(&self, _: &str) -> Result<RefreshedTokens, TokenExchangeError> {
        Err(TokenExchangeError::Transport("unused".into()))
    }
    async fn exchange_code(&self, _: &str, _: &str) -> Result<RefreshedTokens, TokenExchangeError> {
        Err(TokenExchangeError::Transport("unused".into()))
    }
}

fn test_config() -> Config {
    Config {
        owner_spotify_user_id: "yudhyapw".into(),
        auth_basic_username: "owner".into(),
        auth_basic_password: "pw".into(),
        spotify_client_id: "cid".into(),
        spotify_client_secret: "secret".into(),
        spotify_redirect_uri: "http://127.0.0.1:8080/auth/spotify/callback".into(),
        database_url: "sqlite::memory:".into(),
    }
}

/// Cycle 8: AppState gained `spotify: Arc<dyn SpotifyClient>`. healthz
/// does not call it, so a no-op stub satisfies the constructor. Cycle 10
/// will add tests that actually exercise the field.
struct NoopSpotifyClient;
#[async_trait]
impl SpotifyClient for NoopSpotifyClient {
    async fn get_json(
        &self,
        _path: &str,
        _access_token: &str,
    ) -> Result<Option<serde_json::Value>, SpotifyError> {
        Err(SpotifyError::Transport("unused in healthz tests".into()))
    }
}

fn fixed_expires() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 1, 12, 0, 0).unwrap()
}

struct EmptyRepo;
#[async_trait]
impl TokenRepository for EmptyRepo {
    async fn get(&self) -> Result<Option<TokenRecord>, RepoError> {
        Ok(None)
    }
    async fn upsert(&self, _: TokenRecord) -> Result<(), RepoError> {
        Ok(())
    }
    async fn delete(&self) -> Result<(), RepoError> {
        Ok(())
    }
}

struct PrimedRepo;
#[async_trait]
impl TokenRepository for PrimedRepo {
    async fn get(&self) -> Result<Option<TokenRecord>, RepoError> {
        Ok(Some(TokenRecord {
            access_token: "test-access".into(),
            refresh_token: "test-refresh".into(),
            expires_at: fixed_expires(),
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

async fn drive(repo: Arc<dyn TokenRepository>) -> serde_json::Value {
    let state = AppState::new_for_test(
        Arc::new(test_config()),
        repo,
        Arc::new(NoopSpotifyClient),
        Arc::new(NoopExchanger),
        Arc::new(AuthState::new()),
        Arc::new(StateStore::new()),
    );
    let app = music_api::app(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "criterion 15: 200 always");
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn assert_shared_shape(json: &serde_json::Value) {
    let status = json["status"].as_str().expect("status field present");
    assert!(
        matches!(status, "ok" | "degraded" | "needs_reauth"),
        "status must be one of ok|degraded|needs_reauth, got {status:?}",
    );
    assert!(json["version"].is_string(), "version must be a string");
    assert!(json["token_state"].is_string(), "token_state must be a string");
    assert!(
        json["last_fetch_ts"].is_null() || json["last_fetch_ts"].is_string(),
        "last_fetch_ts must be null or ISO8601 string",
    );
}

#[tokio::test]
async fn healthz_with_empty_repo_reports_token_state_uninitialized() {
    let json = drive(Arc::new(EmptyRepo)).await;
    assert_shared_shape(&json);
    assert_eq!(
        json["token_state"], "uninitialized",
        "empty repo must surface token_state=uninitialized",
    );
}

#[tokio::test]
async fn healthz_with_primed_repo_reports_token_state_authorized() {
    // Pinning the seam: a primed (Some) repo MUST produce token_state=authorized.
    // The cycle-6 hardcode could only ever produce "uninitialized" — this
    // assertion proves the handler reads from State(state).
    let json = drive(Arc::new(PrimedRepo)).await;
    assert_shared_shape(&json);
    assert_eq!(
        json["token_state"], "authorized",
        "primed repo must surface token_state=authorized (cycle-7 seam)",
    );
}
