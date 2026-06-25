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

const DEFAULT_AUTH_BASIC_USERNAME: &str = "owner";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("required env var {0} is unset or empty")]
    Missing(&'static str),
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Config {
    pub owner_spotify_user_id: String,
    pub auth_basic_username: String,
    pub auth_basic_password: String,
    pub spotify_client_id: String,
    pub spotify_client_secret: String,
    pub spotify_redirect_uri: String,
    pub database_url: String,
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
        })
    }
}
