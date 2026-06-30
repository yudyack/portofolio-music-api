use std::time::Duration;

use thiserror::Error;

/// Names of env vars consumed at startup. Exposed as constants so callers and
/// tests share one source of truth and Display strings can't drift.
pub const OWNER_SPOTIFY_USER_ID: &str = "OWNER_SPOTIFY_USER_ID";
pub const AUTH_BASIC_USERNAME: &str = "AUTH_BASIC_USERNAME";
pub const AUTH_BASIC_PASSWORD: &str = "AUTH_BASIC_PASSWORD";
pub const SPOTIFY_CLIENT_ID: &str = "SPOTIFY_CLIENT_ID";
pub const SPOTIFY_CLIENT_SECRET: &str = "SPOTIFY_CLIENT_SECRET";
pub const SPOTIFY_REDIRECT_URI: &str = "SPOTIFY_REDIRECT_URI";
pub const DATABASE_URL: &str = "DATABASE_URL";
/// Set to `1` (or any non-empty value) to make the service serve canned
/// Spotify fixtures instead of calling the real Spotify API. Local-dev
/// switch — skips OAuth, seeds a fake token, surfaces through /healthz
/// `mock_mode:true` so the leptos frontend can render a "MOCK DATA"
/// banner.
pub const MOCK_DATA: &str = "MOCK_DATA";

const DEFAULT_AUTH_BASIC_USERNAME: &str = "owner";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("required env var {0} is unset or empty")]
    Missing(&'static str),
}

#[derive(PartialEq, Eq, Clone)]
pub struct Config {
    pub owner_spotify_user_id: String,
    pub auth_basic_username: String,
    pub auth_basic_password: String,
    pub spotify_client_id: String,
    pub spotify_client_secret: String,
    pub spotify_redirect_uri: String,
    pub database_url: String,
    /// Serve canned Spotify fixtures instead of the real API. Off by default.
    pub mock_data: bool,
    /// Scheduler-push pacing + activity gate (spec §5.6). Code-config, not
    /// env-driven: tuning these is a deploy-time decision the operator
    /// makes by editing the defaults below, not a knob to risk a
    /// production trip on.
    pub scheduler: SchedulerConfig,
}

/// Per-endpoint scheduler tick interval. Each `/v1/*` endpoint has its own
/// loop in `app::scheduler` that uses the matching field here.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct SchedulerIntervals {
    pub now: Duration,
    pub recent: Duration,
    pub top: Duration,
    pub profile: Duration,
    pub playlists: Duration,
}

impl Default for SchedulerIntervals {
    fn default() -> Self {
        Self {
            // /v1/now changes constantly, but Spotify's per-app quota is
            // tight enough that a 3-second loop trips 429 within minutes —
            // the leptos progress bar interpolates client-side anyway, so
            // 10-second snapshot freshness is enough.
            now: Duration::from_secs(5),
            // Recently-played updates only after a track ends — minutes,
            // not seconds. The previous 30 s was tight enough that prod
            // logs showed a steady-state 429 loop on /me/player/recently-
            // played (Spotify Retry-After: 60 vs scheduler interval 30 s),
            // which then knocks the per-app rate-limit budget for every
            // other endpoint sharing it. 5 minutes is fresher than the
            // source data ever needs to be.
            recent: Duration::from_secs(300),
            // Spotify recomputes top/profile/playlists on the order of
            // hours-to-days; 5 minutes is already an order of magnitude
            // fresher than the source data ever changes.
            top: Duration::from_secs(300),
            profile: Duration::from_secs(300),
            playlists: Duration::from_secs(300),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct SchedulerConfig {
    pub intervals: SchedulerIntervals,
    /// Idle threshold for `ActivityTracker`. If no `/v1/*` request has
    /// landed within this window, the schedulers park on
    /// `ActivityTracker::woke` until a visitor arrives.
    pub idle_threshold: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            intervals: SchedulerIntervals::default(),
            idle_threshold: Duration::from_secs(60),
        }
    }
}

/// Hand-rolled Debug that redacts secret fields. Pre-arms criterion 13:
/// any future `tracing::*!(?config)` or `dbg!(config)` cannot leak the
/// Client Secret or the admin password. The non-secret fields print
/// verbatim so the diagnostic value of `?config` is preserved.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("owner_spotify_user_id", &self.owner_spotify_user_id)
            .field("auth_basic_username", &self.auth_basic_username)
            .field("auth_basic_password", &"<redacted>")
            .field("spotify_client_id", &self.spotify_client_id)
            .field("spotify_client_secret", &"<redacted>")
            .field("spotify_redirect_uri", &self.spotify_redirect_uri)
            .field("database_url", &self.database_url)
            .field("mock_data", &self.mock_data)
            .field("scheduler", &self.scheduler)
            .finish()
    }
}

impl Config {
    /// Build from process env. Thin wrapper over [`Self::from_lookup`].
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Build from any env source. Empty strings are treated as missing —
    /// the spec's startup-required checks use this rule so a stray `KEY=`
    /// in a `.env` file doesn't sneak past as "set".
    pub fn from_lookup<F>(get: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let required = |key: &'static str| -> Result<String, ConfigError> {
            match get(key) {
                Some(v) if !v.is_empty() => Ok(v),
                _ => Err(ConfigError::Missing(key)),
            }
        };

        Ok(Self {
            owner_spotify_user_id: required(OWNER_SPOTIFY_USER_ID)?,
            auth_basic_password: required(AUTH_BASIC_PASSWORD)?,
            auth_basic_username: get(AUTH_BASIC_USERNAME)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_AUTH_BASIC_USERNAME.to_string()),
            spotify_client_id: required(SPOTIFY_CLIENT_ID)?,
            spotify_client_secret: required(SPOTIFY_CLIENT_SECRET)?,
            spotify_redirect_uri: required(SPOTIFY_REDIRECT_URI)?,
            database_url: required(DATABASE_URL)?,
            // Any non-empty value enables mock mode; "0"/"false" also count
            // because the explicit user intent is "this var is set".
            mock_data: get(MOCK_DATA).map(|v| !v.is_empty()).unwrap_or(false),
            // Code-config — not env-driven. Defaults bake in the spec §5.6
            // numbers; operators edit the source if they ever need to tune.
            scheduler: SchedulerConfig::default(),
        })
    }
}
