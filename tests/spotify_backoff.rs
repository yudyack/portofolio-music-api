//! Criterion 9 — on Spotify 5xx, retry with exponential backoff
//! (1 s, 2 s, 4 s + jitter ≤ 250 ms), max 3 retries, then surface the
//! upstream status (the cache/`502` mapping lands in cycles 11-12).
//!
//! The 5xx backoff is a sibling middleware between
//! `RetryAfterMiddleware` (429, outermost) and `GovernorMiddleware`
//! (pacing, innermost). Sitting OUTSIDE the governor means every 5xx
//! retry re-enters the limiter and consumes a fresh token — the same
//! layering invariant cycle 9 pinned for 429
//! (`tests/spotify_layering.rs`), generalized here to 5xx per the
//! cycle-9 QA report's cycle-10 must-do.
//!
//! These integration tests drive a SMALL backoff base via the
//! `with_quota_and_backoff` test seam with jitter disabled, so the
//! exponential schedule is observable in well under a second and the
//! per-retry growth is deterministic. The PRODUCTION 1 s / 2 s / 4 s
//! schedule is pinned deterministically by the unit tests inside
//! `src/infra/spotify_backoff.rs` (no 7 s wall-clock wait needed).

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use governor::Quota;
use music_api::domain::spotify::{SpotifyClient, SpotifyError};
use music_api::infra::spotify_backoff::BackoffConfig;
use music_api::infra::spotify_client::ReqwestSpotifyClient;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A loose quota so the governor never blocks during the backoff-timing
/// tests; pacing/layering is isolated in its own test below.
fn loose_quota() -> Quota {
    Quota::with_period(Duration::from_millis(1))
        .unwrap()
        .allow_burst(NonZeroU32::new(1000).unwrap())
}

