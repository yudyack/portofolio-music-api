//! Criterion 7 — every outbound Spotify call passes through a governor
//! token-bucket. The cycle-8 RED pins pacing behavior with a sized-down
//! quota under real wall-clock; the production quota (spec §5.5, 30 req /
//! 30 s) lives in `ReqwestSpotifyClient::new`. Cycle 9 will add the
//! Retry-After / 5xx interaction tests.
//!
//! Why wall-clock and not `tokio::time::pause` + `FakeRelativeClock`:
//! governor 0.7 binds its `Clock` at `RateLimiter` construction
//! (`direct_with_clock`) and its async `until_ready().await` requires a
//! `ReasonablyRealtime` clock — `FakeRelativeClock` only supports the
//! synchronous `check()` family. Wall-clock with a small quota gives a
//! deterministic ~4 s test; FakeRelativeClock would deadlock the 10
//! spawned tasks.
//!
//! Math: `Quota::with_period(500 ms).allow_burst(2)` = 2 cells / s steady
//! state, burst = 2. 10 calls → first 2 fire at t≈0 (burst), then 1 every
//! 500 ms. 10th call fires at t≈4000 ms.
//!  * BOUND A (burst floor): 2nd completion finishes within 500 ms
//!  * BOUND B (pace ceiling): 10th completion takes ≥ 3.5 s
//!  * Wiremock receives exactly 10 requests (no drops, no retries)

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use governor::Quota;
use music_api::domain::spotify::SpotifyClient;
use music_api::infra::spotify_client::ReqwestSpotifyClient;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn governor_paces_outbound_calls_under_a_small_burst_quota() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "yudhyapw"})))
        .mount(&server)
        .await;

    // 2 cells / s, burst = 2 — chosen so the test finishes in ~4 s wall clock
    // and the two bounds (burst, pace) are mathematically achievable.
    let quota = Quota::with_period(Duration::from_millis(500))
        .unwrap()
        .allow_burst(NonZeroU32::new(2).unwrap());
    let client = Arc::new(
        ReqwestSpotifyClient::with_quota(server.uri(), quota)
            .expect("ReqwestSpotifyClient::with_quota should succeed for a valid base URL"),
    );

    let started = Instant::now();
    let mut handles = Vec::with_capacity(10);
    for _ in 0..10 {
        let c = client.clone();
        handles.push(tokio::spawn(async move {
            let result = c.get_json("/v1/me", "fake-bearer").await;
            (started.elapsed(), result.is_ok())
        }));
    }

    let mut completions = Vec::with_capacity(10);
    for h in handles {
        // Watchdog: if governor deadlocks or wiremock stalls, this fails
        // clean with a debuggable message instead of hanging CI.
        let outcome = tokio::time::timeout(Duration::from_secs(15), h)
            .await
            .expect("spotify_pacing test must not hang past 15 s")
            .expect("spawned task must not panic");
        completions.push(outcome);
    }
    completions.sort_by_key(|(elapsed, _)| *elapsed);

    // Every call returned Ok against the wiremock 200.
    for (elapsed, ok) in &completions {
        assert!(
            *ok,
            "criterion 7: every call must succeed against wiremock 200 (elapsed {elapsed:?})",
        );
    }

    // BOUND A — burst floor: the 2nd-completing call lands inside the
    // burst window. A no-governor implementation would also pass this, so
    // this bound is paired with BOUND B below to fence shortcuts.
    assert!(
        completions[1].0 < Duration::from_millis(500),
        "criterion 7 burst floor: 2nd completion must finish within 500 ms (burst=2), got {:?}",
        completions[1].0,
    );

    // BOUND B — pace ceiling: 10th completion takes ≥ 3.5 s. A no-governor
    // implementation finishes all 10 in <100 ms and fails this. A hardcoded
    // `sleep` would have to know the bucket period and burst, at which point
    // the shortcut IS a token bucket.
    assert!(
        completions[9].0 >= Duration::from_millis(3500),
        "criterion 7 pace ceiling: 10th completion must take ≥ 3.5 s under 2/s burst=2, got {:?}",
        completions[9].0,
    );

    // Every call reached wiremock exactly once — no drops, no retries.
    let received = server.received_requests().await.unwrap();
    assert_eq!(
        received.len(),
        10,
        "criterion 7: every call must reach Spotify exactly once",
    );
}
