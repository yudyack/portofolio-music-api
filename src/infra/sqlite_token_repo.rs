use crate::domain::tokens::{RepoError, TokenRecord, TokenRepository};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};

pub struct SqliteTokenRepository {
    pool: SqlitePool,
}

impl SqliteTokenRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl TokenRepository for SqliteTokenRepository {
    async fn get(&self) -> Result<Option<TokenRecord>, RepoError> {
        let maybe_row = sqlx::query(
            "SELECT access_token, refresh_token, expires_at, scope, owner_id \
             FROM tokens WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(match maybe_row {
            None => None,
            Some(row) => Some(TokenRecord {
                access_token: row.try_get::<String, _>("access_token")?,
                refresh_token: row.try_get::<String, _>("refresh_token")?,
                expires_at: row.try_get::<DateTime<Utc>, _>("expires_at")?,
                scope: row.try_get::<String, _>("scope")?,
                owner_id: row.try_get::<String, _>("owner_id")?,
            }),
        })
    }

    async fn upsert(&self, t: TokenRecord) -> Result<(), RepoError> {
        sqlx::query(
            "INSERT INTO tokens (id, access_token, refresh_token, expires_at, scope, owner_id, updated_at) \
             VALUES (1, ?1, ?2, ?3, ?4, ?5, datetime('now')) \
             ON CONFLICT(id) DO UPDATE SET \
                access_token = excluded.access_token, \
                refresh_token = excluded.refresh_token, \
                expires_at = excluded.expires_at, \
                scope = excluded.scope, \
                owner_id = excluded.owner_id, \
                updated_at = datetime('now')",
        )
        .bind(&t.access_token)
        .bind(&t.refresh_token)
        .bind(t.expires_at)
        .bind(&t.scope)
        .bind(&t.owner_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn delete(&self) -> Result<(), RepoError> {
        sqlx::query("DELETE FROM tokens WHERE id = 1")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
