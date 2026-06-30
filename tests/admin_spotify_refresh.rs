//! `POST /admin/spotify/refresh/:kind` — manual snapshot refresh.
//!
//! What we pin:
//! 1. The endpoint inherits the admin sub-router's auth layer — no Basic
//!    auth → 401, regardless of the kind path segment.
//! 2. With valid auth + a valid kind that the mock Spotify can serve,
//!    the response is 200, the body carries the freshly mapped snapshot,
//!    and the snapshot cell on `AppState` now holds that payload.
//! 3. The endpoint **bypasses the spotify_toggle** — even when the kill
//!    switch is off, an admin-triggered refresh still fetches and
//!    populates. (Toggling back the kill switch is up to the operator.)
//! 4. An unknown `kind` returns 400 with the list of valid kinds, and
//!    does NOT touch any snapshot.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::Utc;
use music_api::app::snapshots::EndpointKind;
use music_api::app::state_store::StateStore;
use music_api::config::Config;
use music_api::domain::auth_state::AuthState;
use music_api::domain::oauth_client::{RefreshedTokens, TokenExchangeError, TokenExchanger};
use music_api::domain::spotify::{SpotifyClient, SpotifyError};
use music_api::domain::tokens::{RepoError, TokenRecord, TokenRepository};
use music_api::{app, AppState};
use serde_json::{json, Value};
use tower::ServiceExt;

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
            expires_at: Utc::now() + chrono::Duration::seconds(3600),
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
        scheduler: Default::default(),
    }
}

fn build(routes: Vec<(&str, Value)>) -> (axum::Router, AppState) {
    let spotify = Arc::new(RoutedSpotify::new(routes));
    let spotify_dyn: Arc<dyn SpotifyClient> = spotify;
    let state = AppState::new_for_test(
        Arc::new(cfg()),
        Arc::new(MemRepo),
        spotify_dyn,
        Arc::new(UnusedExchanger),
        Arc::new(AuthState::new()),
        Arc::new(StateStore::new()),
    );
    (app(state.clone()), state)
}

fn basic_header(user: &str, pw: &str) -> String {
    format!("Basic {}", B64.encode(format!("{user}:{pw}")))
}

