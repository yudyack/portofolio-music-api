//! Concrete reqwest-backed [`TokenExchanger`]. POSTs to the configured
//! `accounts.spotify.com/api/token` endpoint with the app's client
//! credentials via HTTP Basic and `grant_type=refresh_token`.
//!
//! The refresh endpoint is NOT routed through the `governor` pacing chain
//! that fronts `api.spotify.com`: refreshes are rare (≈ hourly, plus the
//! occasional 401 recovery) and target a different host. If refresh
//! traffic ever needs pacing, wrap this client the same way
//! `ReqwestSpotifyClient` wraps its data client.

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use crate::domain::oauth_client::{RefreshedTokens, TokenExchangeError, TokenExchanger};

pub struct ReqwestTokenExchanger {
    /// Full token endpoint, e.g. `https://accounts.spotify.com/api/token`.
    token_url: String,
    client_id: String,
    client_secret: String,
    client: Client,
}

impl ReqwestTokenExchanger {
    pub fn new(
        token_url: String,
        client_id: String,
        client_secret: String,
    ) -> Result<Self, TokenExchangeError> {
        let client = Client::builder()
            .build()
            .map_err(|e| TokenExchangeError::Transport(e.to_string()))?;
        Ok(Self {
            token_url,
            client_id,
            client_secret,
            client,
        })
    }
}

/// Wire shape of a successful token response. `refresh_token` and `scope`
/// are optional (Spotify omits the former when it doesn't rotate it).
#[derive(Deserialize)]
struct TokenResponseDto {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: i64,
    #[serde(default)]
    scope: Option<String>,
}

/// Mask a token-like string for logging — show a short prefix and the
/// total length so refreshes are distinguishable without exposing the
/// secret material in plaintext.
fn mask_token(s: &str) -> String {
    if s.len() <= 8 {
        return "***".to_string();
    }
    format!("{}…(len={})", &s[..6], s.len())
}

impl ReqwestTokenExchanger {
    /// POST the form to the token endpoint and map the response. Shared by
    /// both grants — only the form fields differ.
    async fn post_token(
        &self,
        form: &[(&str, &str)],
    ) -> Result<RefreshedTokens, TokenExchangeError> {
        let grant_type = form
            .iter()
            .find(|(k, _)| *k == "grant_type")
            .map(|(_, v)| *v)
            .unwrap_or("");
        tracing::info!(
            target: "music_api::wire::spotify_oauth",
            direction = "→",
            method = "POST",
            url = %self.token_url,
            grant_type = grant_type,
            "spotify oauth request",
        );

        let resp = self
            .client
            .post(&self.token_url)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(form)
            .send()
            .await
            .map_err(|e| TokenExchangeError::Transport(e.to_string()))?;

        let status = resp.status();
        // A 400 may be `invalid_grant` (dead refresh / bad code) — the
        // caller must distinguish it.
        if status == StatusCode::BAD_REQUEST {
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| TokenExchangeError::Decode(e.to_string()))?;
            tracing::info!(
                target: "music_api::wire::spotify_oauth",
                direction = "←",
                status = 400,
                body = %body,
                "spotify oauth response (bad request)",
            );
            if body.get("error").and_then(|e| e.as_str()) == Some("invalid_grant") {
                return Err(TokenExchangeError::InvalidGrant);
            }
            return Err(TokenExchangeError::Status(400));
        }
        if !status.is_success() {
            tracing::info!(
                target: "music_api::wire::spotify_oauth",
                direction = "←",
                status = status.as_u16(),
                "spotify oauth response (error)",
            );
            return Err(TokenExchangeError::Status(status.as_u16()));
        }

        let dto: TokenResponseDto = resp
            .json()
            .await
            .map_err(|e| TokenExchangeError::Decode(e.to_string()))?;
        tracing::info!(
            target: "music_api::wire::spotify_oauth",
            direction = "←",
            status = status.as_u16(),
            access_token = %mask_token(&dto.access_token),
            refresh_token = %dto
                .refresh_token
                .as_deref()
                .map(mask_token)
                .unwrap_or_else(|| "<absent>".to_string()),
            expires_in = dto.expires_in,
            scope = dto.scope.as_deref().unwrap_or(""),
            "spotify oauth response",
        );
        Ok(RefreshedTokens {
            access_token: dto.access_token,
            refresh_token: dto.refresh_token,
            expires_in: dto.expires_in,
            scope: dto.scope,
        })
    }
}

#[async_trait]
impl TokenExchanger for ReqwestTokenExchanger {
    async fn refresh(&self, refresh_token: &str) -> Result<RefreshedTokens, TokenExchangeError> {
        self.post_token(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .await
    }

    async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
    ) -> Result<RefreshedTokens, TokenExchangeError> {
        self.post_token(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
        ])
        .await
    }
}
