//! Criterion 8 + architect cycles 7-8 review caveat #6 — the
//! governor-INSIDE-retry layering invariant: a 429 retry MUST consume a
//! SECOND governor token, per spec §5.5. Without this pin, a future
//! refactor could swap the middleware order and only fail in production
//! (the cycle-8 pacing test would still pass because no retry happens).
//!
//! Test shape (verifier-amended): wall-clock with quota
//! `with_period(3 s).allow_burst(2)`. Bucket starts full with 2 tokens.
//! Sequence:
//!   call 1: first attempt → 429 (consumes token #1, bucket=1)
//!           sleep Retry-After (1 s)
//!           retry → 200 (consumes token #2 IFF layering correct, bucket=0)
//!   call 2: first attempt → blocks until next refill at t≈3 s if layering
//!           correct; otherwise consumes the still-available 2nd token
//!           immediately.
//!
//! Discrimination: `call2_alone in [1.5 s, 4.5 s]`. WRONG layering →
//! ~0 ms (fails 1.5 s floor). CORRECT layering → ~2 s (call 1 took ~1 s
//! for Retry-After sleep; next refill at t≈3 s; call2_alone ≈ 2 s).
//! 1.5 s lower margin is 15× the 100 ms threshold the cycle-9 verifier
//! flagged as flake-prone.
//!
//! The cycle-8 verifier landmines accounted for:
//!  - governor 0.7's `until_ready().await` is wall-clock (cannot be
//!    paused), so this test runs real time. The Retry-After timing test
//!    uses paused clock separately.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use governor::Quota;
use music_api::domain::spotify::SpotifyClient;
use music_api::infra::spotify_client::ReqwestSpotifyClient;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spotify_429_retry_consumes_a_second_governor_token() {
    let server = MockServer::start().await;
    // Arms mounted in declaration order at default priority. wiremock
    // consumes them FIFO: arm A (429 with Retry-After) one-shot, then
    // arm B (200 first reply) one-shot, then arm C (200 second reply).
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

    let quota = Quota::with_period(Duration::from_secs(3))
        .unwrap()
        .allow_burst(NonZeroU32::new(2).unwrap());
    let client = Arc::new(
        ReqwestSpotifyClient::with_quota(server.uri(), quota)
            .expect("ReqwestSpotifyClient::with_quota should succeed for a valid base URL"),
    );

    let t0 = Instant::now();

    // Call 1: 429 → retry → 200. If layering correct, consumes 2 governor
    // tokens. Watchdog at 20 s so a regression fails clean instead of
    // hanging CI.
    let r1 = tokio::time::timeout(Duration::from_secs(20), client.get_json("/v1/me", "tok"))
        .await
        .expect("call 1 watchdog: must not exceed 20 s")
        .expect("call 1 must succeed (429 → Retry-After → retry → 200)")
        .expect("body present");
    let t_after_call1 = t0.elapsed();
    assert_eq!(r1["id"], "call1", "call 1 must return the second-arm body");

    // Call 2: if layering correct, bucket is empty after call 1's retry;
    // must wait until next refill at t≈3 s (so call2_alone ≈ 2 s).
    let r2 = tokio::time::timeout(Duration::from_secs(20), client.get_json("/v1/me", "tok"))
        .await
        .expect("call 2 watchdog: must not exceed 20 s")
        .expect("call 2 must succeed against the third arm")
        .expect("body present");
    let t_after_call2 = t0.elapsed();
    assert_eq!(r2["id"], "call2", "call 2 must return the third-arm body");

    let call2_alone = t_after_call2 - t_after_call1;

    assert!(
        call2_alone >= Duration::from_millis(1500),
        "criterion 8 layering invariant: a 429 retry MUST consume a second \
         governor token. With period=3s burst=2, the bucket should be empty \
         after call 1's retry; call 2 must wait ~2 s for next refill. \
         Got call2_alone = {:?}. Failure modes: (a) governor middleware \
         OUTSIDE retry, (b) retry skipping chain re-entry, (c) governor not \
         registered.",
        call2_alone,
    );
    assert!(
        call2_alone <= Duration::from_millis(4500),
        "criterion 8 ceiling: call 2 should arrive within ~2 s of refill at \
         t≈3 s. Got call2_alone = {:?} — runaway sleep or duplicate retry?",
        call2_alone,
    );

    let received = server.received_requests().await.unwrap();
    assert_eq!(
        received.len(),
        3,
        "exactly 3 outbound requests: call-1-attempt-1 (429), call-1-retry (200), call-2-attempt-1 (200). Got {} — no extra retries, no drops.",
        received.len(),
    );
}
