//! Unit tests for `ActivityTracker`.
//!
//! Validates the contract the schedulers and the `/v1/*` middleware
//! depend on:
//! - Bootstrap convention: a freshly-constructed tracker reports idle
//!   (`is_active() == false`).
//! - `touch()` flips to active.
//! - Walking `tokio::time` past `idle_threshold` flips back to idle.
//! - A `touch()` whose gap exceeds `idle_threshold` wakes parked waiters.
//! - A `touch()` within the active window does NOT wake parked waiters.

use std::sync::Arc;
use std::time::Duration;

use music_api::app::activity::ActivityTracker;
use tokio::time::timeout;

#[tokio::test]
async fn fresh_tracker_reports_idle() {
    let t = ActivityTracker::new(Duration::from_secs(60));
    assert!(
        !t.is_active(),
        "bootstrap convention: never-touched tracker is idle",
    );
}

#[tokio::test]
async fn touch_flips_to_active() {
    let t = ActivityTracker::new(Duration::from_secs(60));
    t.touch();
    assert!(
        t.is_active(),
        "touch within idle_threshold makes tracker active"
    );
}

#[tokio::test]
async fn idle_threshold_elapsed_without_touch_reports_idle() {
    // `is_active` is keyed off wall time (`SystemTime`) — it isn't
    // controllable by tokio's paused clock. So we use a small real
    // threshold and a small real sleep. ~75 ms keeps the test cheap.
    let t = ActivityTracker::new(Duration::from_millis(30));
    t.touch();
    assert!(t.is_active());
    tokio::time::sleep(Duration::from_millis(75)).await;
    assert!(
        !t.is_active(),
        "after idle_threshold elapses without touch, tracker reports idle",
    );
}

#[tokio::test]
async fn touch_crossing_idle_threshold_wakes_parked_waiter() {
    // Short threshold so the test stays fast — the activity flag is keyed
    // off SystemTime, which we can't freeze under tokio's paused clock.
    let tracker = Arc::new(ActivityTracker::new(Duration::from_millis(50)));

    // Park a waiter on `woke` BEFORE the threshold-crossing touch arrives.
    // Register-before-check pattern: the future is created (and the waiter
    // registered with Notify) before we await.
    let parked = {
        let tracker = tracker.clone();
        tokio::spawn(async move {
            tracker.woke.notified().await;
        })
    };

    // Give the parked task a moment to actually register on `woke`.
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    // First touch: previous = 0, gap = now - 0 = huge -> notifies. Cold-
    // boot wake-up case.
    tracker.touch();

    // The parked waiter must complete promptly.
    let woke = timeout(Duration::from_millis(200), parked).await;
    assert!(
        woke.is_ok(),
        "parked waiter must wake after threshold-crossing touch"
    );
}

#[tokio::test]
async fn touch_within_active_window_does_not_wake_parked_waiter() {
    let tracker = Arc::new(ActivityTracker::new(Duration::from_millis(500)));

    // Bootstrap touch to leave the tracker in the "active" window. The
    // bootstrap touch DOES notify (gap = now since 0), so we burn that
    // wake-up by waiting before parking the test waiter.
    tracker.touch();
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Now park a waiter. The next touch should be within the active window
    // (gap < 500 ms), so it must NOT wake the waiter.
    let parked = {
        let tracker = tracker.clone();
        tokio::spawn(async move {
            tracker.woke.notified().await;
        })
    };
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    tracker.touch();

    // The parked waiter MUST still be pending — give it a small slack to
    // ensure no spurious wake-up.
    let result = timeout(Duration::from_millis(50), parked).await;
    assert!(
        result.is_err(),
        "active-window touch must NOT notify; waiter should remain parked",
    );
}
