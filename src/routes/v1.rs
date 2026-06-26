//! `/v1/*` handlers — anonymous, cached JSON for the leptos frontend.
//!
//! Pattern every handler follows:
//! 1. Criterion-6 guard: if `AuthState::needs_reauth()`, return 503
//!    `{error:"needs_reauth"}` WITHOUT calling Spotify.
//! 2. Cache lookup: serve from cache if present (criterion 11).
//! 3. Call `SpotifyService::get(...)` — that layer owns refresh-on-401 +
//!    single-flight refresh (criteria 10, 26).
//! 4. Map the raw `/me`-style payload into the spec §5.7 shape.
//! 5. Store in cache, return 200.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value};

use crate::app::spotify_service::ServiceError;
use crate::state::AppState;

const PROFILE_CACHE_KEY: &str = "v1:profile";
const PROFILE_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Serialize)]
pub struct ProfileBody {
    pub display_name: Option<String>,
    pub handle: String,
    pub avatar: Option<String>,
    pub followers: u64,
    pub profile_url: Option<String>,
}

/// GET /v1/profile — owner's Spotify profile from `/me`.
///
/// Spec §5.7 names additional fields (`following`, `playlists_count`) that
/// require separate `/me/following` + `/me/playlists` calls — deferred to a
/// later cycle. Criterion 18 remains PARTIAL for this endpoint until then.
pub async fn profile(State(state): State<AppState>) -> Response {
    if state.auth_state.needs_reauth() {
        return needs_reauth();
    }

    if let Some(cached) = state.cache.get(PROFILE_CACHE_KEY) {
        return (StatusCode::OK, Json(cached)).into_response();
    }

    let raw = match state.spotify_service.get("/v1/me").await {
        // /v1/me always returns a body; treat 204 as upstream weirdness.
        Ok(Some(v)) => v,
        Ok(None) => {
            tracing::warn!("/v1/me returned 204 unexpectedly");
            return (StatusCode::BAD_GATEWAY, Json(json!({"error": "upstream"})))
                .into_response();
        }
        Err(ServiceError::NeedsReauth) => return needs_reauth(),
        Err(ServiceError::Upstream(e)) => {
            tracing::warn!(error = %e, "spotify /v1/me failed");
            return (StatusCode::BAD_GATEWAY, Json(json!({"error": "upstream"})))
                .into_response();
        }
        Err(ServiceError::Repo(e)) => {
            tracing::error!(error = %e, "token repo lookup failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "repo"})))
                .into_response();
        }
    };

    let body = map_profile(&raw);
    let payload = serde_json::to_value(&body).expect("ProfileBody is always serializable");
    state.cache.put(PROFILE_CACHE_KEY.to_string(), payload.clone(), PROFILE_TTL);
    (StatusCode::OK, Json(payload)).into_response()
}

fn needs_reauth() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "needs_reauth"})),
    )
        .into_response()
}

/// Map Spotify `/me` JSON to the §5.7 profile shape. Picks the largest
/// available avatar (Spotify orders `images` largest-first when sizes are
/// reported; otherwise we take the first entry as a stable choice).
fn map_profile(me: &Value) -> ProfileBody {
    let display_name = me
        .get("display_name")
        .and_then(Value::as_str)
        .map(str::to_string);
    let handle = me
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let avatar = me
        .get("images")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|img| img.get("url"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let followers = me
        .get("followers")
        .and_then(|f| f.get("total"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let profile_url = me
        .get("external_urls")
        .and_then(|u| u.get("spotify"))
        .and_then(Value::as_str)
        .map(str::to_string);
    ProfileBody {
        display_name,
        handle,
        avatar,
        followers,
        profile_url,
    }
}
