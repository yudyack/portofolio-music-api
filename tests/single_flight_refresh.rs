//! Criterion 26 — single-flight token refresh. When ≥2 outbound calls
//! observe HTTP 401 concurrently, only ONE refresh POST is dispatched; the
//! others await that result and retry with the shared new access token.
//!
//! Driven against concurrency-safe stubs (no network) so the assertion is
//! deterministic: a counting `TokenExchanger` records how many refreshes
//! actually fired. The exchanger sleeps briefly to widen the contention
//! window — without single-flight all five callers enter `refresh`
//! before the first upsert lands, so the count would be 5.
//!
//! Behavioral RED: this test compiles against the existing `SpotifyService`
//! API and FAILS (count == 5) until the single-flight latch is added.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use music_api::app::spotify_service::{ServiceError, SpotifyService};
use music_api::domain::auth_state::AuthState;
use music_api::domain::oauth_client::{RefreshedTokens, TokenExchangeError, TokenExchanger};
use music_api::domain::spotify::{SpotifyClient, SpotifyError};
use music_api::domain::tokens::{RepoError, TokenRecord, TokenRepository};
use serde_json::{json, Value};

/// 401 for the OLD access token, 200 for the NEW one. Stateless → safe to
/// call from many tasks at once.
struct TokenAwareSpotify;

#[async_trait]
impl SpotifyClient for TokenAwareSpotify {
    async fn get_json(&self, _path: &str, access_token: &str) -> Result<Value, SpotifyError> {
        if access_token == "NEW_ACCESS" {
            Ok(json!({"ok": true}))
        } else {
            Err(SpotifyError::Status(401))
        }
    }
}

enum Outcome {
    Rotate,
    InvalidGrant,
}

/// Counts how many times `refresh` actually fired, and sleeps to force the
/// concurrent callers to overlap inside the refresh window.
struct CountingExchanger {
    calls: AtomicUsize,
    outcome: Outcome,
    delay: Duration,
}

impl CountingExchanger {
    fn new(outcome: Outcome) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            outcome,
            delay: Duration::from_millis(100),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl TokenExchanger for CountingExchanger {
    async fn refresh(&self, _refresh_token: &str) -> Result<RefreshedTokens, TokenExchangeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        match self.outcome {
            Outcome::Rotate => Ok(RefreshedTokens {
                access_token: "NEW_ACCESS".to_string(),
                refresh_token: Some("NEW_REFRESH".to_string()),
                expires_in: 3600,
                scope: None,
            }),
            Outcome::InvalidGrant => Err(TokenExchangeError::InvalidGrant),
        }
    }
}

/// Concurrency-safe in-memory single-row repository.
struct MemRepo {
    rec: Mutex<Option<TokenRecord>>,
}

impl MemRepo {
    fn with(rec: TokenRecord) -> Self {
        Self {
            rec: Mutex::new(Some(rec)),
        }
    }
    fn snapshot(&self) -> Option<TokenRecord> {
        self.rec.lock().unwrap().clone()
    }
}

#[async_trait]
impl TokenRepository for MemRepo {
    async fn get(&self) -> Result<Option<TokenRecord>, RepoError> {
        Ok(self.rec.lock().unwrap().clone())
    }
    async fn upsert(&self, tokens: TokenRecord) -> Result<(), RepoError> {
        *self.rec.lock().unwrap() = Some(tokens);
        Ok(())
    }
    async fn delete(&self) -> Result<(), RepoError> {
        *self.rec.lock().unwrap() = None;
        Ok(())
    }
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

async fn drive_five(
    svc: Arc<SpotifyService>,
) -> Vec<Result<Value, ServiceError>> {
    let mut handles = Vec::new();
    for _ in 0..5 {
        let s = svc.clone();
        handles.push(tokio::spawn(async move { s.get("/v1/me").await }));
    }
    let mut out = Vec::new();
    for h in handles {
        out.push(h.await.expect("task did not panic"));
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn five_concurrent_401s_trigger_exactly_one_refresh() {
    let repo = Arc::new(MemRepo::with(seed()));
    let exchanger = Arc::new(CountingExchanger::new(Outcome::Rotate));
    let auth_state = Arc::new(AuthState::new());
    let svc = Arc::new(SpotifyService::new(
        repo.clone(),
        Arc::new(TokenAwareSpotify),
        exchanger.clone(),
        auth_state.clone(),
    ));

    let results = drive_five(svc).await;

    // Criterion 26: exactly ONE refresh POST despite five concurrent 401s.
    assert_eq!(
        exchanger.calls(),
        1,
        "single-flight: five concurrent 401s must collapse into ONE refresh, got {}",
        exchanger.calls(),
    );
    // All five callers succeed on retry with the shared new token.
    for r in &results {
        assert_eq!(r.as_ref().expect("each call succeeds"), &json!({"ok": true}));
    }
    // The rotated token-set is persisted exactly once.
    let stored = repo.snapshot().unwrap();
    assert_eq!(stored.access_token, "NEW_ACCESS");
    assert_eq!(stored.refresh_token, "NEW_REFRESH");
    assert!(!auth_state.needs_reauth());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn five_concurrent_401s_with_invalid_grant_refresh_once_and_all_need_reauth() {
    let repo = Arc::new(MemRepo::with(seed()));
    let exchanger = Arc::new(CountingExchanger::new(Outcome::InvalidGrant));
    let auth_state = Arc::new(AuthState::new());
    let svc = Arc::new(SpotifyService::new(
        repo.clone(),
        Arc::new(TokenAwareSpotify),
        exchanger.clone(),
        auth_state.clone(),
    ));

    let results = drive_five(svc).await;

    // A dead refresh token must be POSTed ONCE, not five times.
    assert_eq!(
        exchanger.calls(),
        1,
        "single-flight on the failure path: one refresh attempt, got {}",
        exchanger.calls(),
    );
    for r in &results {
        assert!(
            matches!(r, Err(ServiceError::NeedsReauth)),
            "every caller surfaces NeedsReauth, got {r:?}",
        );
    }
    assert!(auth_state.needs_reauth());
    // Stored tokens untouched.
    assert_eq!(repo.snapshot().unwrap().access_token, "OLD_ACCESS");
}
