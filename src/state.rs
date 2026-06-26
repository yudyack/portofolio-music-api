//! Handler-visible application state. Cloned into every Router branch.
//! Every field is `Arc<…>` so cloning is cheap.

use std::sync::Arc;

use crate::app::cache::Cache;
use crate::app::spotify_service::SpotifyService;
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
    /// Constructed from the injected `tokens` + `spotify` + `oauth` +
    /// `auth_state`. Wraps the data-plane (`/v1/*`) reads with the
    /// refresh-on-401 + single-flight machinery. The OAuth bootstrap
    /// (`/auth/spotify/*`) uses `spotify` directly — different path, no
    /// token-rotation concerns there.
    pub(crate) spotify_service: Arc<SpotifyService>,
    /// In-memory `/v1/*` cache (criterion 11). Lives only as long as the
    /// process — Spotify content is never persisted (spec §5.6).
    pub(crate) cache: Arc<Cache>,
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
        let spotify_service = Arc::new(SpotifyService::new(
            tokens.clone(),
            spotify.clone(),
            oauth.clone(),
            auth_state.clone(),
        ));
        let cache = Arc::new(Cache::new());
        Self {
            config,
            tokens,
            spotify,
            oauth,
            auth_state,
            state_store,
            spotify_service,
            cache,
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
