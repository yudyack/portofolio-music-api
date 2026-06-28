//! Activity gating for the scheduler-push data plane.
//!
//! The schedulers (see `app::scheduler`) tick the upstream Spotify endpoints
//! at the intervals configured in `config::SchedulerConfig`. To avoid
//! burning Spotify quota when nobody is looking at the page, each scheduler
//! parks on `woke.notified()` while the tracker reports the service as
//! idle. A `/v1/*` request goes through axum middleware that calls
//! `touch()`; if that touch crosses the idle threshold from below, the
//! tracker wakes every parked scheduler so the snapshot starts catching up
//! immediately rather than waiting for the next tick.
//!
//! `last_seen_ms` is `Relaxed` — it is a hint, not a synchronization
//! primitive. The wake-up signal travels through `tokio::sync::Notify`,
//! which has its own ordering guarantees.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::Notify;

/// Tracks the most recent `/v1/*` request and exposes a `Notify` the
/// schedulers can park on while idle.
///
/// `idle_threshold` is the gap (in milliseconds) after which the service is
/// considered idle. A `touch()` whose gap-since-previous exceeds the
/// threshold counts as "crossing the idle line from below" and wakes
/// waiters via `notify_waiters()`. A touch within the active window does
/// nothing besides updating `last_seen_ms`.
pub struct ActivityTracker {
    last_seen_ms: AtomicU64,
    /// All parked schedulers wait on this. Reset to a fresh permit at
    /// every wake — `Notify` is single-permit, but `notify_waiters` flushes
    /// every waiter without consuming a permit.
    pub woke: Notify,
    idle_threshold_ms: u64,
}

impl ActivityTracker {
    /// Construct a tracker with the given idle threshold. The tracker
    /// starts in the "idle" state — `last_seen_ms = 0` — so a freshly
    /// booted server's schedulers park immediately until the first visitor
    /// arrives.
    pub fn new(idle_threshold: Duration) -> Self {
        Self {
            last_seen_ms: AtomicU64::new(0),
            woke: Notify::new(),
            idle_threshold_ms: idle_threshold.as_millis().min(u128::from(u64::MAX)) as u64,
        }
    }

    /// Record a visitor touch. If the gap since the previous touch
    /// exceeds `idle_threshold`, wake every parked scheduler. Returns the
    /// gap in milliseconds for tests.
    pub fn touch(&self) -> u64 {
        let now_ms = now_ms();
        let prev = self.last_seen_ms.swap(now_ms, Ordering::Relaxed);
        let gap = now_ms.saturating_sub(prev);
        if gap > self.idle_threshold_ms {
            // Either we were genuinely idle, or this is the cold boot
            // (`prev == 0`). Either way, schedulers parked on `woke` need
            // to start ticking — flush them all.
            self.woke.notify_waiters();
        }
        gap
    }

    /// True if a touch happened within `idle_threshold` of now. A tracker
    /// that was never touched (`last_seen_ms == 0`) reports false even on
    /// a freshly booted clock — the bootstrap convention is "schedulers
    /// stay parked until the first request lands".
    pub fn is_active(&self) -> bool {
        let last = self.last_seen_ms.load(Ordering::Relaxed);
        if last == 0 {
            return false;
        }
        now_ms().saturating_sub(last) <= self.idle_threshold_ms
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64)
        // Pre-1970 clock skew is silly enough that "treat as t=0" is fine.
        .unwrap_or(0)
}
