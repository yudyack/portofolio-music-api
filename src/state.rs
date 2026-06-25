//! Handler-visible application state. Threaded into every Router branch via
//! `Router::with_state(state)`. Cloning is cheap — every field is `Arc<…>`.
//!
//! Cycle 7 holds a single field. Cycle 8+ will add more (`spotify`, `config`,
//! `rate_limiter`, `refresh_state`, etc.) as the SpotifyClient, governor,
//! and refresher land. The fields are `pub(crate)` so test code constructs
//! via `AppState::new_for_test(repo)` — when cycle 8 adds a required field,
//! tests touch one constructor signature, not every struct-literal site.

use std::sync::Arc;

use crate::domain::tokens::TokenRepository;

#[derive(Clone)]
pub struct AppState {
    pub(crate) tokens: Arc<dyn TokenRepository>,
}

impl AppState {
    /// Production constructor. Used by `init()`.
    pub fn new(tokens: Arc<dyn TokenRepository>) -> Self {
        Self { tokens }
    }

    /// Test-only constructor. Today behaves exactly like `new`; the name
    /// signals to the cycle-8 author that adding required fields should
    /// surface a sensible default here so test fixtures keep compiling.
    pub fn new_for_test(tokens: Arc<dyn TokenRepository>) -> Self {
        Self { tokens }
    }
}
