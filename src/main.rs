use music_api::config::Config;
use music_api::infra::run_migrations;
use music_api::infra::sqlite_token_repo::SqliteTokenRepository;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Criteria 22, 24: required env vars validated before anything else.
    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "startup config invalid");
            eprintln!("music-api: {e}");
            std::process::exit(1);
        }
    };

    // Criterion 21: migrations run before the listener binds. Done by parsing
    // DATABASE_URL, opening a pool with create_if_missing, applying pending
    // migrations, and only then constructing the repository + serving.
    let pool_opts = match SqliteConnectOptions::from_str(&config.database_url) {
        Ok(o) => o.create_if_missing(true),
        Err(e) => {
            tracing::error!(error = %e, "invalid DATABASE_URL");
            eprintln!("music-api: invalid DATABASE_URL: {e}");
            std::process::exit(1);
        }
    };
    let pool = match SqlitePoolOptions::new().connect_with(pool_opts).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "sqlite connect failed");
            eprintln!("music-api: sqlite connect failed: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = run_migrations(&pool).await {
        tracing::error!(error = %e, "migrations failed");
        eprintln!("music-api: migrations failed: {e}");
        std::process::exit(1);
    }
    let _repo: Arc<dyn music_api::domain::tokens::TokenRepository> =
        Arc::new(SqliteTokenRepository::new(pool));

    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .expect("bind");
    tracing::info!(%bind_addr, "music-api listening");
    axum::serve(listener, music_api::app())
        .await
        .expect("serve");
}
