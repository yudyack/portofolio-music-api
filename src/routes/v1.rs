//! `/v1/*` handlers — anonymous, snapshot-served JSON for the leptos
//! frontend.
//!
//! Pattern every handler follows:
//! 1. Criterion-6 guard: if `AuthState::needs_reauth()`, return 503
//!    `{error:"needs_reauth"}` WITHOUT calling Spotify.
//! 2. Snapshot lookup: if the per-endpoint scheduler task has stored a
//!    payload, return it 200.
//! 3. Cold-start fallback: if the snapshot is empty (server boot, before
//!    the first scheduler tick has resolved), do ONE synchronous fetch +
//!    map via `app::scheduler::fetch_and_map` and store the result. Same
//!    shape as the scheduler stores, so subsequent requests pick up the
//!    cached snapshot transparently.
//!
//! Activity gating (the `ActivityTracker::touch()` that wakes parked
//! schedulers) is handled by axum middleware in `lib::v1_activity_layer`,
//! NOT by these handlers — keeps the touch out of every handler body and
//! correctly fires even when the route is mounted in a sub-router.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::app::scheduler::{fetch_and_map, FetchError};
use crate::app::snapshots::EndpointKind;
use crate::state::AppState;

// ---- /v1/profile -------------------------------------------------------

/// GET /v1/profile — three Spotify calls aggregated into the §5.7 shape:
/// `/me` for the bulk, `/me/following?type=artist&limit=1` for `following`,
/// and `/me/playlists?limit=1` for `playlists_count`. The totals are read
/// from the `.total` field of the smaller paginated responses (no need to
/// fetch full lists just to count).
pub async fn profile(State(state): State<AppState>) -> Response {
    serve(state, EndpointKind::Profile).await
}

// ---- /v1/now -----------------------------------------------------------

pub async fn now(State(state): State<AppState>) -> Response {
    serve(state, EndpointKind::Now).await
}

// ---- /v1/recent --------------------------------------------------------

/// Recently played — `GET /me/player/recently-played?limit=20`. Each item
/// in the response carries `played_at` + `track` (the full track object
/// nested at `.track`); we flatten into the §5.7 shape.
pub async fn recent(State(state): State<AppState>) -> Response {
    serve(state, EndpointKind::Recent).await
}

// ---- /v1/top/tracks ----------------------------------------------------

/// Top tracks — `GET /me/top/tracks?time_range=short_term&limit=10`.
/// Spec ships shape `{range, items:[{rank, track, artist, album, cover, duration_ms}]}`;
/// `rank` is 1-indexed (matches the user-visible "#1, #2, …" ordering).
pub async fn top_tracks(State(state): State<AppState>) -> Response {
    serve(state, EndpointKind::Top).await
}

// ---- /v1/playlists -----------------------------------------------------

pub async fn playlists(State(state): State<AppState>) -> Response {
    serve(state, EndpointKind::Playlists).await
}

// ---- shared 3-step handler ---------------------------------------------

async fn serve(state: AppState, kind: EndpointKind) -> Response {
    tracing::info!(
        target: "music_api::wire::fe",
        direction = "→",
        endpoint = ?kind,
        "frontend request",
    );
    if state.auth_state.needs_reauth() {
        return log_fe_response(kind, 503, json!({"error": "needs_reauth"}));
    }
    if let Some(snapshot) = state.snapshots.get(kind) {
        return log_fe_response(kind, 200, snapshot);
    }
    // Cold start — no scheduler tick has resolved yet. Do ONE synchronous
    // fetch + map (same code path the scheduler uses) and store it so the
    // next visitor lands on the cached snapshot. Concurrent cold-start
    // visitors will race here; we accept the duplicate fetch in exchange
    // for the simpler implementation. The race window is one scheduler
    // interval at most.
    match fetch_and_map(&state, kind).await {
        Ok(payload) => {
            state.snapshots.set(kind, Some(payload.clone()));
            log_fe_response(kind, 200, payload)
        }
        Err(e) => fetch_error_to_response(kind, e),
    }
}

/// Build a JSON response while also emitting `wire::fe` log lines.
/// Returning through this helper keeps the status-code/body that the FE
/// sees and the status-code/body that we log in sync, even as new error
/// branches are added.
///
/// Two log levels: info carries status + endpoint + byte size (cheap
/// state-change signal, safe at default RUST_LOG); debug carries the
/// full body (opt-in via `RUST_LOG=music_api::wire::fe=debug`).
fn log_fe_response(kind: EndpointKind, status: u16, body: serde_json::Value) -> Response {
    let body_str = body.to_string();
    tracing::debug!(
        target: "music_api::wire::fe",
        direction = "←",
        endpoint = ?kind,
        status = status,
        body = %body_str,
        "frontend response (body)",
    );
    tracing::info!(
        target: "music_api::wire::fe",
        direction = "←",
        endpoint = ?kind,
        status = status,
        bytes = body_str.len(),
        "frontend response",
    );
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (code, Json(body)).into_response()
}

fn fetch_error_to_response(kind: EndpointKind, e: FetchError) -> Response {
    match e {
        FetchError::NeedsReauth => log_fe_response(kind, 503, json!({"error": "needs_reauth"})),
        FetchError::Upstream(s) => {
            // Criterion 19: on the cold-start path (snapshot empty) an upstream
            // failure surfaces as 503 `{error:"upstream_unavailable"}` — not
            // 500-class — so the frontend can render a soft "data unavailable"
            // state. A snapshot present case is served upstream of this
            // mapping, so reaching here means no snapshot exists yet.
            tracing::warn!(error = %s, "spotify upstream failure");
            log_fe_response(kind, 503, json!({"error": "upstream_unavailable"}))
        }
        FetchError::Repo(s) => {
            tracing::error!(error = %s, "token repo lookup failed");
            log_fe_response(kind, 500, json!({"error": "repo"}))
        }
    }
}
