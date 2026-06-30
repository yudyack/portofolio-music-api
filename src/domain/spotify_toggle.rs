//! Owner-controlled kill switch for outbound Spotify traffic.
//!
//! Flipping `disable()` parks every scheduler tick (loop sleeps + continues
//! instead of fetching) and forces `/v1/*` handlers to serve only what's
//! already in the snapshot cell — no cold-start fetch, no further Spotify
//! calls of any kind. Re-enable resumes within one scheduler interval.
//!
//! In-memory and resets to enabled on restart by design: the toggle is a
//! runtime escape hatch (e.g., when Spotify is rate-limiting hard and the
//! owner wants to back off), not a persistent operating mode. If the
//! operator wants the process to come up paused, restart-time control
//! belongs in env-var land; this type stays state-free across restarts.
//!
//! `AtomicBool` for the same reason as [`super::auth_state::AuthState`]:
//! one independent flag, read by many tasks, flipped rarely.

use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
pub struct SpotifyToggle {
    enabled: AtomicBool,
}

impl Default for SpotifyToggle {
    fn default() -> Self {
        Self {
            enabled: AtomicBool::new(true),
        }
    }
}

impl SpotifyToggle {
    pub fn new() -> Self {
        Self::default()
    }

    /// True when outbound Spotify traffic is allowed. Schedulers and the
    /// `/v1/*` cold-start fetch path read this on every iteration.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// Allow outbound Spotify traffic. Idempotent.
    pub fn enable(&self) {
        self.enabled.store(true, Ordering::SeqCst);
    }

    /// Stop all outbound Spotify traffic. Idempotent.
    pub fn disable(&self) {
        self.enabled.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_enabled() {
        assert!(SpotifyToggle::new().is_enabled());
    }

    #[test]
    fn disable_then_enable_round_trips() {
        let t = SpotifyToggle::new();
        t.disable();
        assert!(!t.is_enabled());
        t.enable();
        assert!(t.is_enabled());
    }

    #[test]
    fn disable_is_idempotent() {
        let t = SpotifyToggle::new();
        t.disable();
        t.disable();
        assert!(!t.is_enabled());
    }
}
