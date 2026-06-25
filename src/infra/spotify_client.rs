//! Concrete reqwest-backed `SpotifyClient`. Every outbound call awaits a
//! `governor` permit before sending, satisfying criterion 7
//! (≤ 30 req / 30 s). The retry / Retry-After / 5xx-backoff middleware
//! stack lands in cycle 9 — cycle 8 ships pacing only, with no retry
//! middleware to keep the criterion-7 RED's arrival-count assertion
//! unambiguous.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter};
use reqwest::Client;

use crate::domain::spotify::{SpotifyClient, SpotifyError};

type DirectRateLimiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

pub struct ReqwestSpotifyClient {
    base_url: String,
    client: Client,
    limiter: Arc<DirectRateLimiter>,
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
    /// (`Quota::with_period(500 ms).allow_burst(2)`) so the test
    /// finishes in ~4 s wall-clock instead of 30 s windows.
    pub fn with_quota(base_url: String, quota: Quota) -> Result<Self, SpotifyError> {
        let client = Client::builder()
            .build()
            .map_err(|e| SpotifyError::Transport(e.to_string()))?;
        let limiter = Arc::new(RateLimiter::direct(quota));
        Ok(Self {
            base_url,
            client,
            limiter,
        })
    }
}

#[async_trait]
impl SpotifyClient for ReqwestSpotifyClient {
    async fn get_json(
        &self,
        path: &str,
        access_token: &str,
    ) -> Result<serde_json::Value, SpotifyError> {
        // Criterion 7: every outbound call passes through the bucket.
        self.limiter.until_ready().await;

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