/// Test backoff: 100 ms base, no jitter, 3 retries → deterministic
/// delays of 100 ms, 200 ms, 400 ms.
fn fast_backoff() -> BackoffConfig {
    BackoffConfig {
        base: Duration::from_millis(100),
        max_retries: 3,
        jitter_max: Duration::ZERO,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spotify_5xx_then_200_retries_and_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(503).set_body_string(""))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "yudhyapw"})))
        .expect(1)
        .mount(&server)
        .await;

    let client = Arc::new(
        ReqwestSpotifyClient::with_quota_and_backoff(server.uri(), loose_quota(), fast_backoff())
            .expect("with_quota_and_backoff should succeed for a valid base URL"),
    );

    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        client.get_json("/v1/me", "fake-bearer"),
    )
    .await
    .expect("watchdog: get_json must not exceed 20 s")
    .expect("get_json must succeed (503 → backoff → retry → 200)")
    .expect("/v1/me returns a body, not 204");
    let elapsed = started.elapsed();

    assert_eq!(
        result["id"], "yudhyapw",
        "retry must hit the 200 arm and decode the body",
    );
    // One ~100 ms backoff before the single retry. Lower bound discriminates
    // from a no-backoff immediate retry (~0 ms); upper bound from a runaway.
    assert!(
        elapsed >= Duration::from_millis(80),
        "criterion 9: a 5xx must trigger a backoff wait (~100 ms base). Got {elapsed:?}.",
    );
    assert!(
        elapsed <= Duration::from_millis(700),
        "criterion 9: a single 5xx retry should cost ~one base delay. Got {elapsed:?}.",
    );

    let received = server.received_requests().await.unwrap();
    assert_eq!(
        received.len(),
        2,
        "exactly 2 outbound requests: initial 503 + one retry. Got {}.",
        received.len(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spotify_persistent_5xx_exhausts_three_retries_then_surfaces_status() {
    let server = MockServer::start().await;
    // Always 500 — the backoff must give up after exactly 3 retries.
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(500).set_body_string(""))
        .expect(4)
        .mount(&server)
        .await;

    let client = Arc::new(
        ReqwestSpotifyClient::with_quota_and_backoff(server.uri(), loose_quota(), fast_backoff())
            .expect("with_quota_and_backoff should succeed for a valid base URL"),
    );

    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        client.get_json("/v1/me", "fake-bearer"),
    )
    .await
    .expect("watchdog: get_json must not exceed 20 s");
    let elapsed = started.elapsed();

    match result {
        Err(SpotifyError::Status(code)) => assert_eq!(
            code, 500,
            "exhausted 5xx backoff must surface the last upstream status",
        ),
        other => panic!(
            "expected Err(SpotifyError::Status(500)) after exhausting retries, got {other:?}"
        ),
    }

    // Deterministic exponential schedule (jitter disabled): 100 + 200 + 400
    // = 700 ms of cumulative backoff. The lower bound 650 ms discriminates
    // exponential from any CONSTANT backoff: constant-100 → 300 ms,
    // constant-200 → 600 ms — both below the floor. Upper bound guards a
    // 4th (forbidden) retry.
    assert!(
        elapsed >= Duration::from_millis(650),
        "criterion 9: three exponential backoffs (100+200+400 ms) must sum to \
         ~700 ms. Got {elapsed:?} — schedule is not exponential, or fewer than \
         3 retries fired.",
    );
    assert!(
        elapsed <= Duration::from_millis(1500),
        "criterion 9: max 3 retries — got {elapsed:?}, suggesting a 4th backoff \
         (~800 ms more) or a runaway loop.",
    );

    let received = server.received_requests().await.unwrap();
    assert_eq!(
        received.len(),
        4,
        "criterion 9: exactly 4 outbound requests (1 initial + 3 retries), then \
         give up. Got {} — wrong retry count.",
        received.len(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spotify_5xx_retry_consumes_a_second_governor_token() {
    // Generalizes the cycle-9 governor-INSIDE-retry layering invariant
    // (tests/spotify_layering.rs) to 5xx: a backoff retry must re-enter the
    // governor and consume a fresh token. Same discrimination shape.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(502).set_body_string(""))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "call1"})))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "call2"})))
        .expect(1)
        .mount(&server)
        .await;

    // Tiny backoff base so call 1's retry is governed by the 3 s governor
    // refill, not the backoff sleep. Quota: 2-token burst, 3 s period.
    let quota = Quota::with_period(Duration::from_secs(3))
        .unwrap()
        .allow_burst(NonZeroU32::new(2).unwrap());
    let backoff = BackoffConfig {
        base: Duration::from_millis(50),
        max_retries: 3,
        jitter_max: Duration::ZERO,
    };
    let client = Arc::new(
        ReqwestSpotifyClient::with_quota_and_backoff(server.uri(), quota, backoff)
            .expect("with_quota_and_backoff should succeed"),
    );

    let t0 = Instant::now();

    let r1 = tokio::time::timeout(Duration::from_secs(20), client.get_json("/v1/me", "tok"))
        .await
        .expect("call 1 watchdog")
        .expect("call 1 must succeed (502 → backoff → retry → 200)")
        .expect("body present");
    let t_after_call1 = t0.elapsed();
    assert_eq!(r1["id"], "call1");

    let r2 = tokio::time::timeout(Duration::from_secs(20), client.get_json("/v1/me", "tok"))
        .await
        .expect("call 2 watchdog")
        .expect("call 2 must succeed")
        .expect("body present");
    let t_after_call2 = t0.elapsed();
    assert_eq!(r2["id"], "call2");

    let call2_alone = t_after_call2 - t_after_call1;
    assert!(
        call2_alone >= Duration::from_millis(1500),
        "criterion 9 layering: a 5xx retry MUST consume a second governor token. \
         With period=3s burst=2, the bucket should be empty after call 1's retry; \
         call 2 must wait ~2-3 s for refill. Got call2_alone = {call2_alone:?} — \
         backoff middleware is INSIDE the governor, or the retry skipped chain \
         re-entry.",
    );
    assert!(
        call2_alone <= Duration::from_millis(4500),
        "criterion 9 ceiling: call 2 should arrive within ~one refill window. \
         Got call2_alone = {call2_alone:?}.",
    );

    let received = server.received_requests().await.unwrap();
    assert_eq!(
        received.len(),
        3,
        "exactly 3 requests: call-1 (502), call-1-retry (200), call-2 (200). Got {}.",
        received.len(),
    );
}
