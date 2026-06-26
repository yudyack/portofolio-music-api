//! Custom `reqwest_middleware` Middleware that retries Spotify 5xx
//! responses with exponential backoff + jitter (criterion 9, spec §5.5):
//! delays 1 s, 2 s, 4 s (+ random 0–250 ms), max 3 retries, then the last
//! 5xx propagates (the cache fallback / `502` mapping land in cycles 11-12).
//!
//! Sits BETWEEN `RetryAfterMiddleware` (429, outermost) and
//! `GovernorMiddleware` (pacing, innermost) in the chain assembled by
//! [`crate::infra::spotify_client::ReqwestSpotifyClient`]. Because it is
//! OUTSIDE the governor, every backoff retry re-enters the limiter and
//! consumes a fresh token — the same layering invariant cycle 9 pinned
//! for 429, generalized to 5xx (`tests/spotify_backoff.rs`).
//!
//! `reqwest-retry` is deliberately NOT used: its `ExponentialBackoff`
//! jitter algorithm cannot reproduce the spec's exact 1 s / 2 s / 4 s +
//! ≤250 ms schedule, and its default classifier also retries 429 — which
//! would double-handle the status `RetryAfterMiddleware` already owns.
//! A hand-rolled middleware keeps both concerns separated and the schedule
//! exactly testable, mirroring the cycle-9 `RetryAfterMiddleware` rationale.

use std::time::Duration;

use async_trait::async_trait;
use http::Extensions;
use rand::Rng;
use reqwest::{Request, Response};
use reqwest_middleware::{Middleware, Next};

/// Tunable backoff schedule. [`BackoffConfig::production`] is the spec
/// §5.5 schedule; tests inject a smaller `base` and zero `jitter_max` so
/// the exponential growth is observable in well under a second and is
/// deterministic.
#[derive(Clone, Copy, Debug)]
pub struct BackoffConfig {
    /// Delay before the first retry. Each subsequent retry doubles it.
    pub base: Duration,
    /// Maximum number of retries (NOT counting the initial attempt).
    pub max_retries: u32,
    /// Upper bound on the random jitter added to each delay.
    pub jitter_max: Duration,
}

impl BackoffConfig {
    /// Spec §5.5 / criterion 9: 1 s, 2 s, 4 s + jitter ≤ 250 ms, max 3
    /// retries.
    pub fn production() -> Self {
        Self {
            base: Duration::from_secs(1),
            max_retries: 3,
            jitter_max: Duration::from_millis(250),
        }
    }
}

/// Compute the backoff delay before retry number `attempt` (0-based):
/// `base * 2^attempt` plus uniform random jitter in `[0, jitter_max]`.
///
/// With production params this yields 1 s / 2 s / 4 s (+ ≤250 ms) for
/// attempts 0 / 1 / 2.
fn backoff_delay(attempt: u32, base: Duration, jitter_max: Duration) -> Duration {
    let exp = base * 2u32.pow(attempt);
    if jitter_max.is_zero() {
        return exp;
    }
    let jitter_ms = rand::thread_rng().gen_range(0..=jitter_max.as_millis() as u64);
    exp + Duration::from_millis(jitter_ms)
}

pub(crate) struct BackoffMiddleware {
    cfg: BackoffConfig,
}

impl BackoffMiddleware {
    pub(crate) fn new(cfg: BackoffConfig) -> Self {
        Self { cfg }
    }
}

#[async_trait]
impl Middleware for BackoffMiddleware {
    async fn handle(
        &self,
        mut req: Request,
        ext: &mut Extensions,
        next: Next<'_>,
    ) -> reqwest_middleware::Result<Response> {
        let mut attempt: u32 = 0;
        loop {
            // Clone before each send so the request can be re-issued on a
            // 5xx. Spotify GETs have no body so try_clone always succeeds —
            // defensively erroring just in case.
            let cloned = req.try_clone().ok_or_else(|| {
                reqwest_middleware::Error::Middleware(anyhow::anyhow!(
                    "spotify request must be cloneable for backoff retry"
                ))
            })?;

            let resp = next.clone().run(req, ext).await?;
            if !resp.status().is_server_error() || attempt >= self.cfg.max_retries {
                // Success, a non-5xx error, or retries exhausted: surface
                // the response as-is (the 5xx then propagates as
                // SpotifyError::Status).
                return Ok(resp);
            }

            // Drop the 5xx response BEFORE sleeping so the body reader
            // doesn't hold a borrow into the connection pool during the wait.
            drop(resp);
            let delay = backoff_delay(attempt, self.cfg.base, self.cfg.jitter_max);
            tokio::time::sleep(delay).await;

            req = cloned;
            attempt += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_backoff_schedule_is_1s_2s_4s_within_jitter() {
        let cfg = BackoffConfig::production();
        // (attempt, lo_ms, hi_ms) — base*2^attempt .. base*2^attempt + 250.
        for (attempt, lo, hi) in [(0u32, 1000u64, 1250u64), (1, 2000, 2250), (2, 4000, 4250)] {
            // Sample repeatedly to exercise the jitter range.
            for _ in 0..64 {
                let d = backoff_delay(attempt, cfg.base, cfg.jitter_max).as_millis() as u64;
                assert!(
                    d >= lo && d <= hi,
                    "criterion 9: attempt {attempt} delay {d} ms out of [{lo}, {hi}] ms",
                );
            }
        }
    }

    #[test]
    fn zero_jitter_is_exact_exponential() {
        let base = Duration::from_millis(100);
        assert_eq!(backoff_delay(0, base, Duration::ZERO), Duration::from_millis(100));
        assert_eq!(backoff_delay(1, base, Duration::ZERO), Duration::from_millis(200));
        assert_eq!(backoff_delay(2, base, Duration::ZERO), Duration::from_millis(400));
    }

    #[test]
    fn jitter_never_exceeds_configured_ceiling() {
        let base = Duration::from_secs(1);
        let jitter_max = Duration::from_millis(250);
        for attempt in 0..3u32 {
            let exp = base * 2u32.pow(attempt);
            for _ in 0..256 {
                let d = backoff_delay(attempt, base, jitter_max);
                assert!(
                    d >= exp && d <= exp + jitter_max,
                    "criterion 9: delay {d:?} outside [{exp:?}, {:?}]",
                    exp + jitter_max,
                );
            }
        }
    }
}
