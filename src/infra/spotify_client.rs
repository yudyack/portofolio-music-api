//! Concrete reqwest-backed `SpotifyClient`. Outbound requests pass
//! through a three-layer middleware chain assembled at construction:
//!
//!   RetryAfterMiddleware (429, outermost)
//!     → BackoffMiddleware (5xx)
//!       → GovernorMiddleware (pacing, innermost)
//!         → reqwest::Client
//!
//! Cycle 8 acquired the governor permit inline inside `get_json`;
//! cycle 9 lifted both the permit and the 429 retry into middleware so
//! the spec §5.5 invariant — "a 429 retry still consumes a token" —
//! is enforced by chain composition. Cycle 10 adds the 5xx exponential
//! backoff (criterion 9) as a sibling middleware BETWEEN the 429 retry
//! and the governor, so a 5xx retry likewise re-enters the limiter.
//!
//! `tests/spotify_layering.rs` pins the 429 layering invariant;
//! `tests/spotify_retry_after.rs` pins the Retry-After timing;
//! `tests/spotify_backoff.rs` pins the 5xx schedule + its layering
//! generalization. The exact production 1 s / 2 s / 4 s backoff schedule
//! is pinned by unit tests in `crate::infra::spotify_backoff`.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use governor::{Quota, RateLimiter};
use reqwest_middleware::ClientWithMiddleware;

use crate::domain::spotify::{SpotifyClient, SpotifyError};
use crate::infra::http_logging::{LoggingMiddleware, WireTarget};
use crate::infra::spotify_backoff::{BackoffConfig, BackoffMiddleware};
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
        let quota = Quota::with_period(Duration::from_millis(1500))
            .expect("1500 ms is non-zero")
            .allow_burst(NonZeroU32::new(20).expect("20 is non-zero"));
        Self::with_quota(base_url, quota)
    }

    /// Test seam over a custom quota with the production 5xx backoff
    /// schedule. The criterion-7 RED sizes a small quota so the test
    /// finishes in seconds, not 30 s windows. The cycle-8 pacing test uses
    /// `with_period(500 ms).allow_burst(2)`; the cycle-9 layering test uses
    /// `with_period(3 s).allow_burst(2)`.
    pub fn with_quota(base_url: String, quota: Quota) -> Result<Self, SpotifyError> {
        Self::with_quota_and_backoff(base_url, quota, BackoffConfig::production())
    }

    /// Full test seam: custom quota AND backoff schedule. The criterion-9
    /// backoff tests inject a small `base` with jitter disabled so the
    /// exponential schedule is observable in well under a second.
    pub fn with_quota_and_backoff(
        base_url: String,
        quota: Quota,
        backoff: BackoffConfig,
    ) -> Result<Self, SpotifyError> {
        let raw = reqwest::Client::builder()
            .build()
            .map_err(|e| SpotifyError::Transport(e.to_string()))?;
        let limiter = Arc::new(RateLimiter::direct(quota));
        let client = reqwest_middleware::ClientBuilder::new(raw)
            // First .with() → outermost: a 429 retry re-enters everything
            // below it (criterion 8 layering invariant).
            .with(RetryAfterMiddleware::new())
            // Second .with() → 5xx exponential backoff. Outside the
            // governor so each backoff retry consumes a fresh token
            // (criterion 9 layering invariant).
            .with(BackoffMiddleware::new(backoff))
            // Third .with() → per-attempt logging. Inside the retry
            // middlewares above so 429/5xx retries each produce their own
            // outbound+response log pair (closes architect F3). Outside
            // the governor so the log fires just before the actual send.
            .with(LoggingMiddleware::new(WireTarget::SpotifyHttp))
            // Fourth .with() → innermost: every attempt (initial OR any
            // retry) acquires a fresh governor permit before reqwest sees
            // the request.
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
    ) -> Result<Option<serde_json::Value>, SpotifyError> {
        let url = format!("{}{}", self.base_url, path);
        // Outbound request log is emitted by LoggingMiddleware (per
        // attempt, including retries). Application-level logs below
        // carry the body — middleware never reads or logs it.
        let response = self
            .client
            .get(&url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| SpotifyError::Transport(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            // Error envelopes (`{"error":{"status":..,"message":..}}`) are
            // load-bearing for diagnosing 401/403/429 but uncapped foreign
            // bytes off an edge CDN are not — cap the prefix at 256 chars.
            let body = response.text().await.unwrap_or_default();
            let body_preview: String = body.chars().take(256).collect();
            tracing::warn!(
                target: "music_api::wire::spotify",
                direction = "←",
                url = %url,
                status = status.as_u16(),
                bytes = body.len(),
                body_preview = %body_preview,
                "spotify response (error)",
            );
            return Err(SpotifyError::Status(status.as_u16()));
        }
        // 204 No Content — Spotify's "nothing playing" signal for
        // /me/player. Distinct from 200 with a JSON `null` body. The
        // status itself is logged by LoggingMiddleware; no body to
        // emit at the app layer.
        if status.as_u16() == 204 {
            return Ok(None);
        }
        let body = response
            .json::<serde_json::Value>()
            .await
            .map_err(|e| SpotifyError::Decode(e.to_string()))?;
        // Body at debug — opt-in via RUST_LOG=music_api::wire=debug. The
        // default `info` deploy must not stream Spotify response bodies
        // (PII + ai-spotify.md "do not cache Spotify content beyond what
        // is needed for immediate use").
        tracing::debug!(
            target: "music_api::wire::spotify",
            direction = "←",
            url = %url,
            status = status.as_u16(),
            body = %body,
            "spotify response",
        );
        Ok(Some(body))
    }
}
