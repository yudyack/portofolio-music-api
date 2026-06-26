//! `/v1/*` handlers — anonymous, cached JSON for the leptos frontend.
//!
//! Pattern every handler follows:
//! 1. Criterion-6 guard: if `AuthState::needs_reauth()`, return 503
//!    `{error:"needs_reauth"}` WITHOUT calling Spotify.
//! 2. Cache lookup: serve from cache if present (criterion 11).
//! 3. Call `SpotifyService::get(...)` — that layer owns refresh-on-401 +
//!    single-flight refresh (criteria 10, 26).
//! 4. Map the raw Spotify payload into the spec §5.7 shape.
//! 5. Store in cache, return 200.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::app::spotify_service::ServiceError;
use crate::state::AppState;

const PROFILE_CACHE_KEY: &str = "v1:profile";
const PROFILE_TTL: Duration = Duration::from_secs(15 * 60);

const NOW_CACHE_KEY: &str = "v1:now";
const NOW_TTL: Duration = Duration::from_secs(10);

const RECENT_CACHE_KEY: &str = "v1:recent";
const RECENT_TTL: Duration = Duration::from_secs(60);

const TOP_CACHE_KEY: &str = "v1:top:tracks";
const TOP_TTL: Duration = Duration::from_secs(15 * 60);

const PLAYLISTS_CACHE_KEY: &str = "v1:playlists";
const PLAYLISTS_TTL: Duration = Duration::from_secs(15 * 60);

// ---- /v1/profile -------------------------------------------------------

/// GET /v1/profile — three Spotify calls aggregated into the §5.7 shape:
/// `/me` for the bulk + `/me/following?type=artist&limit=1` for `following`
/// + `/me/playlists?limit=1` for `playlists_count`. The totals are read
/// from the `.total` field of the smaller paginated responses (no need to
/// fetch full lists just to count).
pub async fn profile(State(state): State<AppState>) -> Response {
    if state.auth_state.needs_reauth() {
        return needs_reauth();
    }
    if let Some(cached) = state.cache.get(PROFILE_CACHE_KEY) {
        return (StatusCode::OK, Json(cached)).into_response();
    }

    let me = match svc_get(&state, "/v1/me").await {
        Ok(Some(v)) => v,
        Ok(None) => return upstream_204_unexpected("/v1/me"),
        Err(e) => return service_error_to_response(e),
    };
    let following = match svc_get(&state, "/v1/me/following?type=artist&limit=1").await {
        Ok(Some(v)) => total_in(&v.get("artists").cloned().unwrap_or(Value::Null)),
        // Spotify never 204s these; on error/204 degrade to 0 — better UX
        // than failing the whole panel for the count field alone.
        _ => 0,
    };
    let playlists_count = match svc_get(&state, "/v1/me/playlists?limit=1").await {
        Ok(Some(v)) => total_in(&v),
        _ => 0,
    };

    let payload = map_profile(&me, following, playlists_count);
    cache_and_respond(&state, PROFILE_CACHE_KEY, PROFILE_TTL, payload)
}

