//! Tripwire for the SPEC-LITERAL variant of criterion 8's timing: the
//! ±100 ms tolerance "via mocked clock". The cycle-9 coder applied the
//! cycle-8 verifier's pre-documented fallback (real wall-clock with
//! ±300 ms tolerance) because `tokio::time::pause` + `start_paused` +
//! wiremock 0.6's TcpListener accept loop don't compose — auto-advance
//! doesn't fire and the test hangs.
//!
//! This tripwire stages the spec-literal test as `#[ignore]`'d. A future
//! cycle that fixes the interaction (newer wiremock, newer tokio, or a
//! different mocking shape) unignores it; the assertions then enforce
//! the spec's ±100 ms tolerance against virtual time. Forgetting to
//! unignore after a wiremock upgrade is the failure mode this guards
//! against — same recipe as the existing `auth_constant_time.rs` and
//! `healthz_degradation_tripwire.rs` tripwires.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use governor::Quota;
use music_api::domain::spotify::SpotifyClient;
use music_api::infra::spotify_client::ReqwestSpotifyClient;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "current_thread", start_paused = true)]
#[ignore = "Tripwire: paused-clock variant for spec's ±100 ms tolerance. Cycle 9 hit wiremock-auto-advance interaction failure. Unignore when a newer wiremock/tokio combo fixes it; the assertions pin the spec literal."]
async fn spotify_retry_after_under_paused_clock_with_spec_literal_tolerance() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "2")
                .set_body_string(""),
        )
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

    let loose_quota = Quota::with_period(Duration::from_millis(1))
        .unwrap()
        .allow_burst(NonZeroU32::new(1000).unwrap());
    let client = Arc::new(
        ReqwestSpotifyClient::with_quota(server.uri(), loose_quota)
            .expect("with_quota should succeed"),
    );

    let virtual_started = tokio::time::Instant::now();
    let real_started = std::time::Instant::now();

    let result = tokio::time::timeout(
        Duration::from_secs(20),
        client.get_json("/v1/me", "fake-bearer"),
    )
    .await
    .expect("watchdog: get_json must not exceed 20 s")
    .expect("get_json must succeed (429 → Retry-After → retry → 200)");

    let virtual_elapsed = virtual_started.elapsed();
    let real_elapsed = real_started.elapsed();

    assert_eq!(result["id"], "yudhyapw");

    // Spec-literal ±100 ms tolerance against virtual time.
    assert!(
        virtual_elapsed >= Duration::from_millis(1900)
            && virtual_elapsed <= Duration::from_millis(2100),
        "criterion 8 spec-literal tolerance: virtual elapsed must be ∈ [1.9 s, 2.1 s] \
         for Retry-After=2 s. Got {:?}.",
        virtual_elapsed,
    );

    // Virtual-not-wall-clock check: a std::thread::sleep regression in
    // the middleware fails this bound under paused clock.
    assert!(
        real_elapsed < Duration::from_millis(500),
        "wall-clock budget under paused clock: must complete in <500 ms real time. \
         Got {:?}. A std::thread::sleep regression fails this.",
        real_elapsed,
    );

    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 2);
}
