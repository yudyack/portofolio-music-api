//! Concrete reqwest-backed `SpotifyClient`. Outbound requests pass
//! through a two-layer middleware chain assembled at construction:
//!
//!   RetryAfterMiddleware (outermost) → GovernorMiddleware (innermost)
//!   → reqwest::Client
//!
//! Cycle 8 acquired the governor permit inline inside `get_json`;
//! cycle 9 lifts both the permit and the 429 retry into middleware so
//! the spec §5.5 invariant — "a 429 retry still consumes a token" —
//! is enforced by chain composition. `tests/spotify_layering.rs` pins
//! the invariant; `tests/spotify_retry_after.rs` pins the Retry-After
//! timing within ±100 ms.
//!
//! 5xx exponential backoff (criterion 9) lands in cycle 10 — either by
//! extending `RetryAfterMiddleware` with a status-5xx branch or by
//! introducing a sibling backoff middleware between
//! `RetryAfterMiddleware` and `GovernorMiddleware`. Either preserves
//! the current attach order.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use governor::{Quota, RateLimiter};
use reqwest_middleware::ClientWithMiddleware;

use crate::domain::spotify::{SpotifyClient, SpotifyError};
use crate::infra::spotify_governor::GovernorMiddleware;
use crate::infra::spotify_retry::RetryAfterMiddleware;

pub struct ReqwestSpotifyClient {
    base_url: String,
    client: ClientWithMiddleware,
}

impl ReqwestSpotifyClient {
    /// Production constructor. Uses the spec §5.5 quota: 30 cells of
    /// burst with 1 cell / s replenishment (so a 30-call burst is paced
    /// to ~30 calls per rolling 30 s steady state).
    pub fn new(base_url: String) -> Result<Self, SpotifyError> {
        let quota = Quota::with_period(Duration::from_secs(1))
            .expect("1 s is non-zero")
            .allow_burst(NonZeroU32::new(30).expect("30 is non-zero"));
        Self::with_quota(base_url, quota)
    }

    /// Test-only constructor. The criterion-7 RED sizes a small quota
    /// so the test finishes in seconds, not 30 s windows. The cycle-8
    /// pacing test uses `with_period(500 ms).allow_burst(2)`; the
    /// cycle-9 layering test uses `with_period(3 s).allow_burst(2)`.
    pub fn with_quota(base_url: String, quota: Quota) -> Result<Self, SpotifyError> {
        let raw = reqwest::Client::builder()
            .build()
            .map_err(|e| SpotifyError::Transport(e.to_string()))?;
        let limiter = Arc::new(RateLimiter::direct(quota));
        let client = reqwest_middleware::ClientBuilder::new(raw)
            // First .with() → outermost: a retry re-enters everything
            // below it, which is where the layering invariant lives.
            .with(RetryAfterMiddleware::new())
            // Second .with() → innermost: every attempt (initial OR
            // retry) acquires a fresh governor permit before reqwest
            // sees the request.
            .with(GovernorMiddleware { limiter })
            .build();
        Ok(Self { base_url, client })
    }
}

#[async_trait]
impl SpotifyClient for ReqwestSpotifyClient {
    async fn get_json(
        &self,
        path: &str,
        access_token: &str,
    ) -> Result<serde_json::Value, SpotifyError> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .client
            .get(&url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| SpotifyError::Transport(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(SpotifyError::Status(status.as_u16()));
        }
        response
            .json::<serde_json::Value>()
            .await
            .map_err(|e| SpotifyError::Decode(e.to_string()))
    }
}
