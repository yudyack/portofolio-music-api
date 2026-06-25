//! Infrastructure layer. Every `sqlx::query*` call in this crate lives under
//! this module (criterion 20, enforced by a static-grep test). Domain and
//! app-services depend only on the `TokenRepository` trait re-exported from
//! [`crate::domain::tokens`].

pub mod sqlite_token_repo;

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
