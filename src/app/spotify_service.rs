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
//! Single-flight refresh (criterion 26): concurrent 401s collapse into ONE
//! refresh POST. A `tokio::sync::Mutex` serializes the refresh critical
//! section; the first caller through refreshes and upserts, every later
//! caller re-reads the now-rotated token and reuses it instead of POSTing
//! again. The async mutex is held across the refresh `.await` deliberately
//! — waiters block until the in-flight refresh resolves.

use std::sync::Arc;

use chrono::{Duration, Utc};
use serde_json::Value;
use tokio::sync::Mutex;

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
    /// Serializes the refresh critical section so concurrent 401s collapse
    /// into one refresh POST (criterion 26).
    refresh_lock: Mutex<()>,
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
            refresh_lock: Mutex::new(()),
        }
    }

    /// GET a `/v1/*` path as the owner, transparently refreshing on 401.
    ///
    /// `Ok(None)` is the HTTP 204 No Content case — Spotify uses it on
    /// `/me/player` to mean "nothing currently playing" (criterion 17).
    /// Handlers map that to a domain-specific shape (e.g. `playing:false`).
    ///
    /// Note: this does NOT short-circuit when `AuthState` is already
    /// `NeedsReauth`. Refusing calls in that state — and reporting it via
    /// `/healthz` + `/v1/* 503` — is criterion 6's read-side, which lives
    /// at the handler layer (the entry guard).
    pub async fn get(&self, path: &str) -> Result<Option<Value>, ServiceError> {
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
    ///
    /// Single-flight (criterion 26): the refresh critical section runs under
    /// `refresh_lock`, so concurrent 401s serialize. The first caller through
    /// refreshes and upserts; every later caller observes the rotated token
    /// on re-read and reuses it without POSTing again. The lock is released
    /// before the retry so the retries themselves run concurrently.
    async fn refresh_and_retry(
        &self,
        path: &str,
        current: TokenRecord,
    ) -> Result<Option<Value>, ServiceError> {
        let new_access = {
            let _guard = self.refresh_lock.lock().await;

            // A concurrent caller already found the refresh token dead — do
            // not re-POST it. (This is criterion-26 latch logic, NOT the
            // criterion-6 entry guard, which lives at the handler layer.)
            if self.auth_state.needs_reauth() {
                return Err(ServiceError::NeedsReauth);
            }

            // A concurrent caller may have refreshed while we waited for the
            // lock. If the stored access token has moved on, reuse it.
            let latest = self
                .tokens
                .get()
                .await
                .map_err(|e| ServiceError::Repo(e.to_string()))?
                .ok_or(ServiceError::NeedsReauth)?;

            if latest.access_token != current.access_token {
                latest.access_token
            } else {
                // We are the first concurrent caller: perform the refresh.
                let refreshed = match self.oauth.refresh(&current.refresh_token).await {
                    Ok(r) => r,
                    Err(TokenExchangeError::InvalidGrant) => {
                        // Dead refresh token: flip state, keep stored tokens
                        // for the owner's reauth to overwrite.
                        self.auth_state.set_needs_reauth();
                        return Err(ServiceError::NeedsReauth);
                    }
                    Err(e) => return Err(ServiceError::Upstream(e.to_string())),
                };

                let new_access = refreshed.access_token.clone();
                let record = TokenRecord {
                    access_token: refreshed.access_token,
                    // Spotify only returns a refresh_token when it rotates
                    // one; otherwise keep the existing token.
                    refresh_token: refreshed.refresh_token.unwrap_or(current.refresh_token),
                    expires_at: Utc::now() + Duration::seconds(refreshed.expires_in),
                    scope: refreshed.scope.unwrap_or(current.scope),
                    owner_id: current.owner_id,
                };
                self.tokens
                    .upsert(record)
                    .await
                    .map_err(|e| ServiceError::Repo(e.to_string()))?;
                new_access
            }
        }; // refresh_lock released here — retries run concurrently.

        // Retry ONCE with the fresh access token. A second non-2xx is an
        // upstream failure — no further retry.
        self.spotify
            .get_json(path, &new_access)
            .await
            .map_err(|e| ServiceError::Upstream(e.to_string()))
    }
}
