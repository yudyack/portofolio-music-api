//! Handler-visible application state. Cloned into every Router branch.
//! Every field is `Arc<…>` so cloning is cheap.

use std::sync::Arc;

use crate::app::state_store::StateStore;
use crate::config::Config;
use crate::domain::auth_state::AuthState;
use crate::domain::oauth_client::TokenExchanger;
use crate::domain::spotify::SpotifyClient;
use crate::domain::tokens::TokenRepository;

#[derive(Clone)]
pub struct AppState {
    pub(crate) config: Arc<Config>,
    pub(crate) tokens: Arc<dyn TokenRepository>,
    pub(crate) spotify: Arc<dyn SpotifyClient>,
    pub(crate) oauth: Arc<dyn TokenExchanger>,
    pub(crate) auth_state: Arc<AuthState>,
    pub(crate) state_store: Arc<StateStore>,
}

impl AppState {
    pub fn new(
        config: Arc<Config>,
        tokens: Arc<dyn TokenRepository>,
        spotify: Arc<dyn SpotifyClient>,
        oauth: Arc<dyn TokenExchanger>,
        auth_state: Arc<AuthState>,
        state_store: Arc<StateStore>,
    ) -> Self {
        Self {
            config,
            tokens,
            spotify,
            oauth,
            auth_state,
            state_store,
        }
    }

    /// Test constructor — same fields. Kept distinct from `new` so test
    /// wiring is greppable.
    pub fn new_for_test(
        config: Arc<Config>,
        tokens: Arc<dyn TokenRepository>,
        spotify: Arc<dyn SpotifyClient>,
        oauth: Arc<dyn TokenExchanger>,
        auth_state: Arc<AuthState>,
        state_store: Arc<StateStore>,
    ) -> Self {
        Self::new(config, tokens, spotify, oauth, auth_state, state_store)
    }
}
