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

/// Mask a token-like string for logging. The length is emitted as a
/// rotation signal (a refresh that changes token length is observable);
/// no token bytes are exposed. Matches the repo's `<redacted>` precedent
/// (`Config::Debug` in `src/config.rs`, pinned by `tests/debug_redaction.rs`).
pub(crate) fn mask_token(s: &str) -> String {
    format!("<redacted len={}>", s.len())
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
            // Per RFC 6749 §5.2 the body's `error_description` may echo
            // offending grant material (e.g. the authorization code).
            // Log only the parsed `error` discriminant — diagnostic and
            // PII/secret-free.
            let error_kind = body
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("<unknown>");
            tracing::warn!(
                target: "music_api::wire::spotify_oauth",
                direction = "←",
                status = 400,
                error = %error_kind,
                "spotify oauth response (bad request)",
            );
            if error_kind == "invalid_grant" {
                return Err(TokenExchangeError::InvalidGrant);
            }
            return Err(TokenExchangeError::Status(400));
        }
        if !status.is_success() {
            // Capture a capped body preview for 401/5xx diagnosis. Token-
            // endpoint error envelopes are diagnostic strings only (no
            // user PII); 256 chars is enough for `invalid_client` style
            // shapes and defends against unbounded edge responses.
            let body = resp.text().await.unwrap_or_default();
            let body_preview: String = body.chars().take(256).collect();
            tracing::warn!(
                target: "music_api::wire::spotify_oauth",
                direction = "←",
                status = status.as_u16(),
                bytes = body.len(),
                body_preview = %body_preview,
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

#[cfg(test)]
mod tests {
    //! Pin the redaction invariant added by this PR. A future refactor that
    //! drops the `<redacted>` marker or re-introduces a plaintext prefix
    //! must fail compile/test, not silently in prod logs. Mirrors the
    //! Config::Debug pattern in `tests/debug_redaction.rs`.
    use super::mask_token;

    const TOKEN_SENTINEL: &str = "S3CR3T-ACCESS-TOKEN-SHOULD-NEVER-APPEAR-IN-FULL-XYZ";

    #[test]
    fn mask_token_does_not_contain_full_input() {
        let out = mask_token(TOKEN_SENTINEL);
        assert!(
            !out.contains(TOKEN_SENTINEL),
            "mask_token must not echo its input. Got:\n{out}",
        );
        // Defend against a future refactor re-introducing a prefix: no
        // contiguous 6-char slice of the sentinel may appear in the output.
        for i in 0..=TOKEN_SENTINEL.len() - 6 {
            let slice = &TOKEN_SENTINEL[i..i + 6];
            assert!(
                !out.contains(slice),
                "mask_token leaked a 6-char slice {slice:?} of the input. Got:\n{out}",
            );
        }
    }

    #[test]
    fn mask_token_uses_redacted_marker() {
        let out = mask_token("anything");
        assert!(
            out.contains("<redacted"),
            "mask_token must mark redacted output with the literal \"<redacted\" \
             so a future Debug-style refactor cannot silently un-redact. Got:\n{out}",
        );
    }

    #[test]
    fn mask_token_preserves_length_signal() {
        // Spotify access tokens are typically 43 chars; the length is the
        // rotation-diagnostic value the masked form must preserve.
        let input = "x".repeat(43);
        let out = mask_token(&input);
        assert!(
            out.contains("len=43"),
            "mask_token must surface the length as `len=N`. Got:\n{out}",
        );
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
