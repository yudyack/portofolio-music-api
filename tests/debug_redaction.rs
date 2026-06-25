//! Architect-flagged caveat (cycles 0-6 review): the hand-rolled
//! `impl Debug for Config` must redact `spotify_client_secret` and
//! `auth_basic_password` so a future `tracing::*!(?config)` or `dbg!(config)`
//! cannot leak them. Pre-arms criterion 13.

use music_api::config::Config;
use std::collections::HashMap;

const SECRET_SENTINEL: &str = "S3CR3T-SHOULD-NEVER-APPEAR-IN-DEBUG";
const PASSWORD_SENTINEL: &str = "P4SSW0RD-SHOULD-NEVER-APPEAR-IN-DEBUG";

fn config_with(secret: &str, password: &str) -> Config {
    let env: HashMap<&'static str, String> = [
        ("OWNER_SPOTIFY_USER_ID", "yudhyapw".to_string()),
        ("AUTH_BASIC_PASSWORD", password.to_string()),
        ("AUTH_BASIC_USERNAME", "yudhya".to_string()),
        ("SPOTIFY_CLIENT_ID", "client-id-abc".to_string()),
        ("SPOTIFY_CLIENT_SECRET", secret.to_string()),
        (
            "SPOTIFY_REDIRECT_URI",
            "https://musicapi.yudhyapw.com/auth/spotify/callback".to_string(),
        ),
        ("DATABASE_URL", "sqlite::memory:".to_string()),
    ]
    .into_iter()
    .collect();
    Config::from_lookup(|k| env.get(k).cloned()).expect("test fixture is complete")
}

#[test]
fn config_debug_does_not_leak_spotify_client_secret() {
    let config = config_with(SECRET_SENTINEL, "ignored");
    let debug = format!("{config:?}");
    assert!(
        !debug.contains(SECRET_SENTINEL),
        "Debug must NOT contain the raw spotify_client_secret value. Got:\n{debug}",
    );
}

#[test]
fn config_debug_does_not_leak_auth_basic_password() {
    let config = config_with("ignored", PASSWORD_SENTINEL);
    let debug = format!("{config:?}");
    assert!(
        !debug.contains(PASSWORD_SENTINEL),
        "Debug must NOT contain the raw auth_basic_password value. Got:\n{debug}",
    );
}

#[test]
fn config_debug_marks_redacted_fields_explicitly() {
    let config = config_with(SECRET_SENTINEL, PASSWORD_SENTINEL);
    let debug = format!("{config:?}");
    // The redaction must be discoverable by a reader — a silent omission
    // would let a future refactor accidentally re-derive Debug without
    // anyone noticing the leak returned.
    assert!(
        debug.contains("<redacted>"),
        "Debug must mark secret fields with the literal \"<redacted>\". Got:\n{debug}",
    );
}

#[test]
fn config_debug_preserves_nonsecret_fields_for_diagnostics() {
    // Confirm the redaction didn't go too far. The whole point of keeping
    // a Debug impl (rather than just removing the derive) is that the
    // non-secret fields stay useful for ?config diagnostics.
    let config = config_with("ignored", "ignored");
    let debug = format!("{config:?}");
    for field_value in [
        "yudhyapw",
        "yudhya",
        "client-id-abc",
        "https://musicapi.yudhyapw.com/auth/spotify/callback",
        "sqlite::memory:",
    ] {
        assert!(
            debug.contains(field_value),
            "Debug should preserve non-secret field value {field_value:?}; got:\n{debug}",
        );
    }
}
