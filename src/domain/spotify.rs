use async_trait::async_trait;
use thiserror::Error;

/// Outbound Spotify call failure surface. Variants carry stringified
/// detail; the source chain is logged at the implementor's boundary so
/// the Bearer header and Client Secret cannot leak via `Display`
/// (criterion 13).
#[derive(Debug, Error)]
pub enum SpotifyError {
    #[error("spotify transport error: {0}")]
    Transport(String),

    #[error("spotify returned status {0}")]
    Status(u16),

    #[error("spotify response decode failed: {0}")]
    Decode(String),
}

/// Outbound Spotify API client.
///
/// Cycle 8 ships a single primitive method (`get_json`). Typed
/// per-endpoint wrappers (e.g. `currently_playing`, `recently_played`,
/// `top_tracks`) land in cycle 11+ where their `/v1/*` callers
/// materialize. The OAuth-side `POST /api/token` (refresh_token grant)
/// is deferred to cycle 10, where the refresh-on-401 RED can pin
/// "refresh actually ran" against a wiremock returning the rotated
/// token-set — introducing it here would leave a stub cycle 10 has to
/// disambiguate.
///
/// Pacing per spec §5.5 (≤ 30 req / 30 s) is an implementation property,
/// not a trait contract. The in-repo `ReqwestSpotifyClient` honors it
/// via `governor`; alternate implementors are responsible for the
/// equivalent themselves.
#[async_trait]
pub trait SpotifyClient: Send + Sync {
    /// GET the path under the client's base URL with
    /// `Authorization: Bearer {access_token}`, parse the 2xx body as
    /// JSON.
    ///
    /// Returns `Ok(None)` on HTTP 204 No Content — Spotify uses this
    /// for "nothing currently playing" on `/me/player` (criterion 17).
    /// Every other 2xx returns `Ok(Some(value))`.
    ///
    /// `path` is appended to `base_url` verbatim — callers pass the
    /// full Spotify path including the version segment
    /// (e.g. `"/v1/me"`, `"/v1/me/player"`). The base URL therefore
    /// EXCLUDES `/v1`; production wiring is `https://api.spotify.com`.
    async fn get_json(
        &self,
        path: &str,
        access_token: &str,
    ) -> Result<Option<serde_json::Value>, SpotifyError>;
}
