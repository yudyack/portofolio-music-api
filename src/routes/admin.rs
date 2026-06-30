//! Owner-only control surface. Today: the Spotify kill switch plus a
//! manual snapshot refresh.
//!
//! All endpoints are gated by the same constant-time Basic-auth check
//! `/auth/spotify/login` uses — but applied here as a single axum
//! middleware layer ([`auth_layer`]) rather than per-handler checks, so
//! handler bodies stay focused on intent and a future endpoint added to
//! the `/admin/*` sub-router can't accidentally skip the gate.
//!
//! - `GET  /admin/spotify`              → `{"enabled": bool}` (read)
//! - `POST /admin/spotify/enable`       → `{"enabled": true}`  (allow Spotify calls)
//! - `POST /admin/spotify/disable`      → `{"enabled": false}` (stop all Spotify calls)
//! - `POST /admin/spotify/refresh/:kind`→ force a synchronous Spotify
//!   fetch + snapshot store for one endpoint, regardless of scheduler
//!   timing or the kill switch. Useful after re-enabling the toggle, or
//!   when you want fresh data NOW without waiting for the next tick.
//!
//! Toggle endpoints are idempotent. `disable` parks every scheduler tick
//! and forces `/v1/*` to serve cached snapshots only; `enable` resumes
//! the scheduler loops within one tick interval (see
//! `domain::spotify_toggle::SpotifyToggle`).

use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum_extra::headers::authorization::Basic;
use axum_extra::headers::Authorization;
use axum_extra::TypedHeader;
use serde_json::json;

use crate::app::scheduler::{fetch_and_map, FetchError};
use crate::app::snapshots::EndpointKind;
use crate::routes::auth::{basic_auth_ok, unauthorized};
use crate::AppState;

/// axum middleware that gates every `/admin/*` route on the same
/// constant-time Basic-auth check `/auth/spotify/login` uses. Mounted in
/// `lib::app` via `from_fn_with_state` on the admin sub-router so any
/// future `/admin/*` route inherits the gate without an explicit opt-in.
pub async fn auth_layer(
    State(state): State<AppState>,
    auth: Option<TypedHeader<Authorization<Basic>>>,
    request: Request,
    next: Next,
) -> Response {
    if !basic_auth_ok(&state.config, &auth) {
        return unauthorized();
    }
    next.run(request).await
}

/// GET /admin/spotify — report current toggle state.
pub async fn get_spotify(State(state): State<AppState>) -> Response {
    (
        StatusCode::OK,
        Json(json!({"enabled": state.spotify_toggle.is_enabled()})),
    )
        .into_response()
}

/// POST /admin/spotify/enable — allow outbound Spotify traffic.
pub async fn enable_spotify(State(state): State<AppState>) -> Response {
    state.spotify_toggle.enable();
    tracing::info!("owner enabled outbound Spotify traffic via /admin/spotify/enable");
    (StatusCode::OK, Json(json!({"enabled": true}))).into_response()
}

/// POST /admin/spotify/disable — stop all outbound Spotify traffic.
pub async fn disable_spotify(State(state): State<AppState>) -> Response {
    state.spotify_toggle.disable();
    tracing::warn!("owner disabled outbound Spotify traffic via /admin/spotify/disable");
    (StatusCode::OK, Json(json!({"enabled": false}))).into_response()
}

/// Parse a URL-path segment into an `EndpointKind`. Closed enum, not a
/// `FromStr` impl: keeps the only consumer (this route) the only place
/// the string→kind mapping is defined, so a typo elsewhere doesn't
/// silently round-trip.
fn parse_kind(s: &str) -> Option<EndpointKind> {
    match s {
        "now" => Some(EndpointKind::Now),
        "recent" => Some(EndpointKind::Recent),
        "top" => Some(EndpointKind::Top),
        "profile" => Some(EndpointKind::Profile),
        "playlists" => Some(EndpointKind::Playlists),
        _ => None,
    }
}

/// POST /admin/spotify/refresh/:kind — force a synchronous Spotify
/// fetch for one endpoint and store the result in its snapshot cell.
///
/// Calls the same `fetch_and_map` path the scheduler and the handler
/// cold-start use, so the stored shape is identical. **Bypasses the
/// `spotify_toggle`** — an admin-triggered refresh is an explicit
/// override of the kill switch (the operator can always disable again
/// after). Response carries the new snapshot so the operator can verify
/// the result without a follow-up `/v1/*` call.
pub async fn refresh_spotify(
    State(state): State<AppState>,
    Path(kind_str): Path<String>,
) -> Response {
    let Some(kind) = parse_kind(&kind_str) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "invalid_kind",
                "valid": ["now", "recent", "top", "profile", "playlists"],
            })),
        )
            .into_response();
    };

    tracing::info!(
        ?kind,
        "owner triggered manual refresh via /admin/spotify/refresh"
    );

    match fetch_and_map(&state, kind).await {
        Ok(payload) => {
            state.snapshots.set(kind, Some(payload.clone()));
            (
                StatusCode::OK,
                Json(json!({
                    "kind": kind_str,
                    "refreshed": true,
                    "snapshot": payload,
                })),
            )
                .into_response()
        }
        Err(FetchError::NeedsReauth) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "kind": kind_str,
                "refreshed": false,
                "error": "needs_reauth",
            })),
        )
            .into_response(),
        Err(FetchError::Upstream(msg)) => {
            tracing::warn!(?kind, error = %msg, "admin refresh upstream failure");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "kind": kind_str,
                    "refreshed": false,
                    "error": "upstream",
                })),
            )
                .into_response()
        }
        Err(FetchError::Repo(msg)) => {
            tracing::error!(?kind, error = %msg, "admin refresh repo failure");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "kind": kind_str,
                    "refreshed": false,
                    "error": "repo",
                })),
            )
                .into_response()
        }
    }
}
