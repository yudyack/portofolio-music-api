//! Custom `reqwest_middleware` Middleware that gates every outbound
//! request on a `governor` token-bucket permit. Sits INNERMOST in the
//! cycle-9 chain so every retry attempt issued by
//! [`crate::infra::spotify_retry::RetryAfterMiddleware`] re-enters this
//! layer and consumes a fresh permit — satisfying spec §5.5: "a 429
//! retry still consumes a token".
//!
//! Cycle 8 acquired the permit inline inside `get_json`; cycle 9 lifts
//! it into the middleware chain so the layering invariant is enforced
//! by chain composition, not by a hand-rolled call sequence.

use std::sync::Arc;

use async_trait::async_trait;
use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use governor::RateLimiter;
use http::Extensions;
use reqwest::{Request, Response};
use reqwest_middleware::{Middleware, Next};

pub(crate) type DirectRateLimiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

pub(crate) struct GovernorMiddleware {
    pub(crate) limiter: Arc<DirectRateLimiter>,
}

#[async_trait]
impl Middleware for GovernorMiddleware {
    async fn handle(
        &self,
        req: Request,
        ext: &mut Extensions,
        next: Next<'_>,
    ) -> reqwest_middleware::Result<Response> {
        self.limiter.until_ready().await;
        next.run(req, ext).await
    }
}
