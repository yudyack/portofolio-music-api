//! Owner OAuth bootstrap. `/login` is Basic-auth gated and redirects to
//! Spotify; `/callback` (no Basic auth — Spotify's redirect can't carry it)
//! validates state, exchanges the code, verifies the owner via `/me`, and
//! stores the tokens. Criteria 1b, 2, 3, 4, 23, 25.

use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_extra::headers::authorization::Basic;
use axum_extra::headers::Authorization;
use axum_extra::TypedHeader;
use chrono::{Duration, Utc};
use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::config::Config;
use crate::domain::tokens::TokenRecord;
use crate::oauth::build_authorize_url;
use crate::AppState;

const REALM: &str = "Basic realm=\"music-api\"";

pub(crate) fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, REALM)],
        "unauthorized",
    )
        .into_response()
}

/// Constant-time Basic-auth check against config (criterion 23). Username
/// is compared plainly; the password goes through `subtle::ConstantTimeEq`.
/// Shared with `routes::admin` so both owner-gated surfaces flow through
/// the same constant-time path — no second auth implementation to drift.
pub(crate) fn basic_auth_ok(
    config: &Config,
    auth: &Option<TypedHeader<Authorization<Basic>>>,
) -> bool {
    let Some(TypedHeader(Authorization(basic))) = auth else {
        return false;
    };
    let user_ok = basic.username() == config.auth_basic_username;
    let pass_ok: bool = basic
        .password()
        .as_bytes()
        .ct_eq(config.auth_basic_password.as_bytes())
        .into();
    user_ok && pass_ok
}

/// GET /auth/spotify/login (criteria 1b, 23).
pub async fn login(
    State(state): State<AppState>,
    auth: Option<TypedHeader<Authorization<Basic>>>,
) -> Response {
    if !basic_auth_ok(&state.config, &auth) {
        return unauthorized();
    }
    let csrf = state.state_store.issue();
    let url = build_authorize_url(&state.config, &csrf);
    let cookie = format!("oauth_state={csrf}; HttpOnly; Path=/; Max-Age=600; SameSite=Lax");
    (
        StatusCode::FOUND,
        [(header::LOCATION, url), (header::SET_COOKIE, cookie)],
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: String,
    state: String,
}

/// GET /auth/spotify/callback (criteria 2, 3, 4, 25). No Basic auth.
pub async fn callback(State(state): State<AppState>, Query(q): Query<CallbackQuery>) -> Response {
    // criterion 2 / 25: state must match a live issuance.
    if !state.state_store.consume(&q.state) {
        return (StatusCode::BAD_REQUEST, "state mismatch").into_response();
    }

    // criterion 3: exchange the code, then verify the owner via /me.
    let tokens = match state
        .oauth
        .exchange_code(&q.code, &state.config.spotify_redirect_uri)
        .await
    {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "code exchange failed");
            return (StatusCode::BAD_GATEWAY, "token exchange failed").into_response();
        }
    };
    let me = match state.spotify.get_json("/v1/me", &tokens.access_token).await {
        // /v1/me always returns 200 with a body; a 204 here would be a
        // Spotify API contract change, surface as upstream failure.
        Ok(Some(v)) => v,
        Ok(None) => {
            tracing::warn!("/v1/me returned 204 unexpectedly");
            return (StatusCode::BAD_GATEWAY, "profile fetch failed").into_response();
        }
        Err(e) => {
            tracing::warn!(error = %e, "profile fetch failed");
            return (StatusCode::BAD_GATEWAY, "profile fetch failed").into_response();
        }
    };
    let me_id = me.get("id").and_then(|v| v.as_str()).unwrap_or_default();
    if me_id != state.config.owner_spotify_user_id {
        return (StatusCode::FORBIDDEN, "not the owner").into_response();
    }

    // criterion 4: upsert the tokens, respond 200.
    let record = TokenRecord {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token.unwrap_or_default(),
        expires_at: Utc::now() + Duration::seconds(tokens.expires_in),
        scope: tokens.scope.unwrap_or_default(),
        owner_id: me_id.to_string(),
    };
    if let Err(e) = state.tokens.upsert(record).await {
        tracing::error!(error = %e, "token upsert failed");
        return (StatusCode::INTERNAL_SERVER_ERROR, "could not store tokens").into_response();
    }
    state.auth_state.clear(); // a successful re-link clears NeedsReauth
    (StatusCode::OK, "Spotify linked. You can close this tab.").into_response()
}
