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

#[async_trait]
impl TokenExchanger for ReqwestTokenExchanger {
    async fn refresh(&self, refresh_token: &str) -> Result<RefreshedTokens, TokenExchangeError> {
        let resp = self
            .client
            .post(&self.token_url)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ])
            .send()
            .await
            .map_err(|e| TokenExchangeError::Transport(e.to_string()))?;

        let status = resp.status();

        // A 400 may be `invalid_grant` (revoked refresh token) — the one
        // failure the caller must distinguish to flip NeedsReauth.
        if status == StatusCode::BAD_REQUEST {
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| TokenExchangeError::Decode(e.to_string()))?;
            if body.get("error").and_then(|e| e.as_str()) == Some("invalid_grant") {
                return Err(TokenExchangeError::InvalidGrant);
            }
            return Err(TokenExchangeError::Status(400));
        }

        if !status.is_success() {
            return Err(TokenExchangeError::Status(status.as_u16()));
        }

        let dto: TokenResponseDto = resp
            .json()
            .await
            .map_err(|e| TokenExchangeError::Decode(e.to_string()))?;

        Ok(RefreshedTokens {
            access_token: dto.access_token,
            refresh_token: dto.refresh_token,
            expires_in: dto.expires_in,
            scope: dto.scope,
        })
    }
}
