//! Outbound-HTTP logging middleware. Sits inside every
//! `ClientWithMiddleware` we hand out so any code path that uses one of
//! our HTTP clients gets logged at a uniform format — no per-call-site
//! `tracing::info!` to remember. Fires PER ATTEMPT, so 429-retry and
//! 5xx-backoff retries (which previously hid behind the wrapping
//! `get_json` log) each produce their own request+response pair.
//!
//! Logs at info: method, URL, status, response content-length, elapsed
//! milliseconds. Does NOT log headers (carries the bearer / Basic
//! credential) or the body (carries Spotify content / OAuth tokens —
//! redaction context lives at the application layer in
//! `spotify_client.rs` / `token_exchanger.rs`).

use std::time::Instant;

use async_trait::async_trait;
use http::Extensions;
use reqwest::{Request, Response};
use reqwest_middleware::{Middleware, Next};

/// Reusable middleware. Each client constructs one with its preferred
/// tracing target so per-subsystem filters still work. Targets must be
/// one of the closed set known to `tracing_dispatch::*` below — the
/// `tracing::info!` macro requires a literal target, so adding a new
/// client means adding one arm to those helpers.
pub(crate) struct LoggingMiddleware {
    target: WireTarget,
}

/// Closed enum of tracing targets accepted by the logging middleware.
/// Every variant maps to a `"music_api::wire::*"` literal at the macro
/// call site so `RUST_LOG=music_api::wire=debug` / `WIRE_BODIES=1`
/// captures all of them in one stroke.
#[derive(Clone, Copy)]
pub(crate) enum WireTarget {
    SpotifyHttp,
    SpotifyOAuthHttp,
}

impl LoggingMiddleware {
    pub(crate) fn new(target: WireTarget) -> Self {
        Self { target }
    }
}

#[async_trait]
impl Middleware for LoggingMiddleware {
    async fn handle(
        &self,
        req: Request,
        ext: &mut Extensions,
        next: Next<'_>,
    ) -> reqwest_middleware::Result<Response> {
        let method = req.method().clone();
        let url = req.url().clone();
        emit_outbound(self.target, &method, &url);

        let started = Instant::now();
        let outcome = next.run(req, ext).await;
        let elapsed_ms = started.elapsed().as_millis() as u64;

        match &outcome {
            Ok(resp) => emit_response(
                self.target,
                &method,
                &url,
                resp.status().as_u16(),
                resp.content_length(),
                elapsed_ms,
            ),
            Err(e) => emit_transport_error(self.target, &method, &url, e, elapsed_ms),
        }
        outcome
    }
}

// `tracing::info!` / `warn!` require a literal `target:`, so each call
// path expands to one of two literal arms. Adding a new `WireTarget`
// variant means adding one match arm per helper. The fields are
// identical across arms; only the target literal changes.

macro_rules! info_with_target {
    ($target:literal, $($rest:tt)*) => {
        tracing::info!(target: $target, $($rest)*)
    };
}
macro_rules! warn_with_target {
    ($target:literal, $($rest:tt)*) => {
        tracing::warn!(target: $target, $($rest)*)
    };
}

fn emit_outbound(target: WireTarget, method: &reqwest::Method, url: &reqwest::Url) {
    match target {
        WireTarget::SpotifyHttp => info_with_target!(
            "music_api::wire::spotify_http",
            direction = "→",
            method = %method,
            url = %url,
            "outbound http",
        ),
        WireTarget::SpotifyOAuthHttp => info_with_target!(
            "music_api::wire::spotify_oauth_http",
            direction = "→",
            method = %method,
            url = %url,
            "outbound http",
        ),
    }
}

fn emit_response(
    target: WireTarget,
    method: &reqwest::Method,
    url: &reqwest::Url,
    status: u16,
    content_length: Option<u64>,
    elapsed_ms: u64,
) {
    let is_ok = (200..400).contains(&status);
    match (target, is_ok) {
        (WireTarget::SpotifyHttp, true) => info_with_target!(
            "music_api::wire::spotify_http",
            direction = "←",
            method = %method,
            url = %url,
            status = status,
            content_length = content_length,
            elapsed_ms = elapsed_ms,
            "outbound http response",
        ),
        (WireTarget::SpotifyHttp, false) => warn_with_target!(
            "music_api::wire::spotify_http",
            direction = "←",
            method = %method,
            url = %url,
            status = status,
            content_length = content_length,
            elapsed_ms = elapsed_ms,
            "outbound http response",
        ),
        (WireTarget::SpotifyOAuthHttp, true) => info_with_target!(
            "music_api::wire::spotify_oauth_http",
            direction = "←",
            method = %method,
            url = %url,
            status = status,
            content_length = content_length,
            elapsed_ms = elapsed_ms,
            "outbound http response",
        ),
        (WireTarget::SpotifyOAuthHttp, false) => warn_with_target!(
            "music_api::wire::spotify_oauth_http",
            direction = "←",
            method = %method,
            url = %url,
            status = status,
            content_length = content_length,
            elapsed_ms = elapsed_ms,
            "outbound http response",
        ),
    }
}

fn emit_transport_error(
    target: WireTarget,
    method: &reqwest::Method,
    url: &reqwest::Url,
    err: &reqwest_middleware::Error,
    elapsed_ms: u64,
) {
    match target {
        WireTarget::SpotifyHttp => warn_with_target!(
            "music_api::wire::spotify_http",
            direction = "←",
            method = %method,
            url = %url,
            error = %err,
            elapsed_ms = elapsed_ms,
            "outbound http error (transport)",
        ),
        WireTarget::SpotifyOAuthHttp => warn_with_target!(
            "music_api::wire::spotify_oauth_http",
            direction = "←",
            method = %method,
            url = %url,
            error = %err,
            elapsed_ms = elapsed_ms,
            "outbound http error (transport)",
        ),
    }
}
