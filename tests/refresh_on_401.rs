//! Criterion 10 — on Spotify 401, the service refreshes the token then
//! retries the original call once. If refresh fails with `invalid_grant`,
//! it transitions to `NeedsReauth` (no retry, stored tokens untouched —
//! criterion 6 spirit: the owner reauthing upserts over them).
//!
//! Driven entirely against in-memory stubs (no network): a scripted
//! `SpotifyClient` (401 then 200), a configurable `TokenExchanger`
//! (rotated tokens OR invalid_grant), and an in-memory `TokenRepository`.
//! The wiremock-backed real exchanger is covered by
//! `tests/token_exchanger.rs`; this file pins the ORCHESTRATION.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{Duration, Utc};
use music_api::app::spotify_service::{ServiceError, SpotifyService};
use music_api::domain::auth_state::AuthState;
use music_api::domain::oauth_client::{RefreshedTokens, TokenExchangeError, TokenExchanger};
use music_api::domain::spotify::{SpotifyClient, SpotifyError};
use music_api::domain::tokens::{RepoError, TokenRecord, TokenRepository};
use serde_json::{json, Value};

// ---- stubs -------------------------------------------------------------

/// Scripted SpotifyClient: each `get_json` pops the next programmed
/// outcome and records the access token it was called with.
struct ScriptedSpotify {
    script: Mutex<VecDeque<Result<Value, u16>>>,
    seen_tokens: Mutex<Vec<String>>,
}

