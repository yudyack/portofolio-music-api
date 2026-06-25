//! Cycle-9 QA add: criterion 8's "If still 429, fails to cache" half.
//!
//! Spec §5.5 / criterion 8: after the Retry-After-driven retry, if the
//! second response is also 429, the service must NOT retry again and
//! must surface the 429. The cycle-9 coder implemented the behavior
//! (RetryAfterMiddleware retries exactly once, then any response from
//! the retry — including another 429 — flows up to ReqwestSpotifyClient
//! which returns SpotifyError::Status(429)) but did not write a test
//! for it. The cache layer ("serve last-good if available") lands in
//! cycle 11+ — this test pins only the propagation surface, not the
//! cache fallback.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use governor::Quota;
use music_api::domain::spotify::{SpotifyClient, SpotifyError};
use music_api::infra::spotify_client::ReqwestSpotifyClient;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spotify_second_429_surfaces_as_status_error_no_third_attempt() {
    let server = MockServer::start().await;
    // Arm A: first attempt → 429 with short Retry-After.
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
    // Arm B: retry → 429 again. RetryAfterMiddleware should NOT retry
    // a third time; this 429 must propagate.
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "1")
                .set_body_string(""),
        )
        .expect(1)
        .mount(&server)
        .await;

    // Loose quota so governor never blocks.
    let loose_quota = Quota::with_period(Duration::from_millis(1))
        .unwrap()
        .allow_burst(NonZeroU32::new(1000).unwrap());
    let client = Arc::new(
        ReqwestSpotifyClient::with_quota(server.uri(), loose_quota)
            .expect("with_quota should succeed for a valid base URL"),
    );

    let result = tokio::time::timeout(
        Duration::from_secs(20),
        client.get_json("/v1/me", "fake-bearer"),
    )
    .await
    .expect("watchdog: call must not exceed 20 s");

    match result {
        Err(SpotifyError::Status(429)) => {} // expected
        Err(other) => panic!(
            "criterion 8: second 429 must surface as SpotifyError::Status(429), got {other:?}"
        ),
        Ok(value) => panic!(
            "criterion 8: second 429 must NOT decode as success, got Ok({value:?}) — \
             middleware retried more than once or arm ordering is wrong"
        ),
    }

    let received = server.received_requests().await.unwrap();
    assert_eq!(
        received.len(),
        2,
        "criterion 8 retry-once invariant: exactly 2 outbound requests (initial + 1 retry). \
         A 3rd request means the middleware retried more than once. Got {}.",
        received.len(),
    );
}