fn map_profile(me: &Value, following: u64, playlists_count: u64) -> Value {
    let display_name = me.get("display_name").and_then(Value::as_str).unwrap_or("");
    let handle = me.get("id").and_then(Value::as_str).unwrap_or("");
    let avatar = me
        .get("images")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|img| img.get("url"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let followers = me
        .get("followers")
        .and_then(|f| f.get("total"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let profile_url = me
        .get("external_urls")
        .and_then(|u| u.get("spotify"))
        .and_then(Value::as_str)
        .unwrap_or("");
    json!({
        "display_name": display_name,
        "handle": handle,
        "avatar": avatar,
        "followers": followers,
        "following": following,
        "playlists_count": playlists_count,
        "profile_url": profile_url,
    })
}

fn total_in(v: &Value) -> u64 {
    v.get("total").and_then(Value::as_u64).unwrap_or(0)
}

// ---- /v1/now -----------------------------------------------------------

pub async fn now(State(state): State<AppState>) -> Response {
    if state.auth_state.needs_reauth() {
        return needs_reauth();
    }
    if let Some(cached) = state.cache.get(NOW_CACHE_KEY) {
        return (StatusCode::OK, Json(cached)).into_response();
    }
    let payload = match svc_get(&state, "/v1/me/player").await {
        Ok(Some(v)) => map_now(&v),
        // Criterion 17: 204 → playing:false, 200 not 500.
        Ok(None) => json!({"playing": false}),
        Err(e) => return service_error_to_response(e),
    };
    cache_and_respond(&state, NOW_CACHE_KEY, NOW_TTL, payload)
}

fn map_now(p: &Value) -> Value {
    let item = match p.get("item") {
        Some(i) if !i.is_null() => i,
        _ => return json!({"playing": false}),
    };
    let track = item.get("name").and_then(Value::as_str).unwrap_or("");
    let artist = item
        .get("artists")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("name").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let album = item
        .get("album")
        .and_then(|a| a.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let cover = item
        .get("album")
        .and_then(|a| a.get("images"))
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|img| img.get("url"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let progress_ms = p.get("progress_ms").and_then(Value::as_u64).unwrap_or(0);
    let duration_ms = item.get("duration_ms").and_then(Value::as_u64).unwrap_or(0);
    let playing = p.get("is_playing").and_then(Value::as_bool).unwrap_or(false);
    let device = p
        .get("device")
        .and_then(|d| d.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    json!({
        "playing": playing,
        "track": track,
        "artist": artist,
        "album": album,
        "cover": cover,
        "progress_ms": progress_ms,
        "duration_ms": duration_ms,
        "device": device,
    })
}

// ---- /v1/recent --------------------------------------------------------

/// Recently played — `GET /me/player/recently-played?limit=20`. Each item
/// in the response carries `played_at` + `track` (the full track object
/// nested at `.track`); we flatten into the §5.7 shape.
pub async fn recent(State(state): State<AppState>) -> Response {
    if state.auth_state.needs_reauth() {
        return needs_reauth();
    }
    if let Some(cached) = state.cache.get(RECENT_CACHE_KEY) {
        return (StatusCode::OK, Json(cached)).into_response();
    }
    let raw = match svc_get(&state, "/v1/me/player/recently-played?limit=20").await {
        Ok(Some(v)) => v,
        Ok(None) => json!({"items": []}),
        Err(e) => return service_error_to_response(e),
    };
    let items: Vec<Value> = raw
        .get("items")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(map_recent_item).collect())
        .unwrap_or_default();
    let payload = json!({"items": items});
    cache_and_respond(&state, RECENT_CACHE_KEY, RECENT_TTL, payload)
}

fn map_recent_item(entry: &Value) -> Value {
    let played_at = entry.get("played_at").and_then(Value::as_str).unwrap_or("");
    let track_obj = entry.get("track").cloned().unwrap_or(Value::Null);
    let track = track_obj.get("name").and_then(Value::as_str).unwrap_or("");
    let artist = artists_joined(&track_obj);
    let album = track_obj
        .get("album")
        .and_then(|a| a.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let cover = first_image_url(&track_obj.get("album").cloned().unwrap_or(Value::Null));
    let duration_ms = track_obj
        .get("duration_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "played_at": played_at,
        "track": track,
        "artist": artist,
        "album": album,
        "cover": cover,
        "duration_ms": duration_ms,
    })
}

// ---- /v1/top/tracks ----------------------------------------------------

/// Top tracks — `GET /me/top/tracks?time_range=short_term&limit=10`.
/// Spec ships shape `{range, items:[{rank, track, artist, album, cover, duration_ms}]}`;
/// `rank` is 1-indexed (matches the user-visible "#1, #2, …" ordering).
pub async fn top_tracks(State(state): State<AppState>) -> Response {
    if state.auth_state.needs_reauth() {
        return needs_reauth();
    }
    if let Some(cached) = state.cache.get(TOP_CACHE_KEY) {
        return (StatusCode::OK, Json(cached)).into_response();
    }
    let raw = match svc_get(&state, "/v1/me/top/tracks?time_range=short_term&limit=10").await {
        Ok(Some(v)) => v,
        Ok(None) => json!({"items": []}),
        Err(e) => return service_error_to_response(e),
    };
    let items: Vec<Value> = raw
        .get("items")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .enumerate()
                .map(|(i, t)| map_top_track(i as u64 + 1, t))
                .collect()
        })
        .unwrap_or_default();
    let payload = json!({"range": "short_term", "items": items});
    cache_and_respond(&state, TOP_CACHE_KEY, TOP_TTL, payload)
}

fn map_top_track(rank: u64, t: &Value) -> Value {
    json!({
        "rank": rank,
        "track": t.get("name").and_then(Value::as_str).unwrap_or(""),
        "artist": artists_joined(t),
        "album": t
            .get("album")
            .and_then(|a| a.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(""),
        "cover": first_image_url(&t.get("album").cloned().unwrap_or(Value::Null)),
        "duration_ms": t.get("duration_ms").and_then(Value::as_u64).unwrap_or(0),
    })
}

// ---- /v1/playlists -----------------------------------------------------

pub async fn playlists(State(state): State<AppState>) -> Response {
    if state.auth_state.needs_reauth() {
        return needs_reauth();
    }
    if let Some(cached) = state.cache.get(PLAYLISTS_CACHE_KEY) {
        return (StatusCode::OK, Json(cached)).into_response();
    }
    let raw = match svc_get(&state, "/v1/me/playlists?limit=20").await {
        Ok(Some(v)) => v,
        Ok(None) => json!({"items": [], "total": 0}),
        Err(e) => return service_error_to_response(e),
    };
    let total = raw.get("total").and_then(Value::as_u64).unwrap_or(0);
    let items: Vec<Value> = raw
        .get("items")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(map_playlist_item).collect())
        .unwrap_or_default();
    let payload = json!({"items": items, "total": total});
    cache_and_respond(&state, PLAYLISTS_CACHE_KEY, PLAYLISTS_TTL, payload)
}

fn map_playlist_item(p: &Value) -> Value {
    let owner = p
        .get("owner")
        .and_then(|o| o.get("display_name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let tracks_count = p
        .get("tracks")
        .and_then(|t| t.get("total"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let url = p
        .get("external_urls")
        .and_then(|u| u.get("spotify"))
        .and_then(Value::as_str)
        .unwrap_or("");
    json!({
        "name": p.get("name").and_then(Value::as_str).unwrap_or(""),
        "owner": owner,
        "cover": first_image_url(p),
        "tracks_count": tracks_count,
        "url": url,
    })
}

// ---- shared helpers ----------------------------------------------------

fn artists_joined(track_obj: &Value) -> String {
    track_obj
        .get("artists")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("name").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn first_image_url(container: &Value) -> String {
    container
        .get("images")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|img| img.get("url"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

async fn svc_get(state: &AppState, path: &str) -> Result<Option<Value>, ServiceError> {
    state.spotify_service.get(path).await
}

/// Cache the payload, tag it with `_mock:true` if mock mode is on, return
/// it as 200 JSON. The mock tag goes onto BOTH the cached value and the
/// wire value, so subsequent cache hits stay marked.
fn cache_and_respond(state: &AppState, key: &str, ttl: Duration, mut payload: Value) -> Response {
    if state.config.mock_data {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("_mock".to_string(), Value::Bool(true));
        }
    }
    state.cache.put(key.to_string(), payload.clone(), ttl);
    (StatusCode::OK, Json(payload)).into_response()
}

fn needs_reauth() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "needs_reauth"})),
    )
        .into_response()
}

fn upstream_204_unexpected(path: &str) -> Response {
    tracing::warn!(path = %path, "spotify returned 204 unexpectedly");
    (StatusCode::BAD_GATEWAY, Json(json!({"error": "upstream"}))).into_response()
}

fn service_error_to_response(e: ServiceError) -> Response {
    match e {
        ServiceError::NeedsReauth => needs_reauth(),
        ServiceError::Upstream(s) => {
            tracing::warn!(error = %s, "spotify upstream failure");
            (StatusCode::BAD_GATEWAY, Json(json!({"error": "upstream"}))).into_response()
        }
        ServiceError::Repo(s) => {
            tracing::error!(error = %s, "token repo lookup failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "repo"})))
                .into_response()
        }
    }
}
