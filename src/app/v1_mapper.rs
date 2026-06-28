//! Pure mapping functions from raw Spotify JSON to the spec §5.7 wire
//! shapes. Shared between the synchronous `/v1/*` handler fallback path
//! and the per-endpoint scheduler tasks so the scheduler and the handler
//! cannot drift.
//!
//! No I/O here — every function takes `serde_json::Value` and returns
//! `serde_json::Value`. The handler / scheduler is responsible for
//! tagging `_mock:true` if `Config::mock_data` is on.

use serde_json::{json, Value};

pub fn total_in(v: &Value) -> u64 {
    v.get("total").and_then(Value::as_u64).unwrap_or(0)
}

pub fn artists_joined(track_obj: &Value) -> String {
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

pub fn first_image_url(container: &Value) -> String {
    container
        .get("images")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|img| img.get("url"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

// ---- /v1/profile -------------------------------------------------------

pub fn map_profile(me: &Value, following: u64, playlists_count: u64) -> Value {
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

// ---- /v1/now -----------------------------------------------------------

pub fn map_now(p: &Value) -> Value {
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
    let playing = p
        .get("is_playing")
        .and_then(Value::as_bool)
        .unwrap_or(false);
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

pub fn map_recent(raw: &Value) -> Value {
    let items: Vec<Value> = raw
        .get("items")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(map_recent_item).collect())
        .unwrap_or_default();
    json!({ "items": items })
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

pub fn map_top_tracks(raw: &Value) -> Value {
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
    json!({ "range": "short_term", "items": items })
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

pub fn map_playlists(raw: &Value) -> Value {
    let total = raw.get("total").and_then(Value::as_u64).unwrap_or(0);
    let items: Vec<Value> = raw
        .get("items")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(map_playlist_item).collect())
        .unwrap_or_default();
    json!({ "items": items, "total": total })
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
