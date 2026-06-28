//! Per-endpoint scheduler tasks. One `tokio::spawn` per `/v1/*` endpoint;
//! each loop fetches Spotify on the configured interval and stores the
//! mapped payload into the matching `Snapshots` cell.
//!
//! Each loop honors two gates:
//! 1. **Activity gate** — when no `/v1/*` request has landed in the last
//!    `idle_threshold`, the loop parks on `ActivityTracker::woke.notified()`
//!    instead of burning Spotify quota. Middleware's `touch()` flushes
//!    waiters when activity crosses the threshold from below.
//! 2. **Reauth gate** — if `AuthState::needs_reauth()` is true the loop
//!    skips the fetch but does NOT park (a sleep + `continue` keeps the
//!    loop responsive to a future re-link without losing its wake-up).
//!
//! The fetch+map paths share `app::v1_mapper` with the synchronous
//! cold-start fallback in `routes::v1`, so the scheduler and the handler
//! cannot drift on shape.

use std::time::Duration;

use serde_json::{json, Value};
use tokio::time::sleep;

use crate::app::snapshots::EndpointKind;
use crate::app::spotify_service::ServiceError;
use crate::app::v1_mapper;
use crate::state::AppState;

/// Spawn one background tick loop per `/v1/*` endpoint. Called once at
/// startup from `lib::init`. Returns immediately; the spawned tasks live
/// until the process exits.
pub fn spawn_schedulers(state: AppState) {
    let intervals = state.config.scheduler.intervals.clone();
    spawn_one(state.clone(), EndpointKind::Now, intervals.now);
    spawn_one(state.clone(), EndpointKind::Recent, intervals.recent);
    spawn_one(state.clone(), EndpointKind::Top, intervals.top);
    spawn_one(state.clone(), EndpointKind::Profile, intervals.profile);
    spawn_one(state, EndpointKind::Playlists, intervals.playlists);
}

/// Spawn a single scheduler tick loop. Exposed (rather than private) so
/// integration tests in `tests/scheduler_loop.rs` can drive one loop at a
/// time with a millisecond interval and a tracker they control, instead
/// of going through `spawn_schedulers` and waiting on the production
/// 3-second/15-minute cadence.
pub fn spawn_one(state: AppState, kind: EndpointKind, interval: Duration) {
    tokio::spawn(async move {
        loop {
            // Register the notify intent BEFORE checking is_active, so a
            // `touch()` that fires between the check and the `await` is
            // not lost. `notify_waiters` only flushes already-registered
            // waiters; this is the classic register-before-check pattern.
            let notified = state.activity.woke.notified();
            tokio::pin!(notified);
            if !state.activity.is_active() {
                notified.await;
            }

            if state.auth_state.needs_reauth() {
                // Don't park — auth might recover via the OAuth callback,
                // and we want the next tick to pick it up. Just sleep the
                // interval and try again.
                sleep(interval).await;
                continue;
            }

            match fetch_and_map(&state, kind).await {
                Ok(v) => state.snapshots.set(kind, Some(v)),
                Err(e) => tracing::warn!(?kind, error = %e, "scheduler tick failed"),
            }
            sleep(interval).await;
        }
    });
}

/// Fetch+map an endpoint into the spec §5.7 shape. Returns the payload
/// ready to store in the snapshot cell. Shared with the handler-fallback
/// path in `routes::v1`.
pub(crate) async fn fetch_and_map(
    state: &AppState,
    kind: EndpointKind,
) -> Result<Value, FetchError> {
    let payload = match kind {
        EndpointKind::Now => fetch_now(state).await?,
        EndpointKind::Recent => fetch_recent(state).await?,
        EndpointKind::Top => fetch_top(state).await?,
        EndpointKind::Profile => fetch_profile(state).await?,
        EndpointKind::Playlists => fetch_playlists(state).await?,
    };
    Ok(tag_mock(state, payload))
}

/// Tag the payload with `_mock: true` when running with `MOCK_DATA=1`.
/// Identical to the v1.rs helper — kept here so the scheduler-store path
/// also marks its snapshot, not just the wire copy.
fn tag_mock(state: &AppState, mut payload: Value) -> Value {
    if state.config.mock_data {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("_mock".to_string(), Value::Bool(true));
        }
    }
    payload
}

async fn fetch_now(state: &AppState) -> Result<Value, FetchError> {
    Ok(match state.spotify_service.get("/v1/me/player").await? {
        Some(v) => v1_mapper::map_now(&v),
        // Criterion 17: 204 → playing:false (200, not 500). Snapshot this
        // shape so subsequent /v1/now hits get the same response.
        None => json!({"playing": false}),
    })
}

async fn fetch_recent(state: &AppState) -> Result<Value, FetchError> {
    let raw = state
        .spotify_service
        .get("/v1/me/player/recently-played?limit=20")
        .await?
        .unwrap_or_else(|| json!({"items": []}));
    Ok(v1_mapper::map_recent(&raw))
}

async fn fetch_top(state: &AppState) -> Result<Value, FetchError> {
    let raw = state
        .spotify_service
        .get("/v1/me/top/tracks?time_range=short_term&limit=10")
        .await?
        .unwrap_or_else(|| json!({"items": []}));
    Ok(v1_mapper::map_top_tracks(&raw))
}

async fn fetch_profile(state: &AppState) -> Result<Value, FetchError> {
    let me = state
        .spotify_service
        .get("/v1/me")
        .await?
        .ok_or_else(|| FetchError::Upstream("/v1/me returned 204".to_string()))?;
    // Same degradation policy as the previous cache-fronted handler: a
    // failure of the count subcall reports 0 rather than failing the whole
    // panel — better UX than blanking the profile over a single missing
    // field.
    let following = match state
        .spotify_service
        .get("/v1/me/following?type=artist&limit=1")
        .await
    {
        Ok(Some(v)) => v1_mapper::total_in(&v.get("artists").cloned().unwrap_or(Value::Null)),
        _ => 0,
    };
    let playlists_count = match state.spotify_service.get("/v1/me/playlists?limit=1").await {
        Ok(Some(v)) => v1_mapper::total_in(&v),
        _ => 0,
    };
    Ok(v1_mapper::map_profile(&me, following, playlists_count))
}

async fn fetch_playlists(state: &AppState) -> Result<Value, FetchError> {
    let raw = state
        .spotify_service
        .get("/v1/me/playlists?limit=20")
        .await?
        .unwrap_or_else(|| json!({"items": [], "total": 0}));
    Ok(v1_mapper::map_playlists(&raw))
}

/// Internal error surface for the fetch_and_map helpers. The scheduler
/// loop logs it and moves on; the handler converts it to a proper HTTP
/// response.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FetchError {
    #[error("needs_reauth")]
    NeedsReauth,
    #[error("upstream: {0}")]
    Upstream(String),
    #[error("repo: {0}")]
    Repo(String),
}

impl From<ServiceError> for FetchError {
    fn from(e: ServiceError) -> Self {
        match e {
            ServiceError::NeedsReauth => FetchError::NeedsReauth,
            ServiceError::Upstream(s) => FetchError::Upstream(s),
            ServiceError::Repo(s) => FetchError::Repo(s),
        }
    }
}
