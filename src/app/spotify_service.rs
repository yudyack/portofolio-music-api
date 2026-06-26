//! `SpotifyService` — the orchestration layer between the HTTP handlers
//! and the raw `SpotifyClient`. It owns the token lifecycle around an
//! outbound call: read the stored access token, make the call, and on a
//! 401 refresh the token, persist the rotated set, and retry the call
//! ONCE (criterion 10 / spec §5.5).
//!
//! A refresh that fails with `invalid_grant` flips the shared `AuthState`
//! to `NeedsReauth` and surfaces `ServiceError::NeedsReauth`; the stored
//! tokens are left in place (criterion 6 spirit — the owner reauthing
//! upserts over them). Any other refresh failure, or a retry that is
//! still non-2xx, surfaces as `ServiceError::Upstream` — no second retry,
//! no tight loop.
//!
//! Single-flight refresh (criterion 26 — collapsing concurrent 401s into
//! ONE refresh POST) is a separate criterion and is NOT implemented here
//! yet; today two concurrent 401s could each trigger a refresh. That lands
//! in its own cycle.

use std::sync::Arc;

use chrono::{Duration, Utc};
use serde_json::Value;

use crate::domain::auth_state::AuthState;
use crate::domain::oauth_client::{TokenExchangeError, TokenExchanger};
use crate::domain::spotify::{SpotifyClient, SpotifyError};
use crate::domain::tokens::{TokenRecord, TokenRepository};

/// Failure surface the HTTP handlers map to status codes (in the handler
/// cycle): `NeedsReauth` → 503 `{error:"needs_reauth"}`, `Upstream` → 502 /
/// last-good cache, `Repo` → 500-class. Detail strings never carry secret
/// material (criterion 13).
#[derive(Debug)]
pub enum ServiceError {
    /// The refresh token is dead (or no token is stored). Only a fresh
    /// owner authorization recovers.
    NeedsReauth,
    /// Upstream Spotify failure after retries are exhausted.
    Upstream(String),
    /// Token storage failure.
    Repo(String),
}

pub struct SpotifyService {
    tokens: Arc<dyn TokenRepository>,
    spotify: Arc<dyn SpotifyClient>,
    oauth: Arc<dyn TokenExchanger>,
    auth_state: Arc<AuthState>,
}

impl SpotifyService {
    pub fn new(
        tokens: Arc<dyn TokenRepository>,
        spotify: Arc<dyn SpotifyClient>,
        oauth: Arc<dyn TokenExchanger>,
        auth_state: Arc<AuthState>,
    ) -> Self {
        Self {
            tokens,
            spotify,
            oauth,
            auth_state,
        }
    }

    /// GET a `/v1/*` path as the owner, transparently refreshing on 401.
    ///
    /// Note: this does NOT short-circuit when `AuthState` is already
    /// `NeedsReauth`. Refusing calls in that state — and reporting it via
    /// `/healthz` + `/v1/* 503` — is criterion 6's read-side, which lands
    /// with those handlers. Cycle 11 only SETS the flag (on `invalid_grant`).
    pub async fn get(&self, path: &str) -> Result<Value, ServiceError> {
        let token = self
            .tokens
            .get()
            .await
            .map_err(|e| ServiceError::Repo(e.to_string()))?
            .ok_or(ServiceError::NeedsReauth)?;

        match self.spotify.get_json(path, &token.access_token).await {
            Ok(v) => Ok(v),
            Err(SpotifyError::Status(401)) => self.refresh_and_retry(path, token).await,
            Err(e) => Err(ServiceError::Upstream(e.to_string())),
        }
    }

    /// Refresh the token then retry the call exactly once.
    async fn refresh_and_retry(
        &self,
        path: &str,
        current: TokenRecord,
    ) -> Result<Value, ServiceError> {
        let refreshed = match self.oauth.refresh(&current.refresh_token).await {
            Ok(r) => r,
            Err(TokenExchangeError::InvalidGrant) => {
                // Dead refresh token: flip state, keep stored tokens for the
                // owner's reauth to overwrite.
                self.auth_state.set_needs_reauth();
                return Err(ServiceError::NeedsReauth);
            }
            Err(e) => return Err(ServiceError::Upstream(e.to_string())),
        };

        let new_access = refreshed.access_token.clone();
        let record = TokenRecord {
            access_token: refreshed.access_token,
            // Spotify only returns a refresh_token when it rotates one;
            // otherwise keep the existing token.
            refresh_token: refreshed.refresh_token.unwrap_or(current.refresh_token),
            expires_at: Utc::now() + Duration::seconds(refreshed.expires_in),
            scope: refreshed.scope.unwrap_or(current.scope),
            owner_id: current.owner_id,
        };
        self.tokens
            .upsert(record)
            .await
            .map_err(|e| ServiceError::Repo(e.to_string()))?;

        // Retry ONCE with the fresh access token. A second non-2xx is an
        // upstream failure — no further retry.
        self.spotify
            .get_json(path, &new_access)
            .await
            .map_err(|e| ServiceError::Upstream(e.to_string()))
    }
}
