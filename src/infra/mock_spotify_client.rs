//! `MockSpotifyClient` — path-routed canned Spotify responses for local
//! development (`MOCK_DATA=1`). Bypasses every network/auth concern so a
//! developer can `cargo run` without setting Spotify creds or going
//! through the OAuth bootstrap.
//!
//! Fixtures live as JSON under `mock_fixtures/` and are embedded at compile
//! time with `include_str!`. Shapes mirror real Spotify responses
//! (the handlers in `src/routes/v1.rs` apply the same mapping regardless of
//! source) — the `_mock` marker that distinguishes mock from real data is
//! added by the handler layer, not here.
//!
//! Lives under `src/infra/` like every other SpotifyClient implementation
//! so criterion 20's repository-pattern / reqwest-isolation guards stay
//! green even though this impl is `reqwest`-free.

use async_trait::async_trait;
use serde_json::Value;

use crate::domain::spotify::{SpotifyClient, SpotifyError};

const ME: &str = include_str!("../../mock_fixtures/me.json");
const ME_FOLLOWING: &str = include_str!("../../mock_fixtures/me_following.json");
const ME_PLAYLISTS: &str = include_str!("../../mock_fixtures/me_playlists.json");
const ME_PLAYER: &str = include_str!("../../mock_fixtures/me_player.json");
const ME_PLAYER_RECENT: &str =
    include_str!("../../mock_fixtures/me_player_recently_played.json");
const ME_TOP_TRACKS: &str = include_str!("../../mock_fixtures/me_top_tracks.json");

pub struct MockSpotifyClient;

impl MockSpotifyClient {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MockSpotifyClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SpotifyClient for MockSpotifyClient {
    /// Routes `path` to a canned fixture. Path-matching strips the query
    /// string and matches on the route shape — so the handler's
    /// `/v1/me/following?type=artist&limit=1` and the same path with a
    /// different limit both resolve to the same fixture, which is what a
    /// real proxy would do.
    async fn get_json(
        &self,
        path: &str,
        _access_token: &str,
    ) -> Result<Option<Value>, SpotifyError> {
        let bare = path.split('?').next().unwrap_or(path);
        let raw = match bare {
            "/v1/me" => ME,
            "/v1/me/following" => ME_FOLLOWING,
            "/v1/me/playlists" => ME_PLAYLISTS,
            "/v1/me/player" => ME_PLAYER,
            "/v1/me/player/recently-played" => ME_PLAYER_RECENT,
            "/v1/me/top/tracks" => ME_TOP_TRACKS,
            other => {
                // Unknown paths surface as the same 404 the real Spotify
                // would return — handlers map this through ServiceError::Upstream.
                tracing::warn!(path = %other, "MockSpotifyClient: unmapped path");
                return Err(SpotifyError::Status(404));
            }
        };
        serde_json::from_str::<Value>(raw)
            .map(Some)
            .map_err(|e| SpotifyError::Decode(format!("mock fixture parse: {e}")))
    }
}
