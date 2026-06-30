//! Owner-only control surface. Today: the Spotify kill switch.
//!
//! Three endpoints, all gated by the same constant-time Basic-auth check
//! `/auth/spotify/login` uses — but applied here as a single axum
//! middleware layer ([`auth_layer`]) rather than per-handler checks, so
//! handler bodies stay focused on intent and a future endpoint added to
//! the `/admin/*` sub-router can't accidentally skip the gate.
//!
//! - `GET  /admin/spotify`         → `{"enabled": bool}` (read)
//! - `POST /admin/spotify/enable`  → `{"enabled": true}`  (allow Spotify calls)
//! - `POST /admin/spotify/disable` → `{"enabled": false}` (stop all Spotify calls)
//!
//! All idempotent. Flipping `disable` parks every scheduler tick and forces
//! `/v1/*` to serve cached snapshots only; flipping `enable` resumes the
//! scheduler loops within one tick interval (see
//! `domain::spotify_toggle::SpotifyToggle`).

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum_extra::headers::authorization::Basic;
use axum_extra::headers::Authorization;
use axum_extra::TypedHeader;
use serde_json::json;

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
