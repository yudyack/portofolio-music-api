use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;

/// One Spotify OAuth token-set for the single owner. Persistence is opaque to
/// the domain — `RepoError::Backend` carries a stringified backend message so
/// the trait doesn't leak a `sqlx::Error` upward (criterion 20 spirit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRecord {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
    pub scope: String,
    pub owner_id: String,
}

#[derive(Debug, Error)]
pub enum RepoError {
    /// Generic storage-backend failure. Detail is in the message; never put
    /// the Client Secret or other secret material here. Logs get the full
    /// chain via `tracing::error!(error = ?source)`.
    #[error("storage backend error: {0}")]
    Backend(String),
}

#[async_trait]
pub trait TokenRepository: Send + Sync {
    async fn get(&self) -> Result<Option<TokenRecord>, RepoError>;
    async fn upsert(&self, tokens: TokenRecord) -> Result<(), RepoError>;
    async fn delete(&self) -> Result<(), RepoError>;
}
