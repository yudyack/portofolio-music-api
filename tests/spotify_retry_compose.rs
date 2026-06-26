//! QA acceptance — criteria 8 + 9 compose. A single outbound call that
//! meets `500` then `429 (Retry-After)` then `200` must traverse BOTH
//! retry layers in the chain
//!   RetryAfterMiddleware (429) → BackoffMiddleware (5xx) → Governor
//! and ultimately succeed. This guards against a future refactor that
//! handles one status but accidentally drops the other, or mis-orders the
//! layers so a 5xx-then-429 sequence isn't fully retried.
//!
//! Trace: BackoffMiddleware retries the 500 (one ~base backoff), the
//! retry returns 429 which Backoff passes up (429 is not 5xx),
//! RetryAfterMiddleware then honors Retry-After and retries to the 200.
//! Three upstream requests total.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use governor::Quota;
use music_api::domain::spotify::SpotifyClient;
use music_api::infra::spotify_backoff::BackoffConfig;
use music_api::infra::spotify_client::ReqwestSpotifyClient;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn five_hundred_then_429_then_200_traverses_both_retry_layers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(500).set_body_string(""))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "1")
                .set_body_string(""),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "ok"})))
        .expect(1)
        .mount(&server)
        .await;

    // Loose quota (governor never blocks); small backoff base so the 5xx
    // step costs ~100 ms, leaving the ~1 s Retry-After step dominant.
    let loose = Quota::with_period(Duration::from_millis(1))
        .unwrap()
        .allow_burst(NonZeroU32::new(1000).unwrap());
    let backoff = BackoffConfig {
        base: Duration::from_millis(100),
        max_retries: 3,
        jitter_max: Duration::ZERO,
    };
    let client = Arc::new(
        ReqwestSpotifyClient::with_quota_and_backoff(server.uri(), loose, backoff)
            .expect("client builds"),
    );

    let started = Instant::now();
    let v = tokio::time::timeout(Duration::from_secs(20), client.get_json("/v1/me", "tok"))
        .await
        .expect("watchdog")
        .expect("must succeed after 5xx-backoff then 429-Retry-After");
    let elapsed = started.elapsed();

    assert_eq!(v, Some(json!({"id": "ok"})));
    // ~100 ms backoff + ~1 s Retry-After. Lower bound proves BOTH waits
    // happened (a single layer would be < 1 s or < 0.2 s).
    assert!(
        elapsed >= Duration::from_millis(1000),
        "both layers must wait: ~100 ms (5xx) + ~1 s (429). Got {elapsed:?}",
    );
    assert!(
        elapsed <= Duration::from_millis(2500),
        "no extra retries expected. Got {elapsed:?}",
    );

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 3, "500 + 429 + 200 = three upstream requests");
}

/// Spec §5.5: a 403 (insufficient scope) is NOT retried — it is neither a
/// 5xx (BackoffMiddleware) nor a 429 (RetryAfterMiddleware). It must pass
/// straight through as `Status(403)` on the first attempt. Guards the
/// criterion-9 boundary: the backoff fires ONLY on 5xx, never on 4xx.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn four_oh_three_passes_through_without_retry() {
    use music_api::domain::spotify::SpotifyError;

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(403).set_body_string(""))
        .expect(1) // exactly one attempt, no retry
        .mount(&server)
        .await;

    let loose = Quota::with_period(Duration::from_millis(1))
        .unwrap()
        .allow_burst(NonZeroU32::new(1000).unwrap());
    let backoff = BackoffConfig {
        base: Duration::from_millis(100),
        max_retries: 3,
        jitter_max: Duration::ZERO,
    };
    let client = Arc::new(
        ReqwestSpotifyClient::with_quota_and_backoff(server.uri(), loose, backoff)
            .expect("client builds"),
    );

    let started = Instant::now();
    let err = client
        .get_json("/v1/me", "tok")
        .await
        .expect_err("403 must surface, not succeed");
    let elapsed = started.elapsed();

    assert!(matches!(err, SpotifyError::Status(403)), "got {err:?}");
    assert!(
        elapsed < Duration::from_millis(500),
        "403 must NOT incur a backoff/Retry-After wait. Got {elapsed:?}",
    );
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1, "403 is not retried by any layer");
}
