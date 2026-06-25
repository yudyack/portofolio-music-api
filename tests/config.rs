use music_api::config::{Config, ConfigError};
use std::collections::HashMap;

fn fixture(pairs: &[(&'static str, &'static str)]) -> HashMap<&'static str, &'static str> {
    pairs.iter().copied().collect()
}

#[test]
fn missing_owner_spotify_user_id_is_reported_by_name() {
    let env = fixture(&[("AUTH_BASIC_PASSWORD", "hunter2")]);
    let err = Config::from_lookup(|k| env.get(k).map(|s| s.to_string())).unwrap_err();
    assert_eq!(err, ConfigError::Missing("OWNER_SPOTIFY_USER_ID"));
    assert!(
        err.to_string().contains("OWNER_SPOTIFY_USER_ID"),
        "Display must name the missing var, got {err}",
    );
}

#[test]
fn missing_auth_basic_password_is_reported_by_name() {
    let env = fixture(&[("OWNER_SPOTIFY_USER_ID", "yudhyapw")]);
    let err = Config::from_lookup(|k| env.get(k).map(|s| s.to_string())).unwrap_err();
    assert_eq!(err, ConfigError::Missing("AUTH_BASIC_PASSWORD"));
    assert!(
        err.to_string().contains("AUTH_BASIC_PASSWORD"),
        "Display must name the missing var, got {err}",
    );
}

#[test]
fn empty_string_counts_as_missing() {
    let env = fixture(&[
        ("OWNER_SPOTIFY_USER_ID", ""),
        ("AUTH_BASIC_PASSWORD", "hunter2"),
    ]);
    let err = Config::from_lookup(|k| env.get(k).map(|s| s.to_string())).unwrap_err();
    assert_eq!(err, ConfigError::Missing("OWNER_SPOTIFY_USER_ID"));
}

#[test]
fn auth_basic_username_defaults_to_owner() {
    let env = fixture(&[
        ("OWNER_SPOTIFY_USER_ID", "yudhyapw"),
        ("AUTH_BASIC_PASSWORD", "hunter2"),
    ]);
    let cfg =
        Config::from_lookup(|k| env.get(k).map(|s| s.to_string())).expect("required vars present");
    assert_eq!(cfg.auth_basic_username, "owner");
    assert_eq!(cfg.owner_spotify_user_id, "yudhyapw");
    assert_eq!(cfg.auth_basic_password, "hunter2");
}

#[test]
fn auth_basic_username_env_override_is_respected() {
    let env = fixture(&[
        ("OWNER_SPOTIFY_USER_ID", "yudhyapw"),
        ("AUTH_BASIC_PASSWORD", "hunter2"),
        ("AUTH_BASIC_USERNAME", "yudhya"),
    ]);
    let cfg =
        Config::from_lookup(|k| env.get(k).map(|s| s.to_string())).expect("required vars present");
    assert_eq!(cfg.auth_basic_username, "yudhya");
}

#[test]
fn empty_auth_basic_username_falls_back_to_default() {
    let env = fixture(&[
        ("OWNER_SPOTIFY_USER_ID", "yudhyapw"),
        ("AUTH_BASIC_PASSWORD", "hunter2"),
        ("AUTH_BASIC_USERNAME", ""),
    ]);
    let cfg =
        Config::from_lookup(|k| env.get(k).map(|s| s.to_string())).expect("required vars present");
    assert_eq!(cfg.auth_basic_username, "owner");
}
