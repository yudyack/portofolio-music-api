//! Integration tests for the per-endpoint scheduler loop body
//! (`app::scheduler::spawn_one`). Validates the three gating behaviors
//! the spec §5.6 + criterion 11 contract relies on:
//!
//! 1. **Active path** — when `ActivityTracker::is_active()` is true, the
//!    loop ticks, fetches Spotify, maps, and stores into `Snapshots`.
//! 2. **Idle parking** — when the tracker is idle, the loop parks on
//!    `woke.notified()` and does NOT call Spotify; once a `touch()`
//!    crosses the idle threshold, the next tick fires and the snapshot
//!    populates.
//! 3. **Reauth branch sleeps, does NOT park** — with `AuthState`
//!    flipped to `needs_reauth` and the tracker active, the loop skips
//!    the fetch but loops on the interval (sleep + continue), so
//!    clearing `needs_reauth` allows the very next tick to fetch
//!    without needing another wake-up.
//!
//! These tests live outside `tests/scheduler_activity.rs` (which pins
//! `ActivityTracker` in isolation) because they exercise the loop body
//! itself — fetch + map + store — end-to-end against a programmed
//! Spotify mock and a real `AppState`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use music_api::app::scheduler::spawn_one;
use music_api::app::snapshots::EndpointKind;
use music_api::app::state_store::StateStore;
use music_api::config::{Config, SchedulerConfig};
use music_api::domain::auth_state::AuthState;
use music_api::domain::oauth_client::{RefreshedTokens, TokenExchangeError, TokenExchanger};
use music_api::domain::spotify::{SpotifyClient, SpotifyError};
use music_api::domain::tokens::{RepoError, TokenRecord, TokenRepository};
use music_api::AppState;
use serde_json::{json, Value};

// ---- test doubles -------------------------------------------------------

/// Programmed SpotifyClient. Routes `/v1/me/player` (etc.) to canned
/// responses and counts every call so tests can assert "loop fetched N
/// times" or "loop never called Spotify".
struct CountingSpotify {
    by_path: Mutex<HashMap<String, Value>>,
    calls: AtomicUsize,
}

