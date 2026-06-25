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
    let env = fixture(&full_spotify_env());
    let cfg =
        Config::from_lookup(|k| env.get(k).map(|s| s.to_string())).expect("required vars present");
    assert_eq!(cfg.auth_basic_username, "owner");
    assert_eq!(cfg.owner_spotify_user_id, "yudhyapw");
    assert_eq!(cfg.auth_basic_password, "hunter2");
}

#[test]
fn auth_basic_username_env_override_is_respected() {
    let mut pairs: Vec<(&'static str, &'static str)> = full_spotify_env().to_vec();
    pairs.push(("AUTH_BASIC_USERNAME", "yudhya"));
    let env = fixture(&pairs);
    let cfg =
        Config::from_lookup(|k| env.get(k).map(|s| s.to_string())).expect("required vars present");
    assert_eq!(cfg.auth_basic_username, "yudhya");
}

#[test]
fn empty_auth_basic_username_falls_back_to_default() {
    let mut pairs: Vec<(&'static str, &'static str)> = full_spotify_env().to_vec();
    pairs.push(("AUTH_BASIC_USERNAME", ""));
    let env = fixture(&pairs);
    let cfg =
        Config::from_lookup(|k| env.get(k).map(|s| s.to_string())).expect("required vars present");
    assert_eq!(cfg.auth_basic_username, "owner");
}

fn full_spotify_env() -> [(&'static str, &'static str); 5] {
    [
        ("OWNER_SPOTIFY_USER_ID", "yudhyapw"),
        ("AUTH_BASIC_PASSWORD", "hunter2"),
        ("SPOTIFY_CLIENT_ID", "client123"),
        ("SPOTIFY_CLIENT_SECRET", "secret456"),
        ("SPOTIFY_REDIRECT_URI", "https://musicapi.yudhyapw.com/auth/spotify/callback"),
    ]
}

#[test]
fn missing_spotify_client_id_is_reported_by_name() {
    let pairs: Vec<(&'static str, &'static str)> = full_spotify_env()
        .into_iter()
        .filter(|(k, _)| *k != "SPOTIFY_CLIENT_ID")
        .collect();
    let env = fixture(&pairs);
    let err = Config::from_lookup(|k| env.get(k).map(|s| s.to_string())).unwrap_err();
    assert_eq!(err, ConfigError::Missing("SPOTIFY_CLIENT_ID"));
}

#[test]
fn missing_spotify_client_secret_is_reported_by_name() {
    let pairs: Vec<(&'static str, &'static str)> = full_spotify_env()
        .into_iter()
        .filter(|(k, _)| *k != "SPOTIFY_CLIENT_SECRET")
        .collect();
    let env = fixture(&pairs);
    let err = Config::from_lookup(|k| env.get(k).map(|s| s.to_string())).unwrap_err();
    assert_eq!(err, ConfigError::Missing("SPOTIFY_CLIENT_SECRET"));
}

#[test]
fn missing_spotify_redirect_uri_is_reported_by_name() {
    let pairs: Vec<(&'static str, &'static str)> = full_spotify_env()
        .into_iter()
        .filter(|(k, _)| *k != "SPOTIFY_REDIRECT_URI")
        .collect();
    let env = fixture(&pairs);
    let err = Config::from_lookup(|k| env.get(k).map(|s| s.to_string())).unwrap_err();
    assert_eq!(err, ConfigError::Missing("SPOTIFY_REDIRECT_URI"));
}

#[test]
fn full_env_populates_all_spotify_fields() {
    let env = fixture(&full_spotify_env());
    let cfg =
        Config::from_lookup(|k| env.get(k).map(|s| s.to_string())).expect("required vars present");
    assert_eq!(cfg.spotify_client_id, "client123");
    assert_eq!(cfg.spotify_client_secret, "secret456");
    assert_eq!(
        cfg.spotify_redirect_uri,
        "https://musicapi.yudhyapw.com/auth/spotify/callback"
    );
}
