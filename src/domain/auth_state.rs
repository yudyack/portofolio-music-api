//! Shared, in-memory authorization state for the single owner.
//!
//! Today it carries one bit: whether the stored refresh token has been
//! rejected (`invalid_grant`) and the owner must reauthorize. Cycle 11
//! sets it from the refresh-on-401 path (criterion 10). Criterion 6 will
//! read it from `/healthz` (`status:"needs_reauth"`) and the `/v1/*`
//! handlers (503 `{error:"needs_reauth"}`), and the OAuth callback clears
//! it on a successful re-link.
//!
//! `AtomicBool` (not a `Mutex`) because the access pattern is a single
//! independent flag read by many handlers and flipped rarely.

use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Default)]
pub struct AuthState {
    needs_reauth: AtomicBool,
}

impl AuthState {
    pub fn new() -> Self {
        Self::default()
    }

    /// True once the refresh token has been rejected and only a fresh
    /// owner authorization can recover.
    pub fn needs_reauth(&self) -> bool {
        self.needs_reauth.load(Ordering::SeqCst)
    }

    /// Flip into the reauthorization-required state. Idempotent.
    pub fn set_needs_reauth(&self) {
        self.needs_reauth.store(true, Ordering::SeqCst);
    }

    /// Clear the flag after a successful re-link. Used by the OAuth
    /// callback cycle.
    pub fn clear(&self) {
        self.needs_reauth.store(false, Ordering::SeqCst);
    }
}