impl ScriptedSpotify {
    fn new(script: Vec<Result<Value, u16>>) -> Self {
        Self {
            script: Mutex::new(script.into_iter().collect()),
            seen_tokens: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl SpotifyClient for ScriptedSpotify {
    async fn get_json(&self, _path: &str, access_token: &str) -> Result<Value, SpotifyError> {
        self.seen_tokens.lock().unwrap().push(access_token.to_string());
        match self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .expect("ScriptedSpotify called more times than programmed")
        {
            Ok(v) => Ok(v),
            Err(code) => Err(SpotifyError::Status(code)),
        }
    }
}

/// Configurable TokenExchanger: returns a fixed result and records how many
/// times it was called and the refresh token it received.
struct StubExchanger {
    result: Mutex<Option<Result<RefreshedTokens, TokenExchangeError>>>,
    calls: AtomicUsize,
    seen_refresh: Mutex<Vec<String>>,
}

impl StubExchanger {
    fn ok(refreshed: RefreshedTokens) -> Self {
        Self {
            result: Mutex::new(Some(Ok(refreshed))),
            calls: AtomicUsize::new(0),
            seen_refresh: Mutex::new(Vec::new()),
        }
    }
    fn invalid_grant() -> Self {
        Self {
            result: Mutex::new(Some(Err(TokenExchangeError::InvalidGrant))),
            calls: AtomicUsize::new(0),
            seen_refresh: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl TokenExchanger for StubExchanger {
    async fn refresh(&self, refresh_token: &str) -> Result<RefreshedTokens, TokenExchangeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen_refresh.lock().unwrap().push(refresh_token.to_string());
        // Clone-or-move the single programmed result. Tests call refresh at
        // most once, so taking it is fine; a second call panics loudly.
        match self.result.lock().unwrap().take().expect("StubExchanger refreshed twice") {
            Ok(r) => Ok(r),
            Err(e) => Err(e),
        }
    }
    async fn exchange_code(&self, _: &str, _: &str) -> Result<RefreshedTokens, TokenExchangeError> {
        unimplemented!("stub: exchange_code not exercised by these tests")
    }
}

/// In-memory single-row TokenRepository.
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

fn seed_token() -> TokenRecord {
    TokenRecord {
        access_token: "OLD_ACCESS".to_string(),
        refresh_token: "OLD_REFRESH".to_string(),
        expires_at: Utc::now() - Duration::seconds(10), // already expired
        scope: "user-read-private".to_string(),
        owner_id: "yudhyapw".to_string(),
    }
}

fn service(
    repo: Arc<MemRepo>,
    spotify: Arc<ScriptedSpotify>,
    oauth: Arc<StubExchanger>,
    auth_state: Arc<AuthState>,
) -> SpotifyService {
    SpotifyService::new(repo, spotify, oauth, auth_state)
}

// ---- tests -------------------------------------------------------------

#[tokio::test]
async fn on_401_refreshes_upserts_and_retries_once_succeeding() {
    let repo = Arc::new(MemRepo::with(seed_token()));
    let spotify = Arc::new(ScriptedSpotify::new(vec![
        Err(401),                         // first attempt → 401
        Ok(json!({"playing": false})),    // retry → 200
    ]));
    let oauth = Arc::new(StubExchanger::ok(RefreshedTokens {
        access_token: "NEW_ACCESS".to_string(),
        refresh_token: Some("NEW_REFRESH".to_string()),
        expires_in: 3600,
        scope: Some("user-read-private".to_string()),
    }));
    let auth_state = Arc::new(AuthState::new());
    let svc = service(repo.clone(), spotify.clone(), oauth.clone(), auth_state.clone());

    let v = svc.get("/v1/me/player").await.expect("must succeed after refresh+retry");
    assert_eq!(v, json!({"playing": false}));

    // Refresh ran exactly once with the OLD refresh token.
    assert_eq!(oauth.calls.load(Ordering::SeqCst), 1);
    assert_eq!(oauth.seen_refresh.lock().unwrap().as_slice(), &["OLD_REFRESH"]);

    // The retry used the NEW access token (not the stale one).
    let seen = spotify.seen_tokens.lock().unwrap().clone();
    assert_eq!(seen, vec!["OLD_ACCESS".to_string(), "NEW_ACCESS".to_string()]);

    // The rotated token-set was persisted.
    let stored = repo.snapshot().unwrap();
    assert_eq!(stored.access_token, "NEW_ACCESS");
    assert_eq!(stored.refresh_token, "NEW_REFRESH");
    assert!(stored.expires_at > Utc::now(), "expires_at must move into the future");

    assert!(!auth_state.needs_reauth(), "a successful refresh must NOT flip NeedsReauth");
}

#[tokio::test]
async fn on_401_with_invalid_grant_flips_needs_reauth_and_does_not_retry() {
    let repo = Arc::new(MemRepo::with(seed_token()));
    let spotify = Arc::new(ScriptedSpotify::new(vec![Err(401)])); // only the first attempt
    let oauth = Arc::new(StubExchanger::invalid_grant());
    let auth_state = Arc::new(AuthState::new());
    let svc = service(repo.clone(), spotify.clone(), oauth.clone(), auth_state.clone());

    let err = svc.get("/v1/me").await.expect_err("invalid_grant must surface as an error");
    assert!(matches!(err, ServiceError::NeedsReauth), "got {err:?}");

    assert!(auth_state.needs_reauth(), "invalid_grant must flip NeedsReauth");
    assert_eq!(oauth.calls.load(Ordering::SeqCst), 1, "refresh attempted once");
    // No retry: spotify saw exactly one attempt.
    assert_eq!(spotify.seen_tokens.lock().unwrap().len(), 1, "must NOT retry after invalid_grant");
    // Stored tokens untouched (criterion 6: owner reauth upserts over them).
    assert_eq!(repo.snapshot().unwrap().access_token, "OLD_ACCESS");
}

#[tokio::test]
async fn success_without_401_does_not_refresh() {
    let repo = Arc::new(MemRepo::with(seed_token()));
    let spotify = Arc::new(ScriptedSpotify::new(vec![Ok(json!({"ok": true}))]));
    let oauth = Arc::new(StubExchanger::ok(RefreshedTokens {
        access_token: "UNUSED".to_string(),
        refresh_token: None,
        expires_in: 3600,
        scope: None,
    }));
    let auth_state = Arc::new(AuthState::new());
    let svc = service(repo, spotify.clone(), oauth.clone(), auth_state);

    let v = svc.get("/v1/me").await.expect("must succeed on first try");
    assert_eq!(v, json!({"ok": true}));
    assert_eq!(oauth.calls.load(Ordering::SeqCst), 0, "no 401 → no refresh");
    assert_eq!(spotify.seen_tokens.lock().unwrap().len(), 1, "no retry");
}

#[tokio::test]
async fn retry_still_401_surfaces_upstream_without_second_refresh() {
    let repo = Arc::new(MemRepo::with(seed_token()));
    // Both the original and the retry return 401.
    let spotify = Arc::new(ScriptedSpotify::new(vec![Err(401), Err(401)]));
    let oauth = Arc::new(StubExchanger::ok(RefreshedTokens {
        access_token: "NEW_ACCESS".to_string(),
        refresh_token: Some("NEW_REFRESH".to_string()),
        expires_in: 3600,
        scope: None,
    }));
    let auth_state = Arc::new(AuthState::new());
    let svc = service(repo, spotify.clone(), oauth.clone(), auth_state.clone());

    let err = svc.get("/v1/me").await.expect_err("a persistent 401 must surface");
    assert!(matches!(err, ServiceError::Upstream(_)), "got {err:?}");
    // Retry happened exactly once (2 attempts total), refresh exactly once.
    assert_eq!(spotify.seen_tokens.lock().unwrap().len(), 2, "exactly one retry");
    assert_eq!(oauth.calls.load(Ordering::SeqCst), 1, "refresh not repeated");
    // A second 401 is an upstream failure, not a revoked grant: do NOT flip
    // NeedsReauth (only invalid_grant does that).
    assert!(!auth_state.needs_reauth());
}

#[tokio::test]
async fn rotated_refresh_token_absent_keeps_the_old_one() {
    let repo = Arc::new(MemRepo::with(seed_token()));
    let spotify = Arc::new(ScriptedSpotify::new(vec![Err(401), Ok(json!({"ok": true}))]));
    let oauth = Arc::new(StubExchanger::ok(RefreshedTokens {
        access_token: "NEW_ACCESS".to_string(),
        refresh_token: None, // Spotify did not rotate it
        expires_in: 3600,
        scope: None,
    }));
    let auth_state = Arc::new(AuthState::new());
    let svc = service(repo.clone(), spotify, oauth, auth_state);

    svc.get("/v1/me").await.expect("must succeed");
    let stored = repo.snapshot().unwrap();
    assert_eq!(stored.access_token, "NEW_ACCESS", "access token rotated");
    assert_eq!(
        stored.refresh_token, "OLD_REFRESH",
        "absent rotated refresh token → keep the existing one",
    );
    assert_eq!(stored.scope, "user-read-private", "absent scope → keep the existing one");
}
