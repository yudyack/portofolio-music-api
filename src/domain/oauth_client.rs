//! OAuth token-endpoint client (`accounts.spotify.com/api/token`).
//!
//! Separate from [`crate::domain::spotify::SpotifyClient`] on purpose:
//! different host (accounts vs api), different auth (HTTP Basic client
//! credentials vs per-call Bearer), and a different failure surface
//! (`invalid_grant` → reauthorization).
//!
//! Cycle 11 ships only `refresh` (criterion 10 — the refresh-on-401 path).
//! The callback's `authorization_code` grant (criteria 3, 4) will join
//! this trait in the OAuth-callback cycle; it is deliberately NOT
//! pre-armed here (workflow-notes §8 lever 7).

use async_trait::async_trait;
use thiserror::Error;

/// A freshly-minted token-set from a successful refresh.
///
/// `refresh_token` is optional because Spotify only returns a new one when
/// it ROTATES the refresh token; when absent the caller keeps the existing
/// one. `expires_in` is the lifetime in seconds the caller adds to "now"
/// to compute `expires_at`.
#[derive(Debug, Clone)]
pub struct RefreshedTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: i64,
    pub scope: Option<String>,
}

/// Token-endpoint failure surface. Detail strings never carry the Client
/// Secret (criterion 13): the source chain is logged at the infra boundary,
/// not stringified into these variants beyond the endpoint's own message.
#[derive(Debug, Error)]
pub enum TokenExchangeError {
    /// The refresh token was rejected (`400 {"error":"invalid_grant"}`).
    /// The caller transitions to `NeedsReauth` — only a fresh owner
    /// authorization can recover.
    #[error("refresh token rejected (invalid_grant) — reauthorization required")]
    InvalidGrant,

    #[error("token endpoint transport error: {0}")]
    Transport(String),

    #[error("token endpoint returned status {0}")]
    Status(u16),

    #[error("token endpoint response decode failed: {0}")]
    Decode(String),
}

#[async_trait]
pub trait TokenExchanger: Send + Sync {
    /// Exchange a `refresh_token` for a fresh access token
    /// (`grant_type=refresh_token`). The implementor supplies the app's
    /// client credentials; callers pass only the refresh token.
    async fn refresh(&self, refresh_token: &str) -> Result<RefreshedTokens, TokenExchangeError>;
}
