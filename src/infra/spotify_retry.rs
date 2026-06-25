//! Custom `reqwest_middleware` Middleware that honors HTTP 429
//! `Retry-After` by sleeping for the indicated seconds and retrying
//! exactly once (criterion 8). Sits OUTERMOST in the cycle-9 chain so
//! its retry re-enters `GovernorMiddleware` and consumes a SECOND
//! governor token (architect cycles 7-8 review caveat #6 — pinned by
//! `tests/spotify_layering.rs`).
//!
//! reqwest-retry 0.7 is NOT used: its `RetryPolicy::should_retry` cannot
//! inspect response headers (cycle-8 adversarial verifier finding), so a
//! `Retry-After`-aware retry has to live in a custom Middleware anyway.
//! Cycle 10's 5xx exponential backoff can either extend this middleware
//! with a status-5xx branch or introduce a sibling middleware between
//! `RetryAfterMiddleware` and `GovernorMiddleware` — both preserve the
//! chain order.

use std::time::Duration;

use async_trait::async_trait;
use http::Extensions;
use reqwest::{header, Request, Response, StatusCode};
use reqwest_middleware::{Middleware, Next};

pub(crate) struct RetryAfterMiddleware {
    cap: Duration,
}

impl RetryAfterMiddleware {
    pub(crate) fn new() -> Self {
        Self {
            cap: Duration::from_secs(60),
        }
    }
}

/// Parse the `Retry-After` header value as RFC 7231 §7.1.3 delta-seconds.
///
/// Returns `None` for missing / non-ASCII / non-integer / `<= 0` values.
/// `secs <= 0` is treated as missing because zero seconds would be the
/// tight-loop case forbidden by spec §5.5 / criterion 8 — the call site
/// falls back to a 1 s default so the retry still fires.
///
/// HTTP-date form (RFC 7231 §7.1.3 form 2) is intentionally not
/// supported — Spotify never emits it.
fn parse_retry_after_seconds(h: Option<&header::HeaderValue>) -> Option<Duration> {
    let v = h?.to_str().ok()?.trim();
    let secs: i64 = v.parse().ok()?;
    if secs <= 0 {
        return None;
    }
    Some(Duration::from_secs(secs as u64))
}

#[async_trait]
impl Middleware for RetryAfterMiddleware {
    async fn handle(
        &self,
        req: Request,
        ext: &mut Extensions,
        next: Next<'_>,
    ) -> reqwest_middleware::Result<Response> {
        // Clone the request before the first send so we can re-issue it
        // on a 429. Spotify GETs have no body so try_clone always succeeds
        // — defensively erroring just in case.
        let cloned = req.try_clone().ok_or_else(|| {
            reqwest_middleware::Error::Middleware(anyhow::anyhow!(
                "spotify request must be cloneable for retry"
            ))
        })?;

        let resp = next.clone().run(req, ext).await?;
        if resp.status() != StatusCode::TOO_MANY_REQUESTS {
            return Ok(resp);
        }

        // Spec §5.5: retry MUST happen. Missing / unparseable / out-of-range
        // Retry-After falls back to 1 s default (NOT surfacing 429
        // immediately — the "fail to cache" clause attaches to the SECOND
        // 429, not the first).
        let wait = parse_retry_after_seconds(resp.headers().get(header::RETRY_AFTER))
            .map(|d| d.min(self.cap))
            .unwrap_or_else(|| Duration::from_secs(1));

        // Drop the 429 response BEFORE sleeping so the body reader doesn't
        // hold a borrow into the connection pool during the wait.
        drop(resp);
        tokio::time::sleep(wait).await;

        next.run(cloned, ext).await
    }
}
