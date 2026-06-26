//! End-to-end shape tests for /v1/recent, /v1/top/tracks, /v1/playlists,
//! and the multi-call /v1/profile. Each driven against a path-scripted
//! Spotify stub so we exercise the FULL handler — mapping, caching,
//! needs_reauth guard, multi-call aggregation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use music_api::app::state_store::StateStore;
use music_api::config::Config;
use music_api::domain::auth_state::AuthState;
use music_api::domain::oauth_client::{RefreshedTokens, TokenExchangeError, TokenExchanger};
use music_api::domain::spotify::{SpotifyClient, SpotifyError};
use music_api::domain::tokens::{RepoError, TokenRecord, TokenRepository};
use music_api::{app, AppState};
use serde_json::{json, Value};
use tower::util::ServiceExt;

/// Routes each `path` to a programmed Spotify response; counts per-path
/// hits so cache assertions can prove "second request didn't call Spotify".
struct RoutedSpotify {
    by_path: Mutex<HashMap<String, Value>>,
    calls: AtomicUsize,
}

impl RoutedSpotify {
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
}

#[async_trait]
impl SpotifyClient for RoutedSpotify {
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
            expires_at: Utc::now() + Duration::seconds(3600),
            scope: "user-read-private".into(),
            owner_id: "yudhyapw".into(),
        }))
    }
    async fn upsert(&self, _: TokenRecord) -> Result<(), RepoError> { Ok(()) }
    async fn delete(&self) -> Result<(), RepoError> { Ok(()) }
}

struct UnusedExchanger;
#[async_trait]
impl TokenExchanger for UnusedExchanger {
    async fn refresh(&self, _: &str) -> Result<RefreshedTokens, TokenExchangeError> {
        unimplemented!()
    }
    async fn exchange_code(&self, _: &str, _: &str) -> Result<RefreshedTokens, TokenExchangeError> {
        unimplemented!()
    }
}

fn cfg() -> Config {
    Config {
        spotify_client_id: "cid".into(),
        spotify_client_secret: "secret".into(),
        spotify_redirect_uri: "https://x/callback".into(),
        owner_spotify_user_id: "yudhyapw".into(),
        auth_basic_username: "owner".into(),
        auth_basic_password: "pw".into(),
        database_url: "sqlite::memory:".into(),
        mock_data: false,
    }
}

fn build_app(routes: Vec<(&str, Value)>) -> (axum::Router, Arc<RoutedSpotify>) {
    let spotify = Arc::new(RoutedSpotify::new(routes));
    let spotify_dyn: Arc<dyn SpotifyClient> = spotify.clone();
    let state = AppState::new_for_test(
        Arc::new(cfg()),
        Arc::new(MemRepo),
        spotify_dyn,
        Arc::new(UnusedExchanger),
        Arc::new(AuthState::new()),
        Arc::new(StateStore::new()),
    );
    (app(state), spotify)
}

async fn get_body(router: &axum::Router, path: &str) -> (StatusCode, Value) {
    let resp = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

// ---- /v1/profile complete shape -----------------------------------------

#[tokio::test]
async fn profile_aggregates_followers_following_playlists_count() {
    let me = json!({
        "id": "yudyack",
        "display_name": "Yudhya",
        "followers": {"total": 64},
        "images": [{"url": "https://i/a.jpg"}],
        "external_urls": {"spotify": "https://open.spotify.com/user/yudyack"}
    });
    let following = json!({"artists": {"total": 17}});
    let playlists = json!({"items": [], "total": 9});

    let (router, counter) = build_app(vec![
        ("/v1/me", me),
        ("/v1/me/following?type=artist&limit=1", following),
        ("/v1/me/playlists?limit=1", playlists),
    ]);

    let (status, body) = get_body(&router, "/v1/profile").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["display_name"], json!("Yudhya"));
    assert_eq!(body["handle"], json!("yudyack"));
    assert_eq!(body["avatar"], json!("https://i/a.jpg"));
    assert_eq!(body["followers"], json!(64));
    assert_eq!(body["following"], json!(17), "must come from /me/following.artists.total");
    assert_eq!(body["playlists_count"], json!(9), "must come from /me/playlists.total");
    assert_eq!(body["profile_url"], json!("https://open.spotify.com/user/yudyack"));
    assert_eq!(counter.calls.load(Ordering::SeqCst), 3, "exactly 3 Spotify calls");

    // Second request → all from cache, no extra Spotify calls.
    let (_, _) = get_body(&router, "/v1/profile").await;
    assert_eq!(counter.calls.load(Ordering::SeqCst), 3, "cache hit: no new calls");
}

