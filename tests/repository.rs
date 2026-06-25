//! Round-trip tests for the SqliteTokenRepository against an in-memory SQLite
//! database. Asserts the contract of the `TokenRepository` trait: get returns
//! None on empty DB, upsert + get round-trips, upsert overwrites, delete clears.
//!
//! Also doubles as the criterion 21 check: `run_migrations` against a fresh
//! in-memory DB succeeds (the tokens table is created and queryable).

use chrono::{DateTime, TimeZone, Utc};
use music_api::domain::tokens::{TokenRecord, TokenRepository};
use music_api::infra::sqlite_token_repo::SqliteTokenRepository;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

fn fixed_expires() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 1, 12, 0, 0).unwrap()
}

fn record(access: &str) -> TokenRecord {
    TokenRecord {
        access_token: access.to_string(),
        refresh_token: "refresh-abc".to_string(),
        expires_at: fixed_expires(),
        scope: "user-read-private user-top-read".to_string(),
        owner_id: "yudhyapw".to_string(),
    }
}

async fn fresh_pool() -> SqlitePool {
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")
        .expect("static URI")
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1) // memory DB lives in the connection
        .connect_with(opts)
        .await
        .expect("connect in-memory sqlite");
    music_api::infra::run_migrations(&pool)
        .await
        .expect("migrations apply on fresh in-memory db (criterion 21)");
    pool
}

#[tokio::test]
async fn get_returns_none_when_table_is_empty() {
    let pool = fresh_pool().await;
    let repo = SqliteTokenRepository::new(pool);
    assert!(repo.get().await.expect("get").is_none());
}

#[tokio::test]
async fn upsert_then_get_round_trips_the_record() {
    let pool = fresh_pool().await;
    let repo = SqliteTokenRepository::new(pool);
    let rec = record("access-1");
    repo.upsert(rec.clone()).await.expect("upsert");
    let fetched = repo.get().await.expect("get").expect("row exists");
    assert_eq!(fetched, rec);
}

#[tokio::test]
async fn upsert_overwrites_existing_single_row() {
    let pool = fresh_pool().await;
    let repo = SqliteTokenRepository::new(pool);
    repo.upsert(record("access-1")).await.unwrap();
    repo.upsert(record("access-2")).await.unwrap();
    let fetched = repo.get().await.unwrap().expect("row exists");
    assert_eq!(fetched.access_token, "access-2");
}

#[tokio::test]
async fn delete_clears_the_row() {
    let pool = fresh_pool().await;
    let repo = SqliteTokenRepository::new(pool);
    repo.upsert(record("access-1")).await.unwrap();
    repo.delete().await.expect("delete");
    assert!(repo.get().await.unwrap().is_none());
}

#[tokio::test]
async fn migrations_create_tokens_table_with_single_row_check() {
    // Criterion 21: migrations apply on startup. After run_migrations, the
    // `tokens` table exists and enforces the single-row id=1 invariant.
    let pool = fresh_pool().await;
    // A direct INSERT with id != 1 must fail the CHECK constraint.
    let result = sqlx::query("INSERT INTO tokens (id, access_token, refresh_token, expires_at, scope, owner_id) VALUES (2, 'a', 'r', '2026-07-01T12:00:00Z', 's', 'o')")
        .execute(&pool)
        .await;
    assert!(
        result.is_err(),
        "single-row CHECK (id = 1) must reject id=2"
    );
}
