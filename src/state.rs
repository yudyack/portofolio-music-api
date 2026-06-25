//! Handler-visible application state. Threaded into every Router branch via
//! `Router::with_state(state)`. Cloning is cheap — every field is `Arc<…>`.
//!
//! Cycle 7 introduced `tokens`. Cycle 8 adds `spotify`. The forcing
//! function works as designed: every test fixture that constructed
//! `AppState::new_for_test(repo)` grows ONE argument (the spotify stub),
//! not every struct-literal site. Future cycles will continue the
//! pattern as `config`, `rate_limiter`, `refresh_state`, etc. land.

use std::sync::Arc;

use crate::domain::spotify::SpotifyClient;
use crate::domain::tokens::TokenRepository;

#[derive(Clone)]
pub struct AppState {
    pub(crate) tokens: Arc<dyn TokenRepository>,
    // First reader lands in cycle 10/11 when a `/v1/*` handler invokes
    // SpotifyClient. The field is wired through `init()` and exercised by
    // tests/spotify_pacing.rs against the concrete impl; the `#[allow]`
    // is removed in the cycle that adds the handler.
    #[allow(dead_code)]
    pub(crate) spotify: Arc<dyn SpotifyClient>,
}

impl AppState {
    /// Production constructor. Used by `init()`.
    pub fn new(tokens: Arc<dyn TokenRepository>, spotify: Arc<dyn SpotifyClient>) -> Self {
        Self { tokens, spotify }
    }

    /// Test-only constructor. The cycle 8 grew the second required
    /// field — see this constructor for the seam. The next cycle that
    /// adds a required field should mirror the same pattern.
    pub fn new_for_test(
        tokens: Arc<dyn TokenRepository>,
        spotify: Arc<dyn SpotifyClient>,
    ) -> Self {
        Self { tokens, spotify }
    }
}