#[tokio::test]
async fn profile_degrades_to_zero_when_count_calls_fail() {
    // /v1/me succeeds; the two count calls 404. Panel still renders.
    let me = json!({
        "id": "yudyack",
        "display_name": "Yudhya",
        "followers": {"total": 64},
        "images": [],
        "external_urls": {"spotify": "https://open.spotify.com/user/yudyack"}
    });
    let (router, _) = build_app(vec![("/v1/me", me)]);
    let (status, body) = get_body(&router, "/v1/profile").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["followers"], json!(64));
    assert_eq!(body["following"], json!(0), "fallback to 0 on count-call failure");
    assert_eq!(body["playlists_count"], json!(0));
}

// ---- /v1/recent --------------------------------------------------------

#[tokio::test]
async fn recent_maps_items_into_spec_shape() {
    let recent = json!({
        "items": [
            {
                "played_at": "2026-06-26T10:00:00Z",
                "track": {
                    "name": "Track A",
                    "duration_ms": 180000,
                    "artists": [{"name": "Artist X"}],
                    "album": {"name": "Album A", "images": [{"url": "https://i/a.jpg"}]}
                }
            }
        ]
    });
    let (router, _) = build_app(vec![("/v1/me/player/recently-played?limit=20", recent)]);
    let (status, body) = get_body(&router, "/v1/recent").await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    let it = &items[0];
    assert_eq!(it["played_at"], json!("2026-06-26T10:00:00Z"));
    assert_eq!(it["track"], json!("Track A"));
    assert_eq!(it["artist"], json!("Artist X"));
    assert_eq!(it["album"], json!("Album A"));
    assert_eq!(it["cover"], json!("https://i/a.jpg"));
    assert_eq!(it["duration_ms"], json!(180000));
}

#[tokio::test]
async fn recent_caches_for_60s() {
    let recent = json!({"items": []});
    let (router, counter) = build_app(vec![("/v1/me/player/recently-played?limit=20", recent)]);
    let _ = get_body(&router, "/v1/recent").await;
    let _ = get_body(&router, "/v1/recent").await;
    assert_eq!(counter.calls.load(Ordering::SeqCst), 1, "criterion 11 — cache hit");
}

// ---- /v1/top/tracks ----------------------------------------------------

#[tokio::test]
async fn top_tracks_assigns_one_indexed_rank() {
    let top = json!({
        "items": [
            {"name": "T1", "duration_ms": 100, "artists": [{"name":"A1"}],
             "album": {"name":"AL1", "images": [{"url":"https://i/1.jpg"}]}},
            {"name": "T2", "duration_ms": 200, "artists": [{"name":"A2"}],
             "album": {"name":"AL2", "images": [{"url":"https://i/2.jpg"}]}}
        ]
    });
    let (router, _) = build_app(vec![
        ("/v1/me/top/tracks?time_range=short_term&limit=10", top),
    ]);
    let (status, body) = get_body(&router, "/v1/top/tracks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["range"], json!("short_term"));
    let items = body["items"].as_array().unwrap();
    assert_eq!(items[0]["rank"], json!(1));
    assert_eq!(items[1]["rank"], json!(2));
    assert_eq!(items[0]["track"], json!("T1"));
    assert_eq!(items[1]["cover"], json!("https://i/2.jpg"));
}

// ---- /v1/playlists -----------------------------------------------------

#[tokio::test]
async fn playlists_returns_items_and_total() {
    let playlists = json!({
        "total": 9,
        "items": [
            {
                "name": "Chill",
                "owner": {"display_name": "Yudhya"},
                "images": [{"url": "https://i/c.jpg"}],
                "tracks": {"total": 42},
                "external_urls": {"spotify": "https://open.spotify.com/playlist/xyz"}
            }
        ]
    });
    let (router, _) = build_app(vec![("/v1/me/playlists?limit=20", playlists)]);
    let (status, body) = get_body(&router, "/v1/playlists").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], json!(9));
    let it = &body["items"][0];
    assert_eq!(it["name"], json!("Chill"));
    assert_eq!(it["owner"], json!("Yudhya"));
    assert_eq!(it["cover"], json!("https://i/c.jpg"));
    assert_eq!(it["tracks_count"], json!(42));
    assert_eq!(it["url"], json!("https://open.spotify.com/playlist/xyz"));
}
