//! Owner-only control surface. Today: the Spotify kill switch.
//!
//! Three endpoints, all gated by the same constant-time Basic-auth check
//! `/auth/spotify/login` uses (`routes::auth::basic_auth_ok`):
//!
//! - `GET  /admin/spotify`         → `{"enabled": bool}` (read)
//! - `POST /admin/spotify/enable`  → `{"enabled": true}`  (allow Spotify calls)
//! - `POST /admin/spotify/disable` → `{"enabled": false}` (stop all Spotify calls)
//!
//! All idempotent. Flipping `disable` parks every scheduler tick and forces
//! `/v1/*` to serve cached snapshots only; flipping `enable` resumes the
//! scheduler loops within one tick interval (see
//! `domain::spotify_toggle::SpotifyToggle`).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum_extra::headers::authorization::Basic;
use axum_extra::headers::Authorization;
use axum_extra::TypedHeader;
use serde_json::json;

use crate::routes::auth::{basic_auth_ok, unauthorized};
use crate::AppState;

/// GET /admin/spotify — report current toggle state.
pub async fn get_spotify(
    State(state): State<AppState>,
    auth: Option<TypedHeader<Authorization<Basic>>>,
) -> Response {
    if !basic_auth_ok(&state.config, &auth) {
        return unauthorized();
    }
    (
        StatusCode::OK,
        Json(json!({"enabled": state.spotify_toggle.is_enabled()})),
    )
        .into_response()
}

/// POST /admin/spotify/enable — allow outbound Spotify traffic.
pub async fn enable_spotify(
    State(state): State<AppState>,
    auth: Option<TypedHeader<Authorization<Basic>>>,
) -> Response {
    if !basic_auth_ok(&state.config, &auth) {
        return unauthorized();
    }
    state.spotify_toggle.enable();
    tracing::info!("owner enabled outbound Spotify traffic via /admin/spotify/enable");
    (StatusCode::OK, Json(json!({"enabled": true}))).into_response()
}

/// POST /admin/spotify/disable — stop all outbound Spotify traffic.
pub async fn disable_spotify(
    State(state): State<AppState>,
    auth: Option<TypedHeader<Authorization<Basic>>>,
) -> Response {
    if !basic_auth_ok(&state.config, &auth) {
        return unauthorized();
    }
    state.spotify_toggle.disable();
    tracing::warn!("owner disabled outbound Spotify traffic via /admin/spotify/disable");
    (StatusCode::OK, Json(json!({"enabled": false}))).into_response()
}
