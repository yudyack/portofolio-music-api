pub mod config;
pub mod oauth;

use axum::{routing::get, Json, Router};
use serde::Serialize;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Operational health snapshot. Always returns 200 (criterion 15) so the
/// Cloudflare tunnel doesn't flap when the upstream link is unhealthy — the
/// body carries the degradation state instead.
#[derive(Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
    token_state: &'static str,
    last_fetch_ts: Option<String>,
}

async fn healthz() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: VERSION,
        token_state: "uninitialized",
        last_fetch_ts: None,
    })
}

pub fn app() -> Router {
    Router::new().route("/healthz", get(healthz))
}
