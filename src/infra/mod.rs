//! Infrastructure layer. Every database query in this crate lives under
//! this module (criterion 20, enforced by tests/repository_pattern.rs).
//! Cycle 8 extends the rule symmetrically: every reqwest + governor
//! reference also lives under this module (enforced by
//! tests/reqwest_isolation.rs). Domain and app-services depend only on
//! the `TokenRepository` and `SpotifyClient` traits.

pub mod mock_spotify_client;
pub mod sqlite_token_repo;
pub mod spotify_backoff;
pub mod spotify_client;
pub(crate) mod spotify_governor;
pub(crate) mod spotify_retry;
pub mod token_exchanger;

use crate::domain::tokens::RepoError;
use sqlx::SqlitePool;

impl From<sqlx::Error> for RepoError {
    fn from(e: sqlx::Error) -> Self {
        // Display intentionally generic; full chain goes to logs only.
        RepoError::Backend(e.to_string())
    }
}

/// Apply all embedded migrations to the given pool. Called once at startup
/// before the HTTP server binds (criterion 21). The `sqlx::migrate!` macro
/// embeds files from `./migrations` at compile time.
pub async fn run_migrations(pool: &SqlitePool) -> Result<(), RepoError> {
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(|e| RepoError::Backend(e.to_string()))
}