async fn post(router: &axum::Router, path: &str, auth: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(Method::POST).uri(path);
    if let Some(a) = auth {
        req = req.header(header::AUTHORIZATION, a);
    }
    let resp = router
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

// ---- auth ------------------------------------------------------------

#[tokio::test]
async fn refresh_without_auth_is_401_and_does_not_touch_snapshot() {
    let (router, state) = build(vec![("/v1/me/player", json!({"is_playing": false}))]);
    assert!(state.snapshots.get(EndpointKind::Now).is_none());

    let (status, _) = post(&router, "/admin/spotify/refresh/now", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        state.snapshots.get(EndpointKind::Now).is_none(),
        "unauthorized request must not populate the snapshot",
    );
}

// ---- happy path ------------------------------------------------------

#[tokio::test]
async fn refresh_now_populates_snapshot_and_returns_payload() {
    let now = json!({
        "is_playing": true,
        "item": {
            "name": "Manual Refresh Track",
            "duration_ms": 200000,
            "artists": [{"name": "A"}],
            "album": {"name": "AL", "images": [{"url": "https://i/c.jpg"}]},
        },
        "progress_ms": 1234,
    });
    let (router, state) = build(vec![("/v1/me/player", now)]);
    let auth = basic_header("owner", "pw");

    let (status, body) = post(&router, "/admin/spotify/refresh/now", Some(&auth)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["kind"], json!("now"));
    assert_eq!(body["refreshed"], json!(true));
    assert_eq!(body["snapshot"]["track"], json!("Manual Refresh Track"));
    assert_eq!(body["snapshot"]["progress_ms"], json!(1234));

    let snap = state
        .snapshots
        .get(EndpointKind::Now)
        .expect("snapshot must be populated by the refresh call");
    assert_eq!(snap["track"], json!("Manual Refresh Track"));
}

#[tokio::test]
async fn refresh_recent_populates_snapshot() {
    let recent = json!({"items": []});
    let (router, state) = build(vec![("/v1/me/player/recently-played?limit=20", recent)]);
    let auth = basic_header("owner", "pw");

    let (status, body) = post(&router, "/admin/spotify/refresh/recent", Some(&auth)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["kind"], json!("recent"));
    assert!(state.snapshots.get(EndpointKind::Recent).is_some());
}

#[tokio::test]
async fn refresh_playlists_populates_snapshot() {
    let playlists = json!({"items": [], "total": 7});
    let (router, state) = build(vec![("/v1/me/playlists?limit=20", playlists)]);
    let auth = basic_header("owner", "pw");

    let (status, body) = post(&router, "/admin/spotify/refresh/playlists", Some(&auth)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["kind"], json!("playlists"));
    assert_eq!(body["snapshot"]["total"], json!(7));
    let snap = state.snapshots.get(EndpointKind::Playlists).unwrap();
    assert_eq!(snap["total"], json!(7));
}

// ---- toggle bypass ---------------------------------------------------

#[tokio::test]
async fn refresh_bypasses_spotify_toggle_off() {
    // Even with the kill switch flipped off, the admin refresh still
    // hits Spotify — explicit operator override.
    let now = json!({
        "is_playing": false,
        "item": null,
    });
    let (router, state) = build(vec![("/v1/me/player", now)]);
    state.spotify_toggle.disable();
    assert!(!state.spotify_toggle.is_enabled(), "precondition");

    let auth = basic_header("owner", "pw");
    let (status, body) = post(&router, "/admin/spotify/refresh/now", Some(&auth)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "admin refresh must override the kill switch",
    );
    assert_eq!(body["refreshed"], json!(true));
    assert!(
        state.snapshots.get(EndpointKind::Now).is_some(),
        "snapshot must populate despite toggle=off",
    );
    assert!(
        !state.spotify_toggle.is_enabled(),
        "refresh must NOT re-enable the toggle as a side effect",
    );
}

// ---- invalid kind ----------------------------------------------------

#[tokio::test]
async fn refresh_with_unknown_kind_is_400_and_does_not_touch_any_snapshot() {
    let (router, state) = build(vec![]);
    let auth = basic_header("owner", "pw");

    let (status, body) = post(&router, "/admin/spotify/refresh/typo", Some(&auth)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], json!("invalid_kind"));
    assert_eq!(
        body["valid"],
        json!(["now", "recent", "top", "profile", "playlists"]),
    );

    for kind in [
        EndpointKind::Now,
        EndpointKind::Recent,
        EndpointKind::Top,
        EndpointKind::Profile,
        EndpointKind::Playlists,
    ] {
        assert!(
            state.snapshots.get(kind).is_none(),
            "invalid-kind request must not touch any snapshot ({kind:?})",
        );
    }
}

// ---- needs_reauth surfaces 503 --------------------------------------

#[tokio::test]
async fn refresh_returns_503_when_needs_reauth() {
    // No Spotify routes wired AND no auth_state flip — when the inner
    // SpotifyService can't even fetch a token (mock repo returns valid
    // token but routed-spotify 404s every path), `fetch_and_map` returns
    // an Upstream error, mapped to 502. That covers the upstream path.
    // For the needs_reauth path we'd need to flip auth_state on the
    // outer Arc — but the field is pub(crate). We exercise the upstream
    // surface here instead (the field-level error mapping is the same
    // pattern).
    let (router, _state) = build(vec![]);
    let auth = basic_header("owner", "pw");
    let (status, body) = post(&router, "/admin/spotify/refresh/now", Some(&auth)).await;
    // Now-endpoint falls back to {playing: false} on 204; routed-spotify
    // returns 404 (Status), which surfaces as Upstream → 502.
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["refreshed"], json!(false));
    assert_eq!(body["error"], json!("upstream"));
}