impl CountingSpotify {
    fn new(routes: Vec<(&str, Value)>) -> Self {
        let mut map = HashMap::new();
        for (k, v) in routes {
            map.insert(k.to_string(), v);
        }
        Self {
            by_path: Mutex::new(map),
            calls: AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SpotifyClient for CountingSpotify {
    async fn get_json(&self, path: &str, _t: &str) -> Result<Option<Value>, SpotifyError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.by_path.lock().unwrap().get(path).cloned() {
            Some(v) => Ok(Some(v)),
            None => Err(SpotifyError::Status(404)),
        }
    }
}

struct MemRepo;
#[async_trait]
impl TokenRepository for MemRepo {
    async fn get(&self) -> Result<Option<TokenRecord>, RepoError> {
        Ok(Some(TokenRecord {
            access_token: "ACCESS".into(),
            refresh_token: "REFRESH".into(),
            expires_at: Utc::now() + ChronoDuration::seconds(3600),
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
        unimplemented!("scheduler_loop tests never trigger a refresh")
    }
    async fn exchange_code(&self, _: &str, _: &str) -> Result<RefreshedTokens, TokenExchangeError> {
        unimplemented!("scheduler_loop tests never trigger an exchange")
    }
}

// `is_active` keys off SystemTime, not the tokio clock, so the idle
// threshold needs to be large enough to stay "active" across the full
// test wait — otherwise the tracker silently flips back to idle mid-test
// and the loop parks for reasons unrelated to what's being asserted.
fn cfg_with_idle(idle_threshold: Duration) -> Config {
    Config {
        spotify_client_id: "cid".into(),
        spotify_client_secret: "secret".into(),
        spotify_redirect_uri: "https://x/callback".into(),
        owner_spotify_user_id: "yudhyapw".into(),
        auth_basic_username: "owner".into(),
        auth_basic_password: "pw".into(),
        database_url: "sqlite::memory:".into(),
        mock_data: false,
        scheduler: SchedulerConfig {
            idle_threshold,
            ..Default::default()
        },
    }
}

fn build_state(
    spotify: Arc<CountingSpotify>,
    idle_threshold: Duration,
) -> (AppState, Arc<AuthState>) {
    let auth_state = Arc::new(AuthState::new());
    let spotify_dyn: Arc<dyn SpotifyClient> = spotify;
    let state = AppState::new_for_test(
        Arc::new(cfg_with_idle(idle_threshold)),
        Arc::new(MemRepo),
        spotify_dyn,
        Arc::new(UnusedExchanger),
        auth_state.clone(),
        Arc::new(StateStore::new()),
    );
    (state, auth_state)
}

// ---- (1) active tracker: loop fetches and stores -----------------------

#[tokio::test]
async fn active_tracker_loop_fetches_and_stores() {
    let now_payload = json!({"is_playing": true, "item": {
        "name": "T", "duration_ms": 1, "artists": [{"name": "A"}],
        "album": {"name": "AL", "images": [{"url": "https://i/c.jpg"}]}
    }, "progress_ms": 0});
    let spotify = Arc::new(CountingSpotify::new(vec![("/v1/me/player", now_payload)]));
    // Idle threshold > test wait so the tracker stays active for the
    // entire test — keeps the assertion focused on "loop ticked" rather
    // than racing the wall clock.
    let (state, _auth) = build_state(spotify.clone(), Duration::from_secs(5));

    // Seed the tracker so is_active() returns true. The cold-boot
    // touch also flushes the freshly-created Notify, but the loop's
    // register-before-check pattern means the subsequent is_active()
    // observation skips the await entirely.
    state.activity.touch();
    assert!(state.activity.is_active(), "precondition: tracker active");

    spawn_one(state.clone(), EndpointKind::Now, Duration::from_millis(5));

    // Give the loop time to do at least one full tick — fetch + map + store.
    tokio::time::sleep(Duration::from_millis(80)).await;

    assert!(
        spotify.call_count() >= 1,
        "active loop must call Spotify at least once; got {}",
        spotify.call_count(),
    );
    assert!(
        state.snapshots.get(EndpointKind::Now).is_some(),
        "active loop must store a snapshot",
    );
}

// ---- (2) idle tracker parks; touch wakes it ----------------------------

#[tokio::test]
async fn idle_tracker_loop_parks_until_touch_wakes_it() {
    let now_payload = json!({"is_playing": false});
    let spotify = Arc::new(CountingSpotify::new(vec![("/v1/me/player", now_payload)]));
    // Large idle threshold so that AFTER the wake-up touch the tracker
    // stays active for the rest of the test (otherwise the loop could
    // park again between fetches and the second assertion races).
    let (state, _auth) = build_state(spotify.clone(), Duration::from_secs(5));

    // Precondition: tracker is idle (fresh tracker, never touched).
    assert!(!state.activity.is_active(), "precondition: tracker idle");

    spawn_one(state.clone(), EndpointKind::Now, Duration::from_millis(5));

    // Loop should park on `woke.notified()` and NOT call Spotify.
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        spotify.call_count(),
        0,
        "idle loop must not call Spotify; got {}",
        spotify.call_count(),
    );
    assert!(
        state.snapshots.get(EndpointKind::Now).is_none(),
        "idle loop must not store a snapshot",
    );

    // Now a visitor lands — middleware calls touch(). The gap-since-0 is
    // huge, so notify_waiters() fires and the parked loop unparks.
    state.activity.touch();
    tokio::time::sleep(Duration::from_millis(80)).await;

    assert!(
        spotify.call_count() >= 1,
        "after touch wakes loop, Spotify must be called; got {}",
        spotify.call_count(),
    );
    assert!(
        state.snapshots.get(EndpointKind::Now).is_some(),
        "after touch wakes loop, snapshot must populate",
    );
}

// ---- (3) needs_reauth: loop skips fetch but does NOT park --------------

#[tokio::test]
async fn needs_reauth_branch_sleeps_then_resumes_without_external_wake() {
    let now_payload = json!({"is_playing": false});
    let spotify = Arc::new(CountingSpotify::new(vec![("/v1/me/player", now_payload)]));
    // Idle threshold large enough that the single seed touch keeps the
    // tracker active across BOTH waits — otherwise the loop would park
    // for activity reasons, masking the reauth-branch behavior we want
    // to assert.
    let (state, auth) = build_state(spotify.clone(), Duration::from_secs(5));

    // Tracker active so we ISOLATE the reauth gate — the activity gate is
    // already open. Any "no fetch" we observe is therefore the reauth
    // branch's doing.
    state.activity.touch();
    auth.set_needs_reauth();

    spawn_one(state.clone(), EndpointKind::Now, Duration::from_millis(5));

    // Loop iterates: notified-await (skipped since active) → reauth check
    // → sleep(interval) → continue. NO fetch happens.
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        spotify.call_count(),
        0,
        "reauth branch must skip the Spotify fetch; got {}",
        spotify.call_count(),
    );

    // Critical: the reauth branch must NOT park on woke. If it did, we'd
    // need a touch() to wake it. Clear needs_reauth WITHOUT touching the
    // tracker, wait one interval, and assert the next tick fetched. That
    // pins the sleep+continue behavior versus a parking regression.
    auth.clear();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        spotify.call_count() >= 1,
        "after clearing needs_reauth (no fresh touch), loop must resume \
         fetching — proving the reauth branch slept instead of parking; \
         got call_count = {}",
        spotify.call_count(),
    );
    assert!(
        state.snapshots.get(EndpointKind::Now).is_some(),
        "post-reauth-clear tick must populate the snapshot",
    );
}
