use music_api::config::Config;
use music_api::oauth::{build_authorize_url, SCOPES};
use std::collections::HashMap;
use url::Url;

fn test_config() -> Config {
    let env: HashMap<&'static str, &'static str> = [
        ("OWNER_SPOTIFY_USER_ID", "yudhyapw"),
        ("AUTH_BASIC_PASSWORD", "hunter2"),
        ("SPOTIFY_CLIENT_ID", "client123"),
        ("SPOTIFY_CLIENT_SECRET", "secret456"),
        (
            "SPOTIFY_REDIRECT_URI",
            "https://musicapi.yudhyapw.com/auth/spotify/callback",
        ),
    ]
    .into_iter()
    .collect();
    Config::from_lookup(|k| env.get(k).map(|s| s.to_string())).expect("test fixture is complete")
}

fn pairs(url: &Url) -> HashMap<String, String> {
    url.query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[test]
fn targets_spotify_authorize_endpoint() {
    let url = Url::parse(&build_authorize_url(&test_config(), "state-xyz")).unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some("accounts.spotify.com"));
    assert_eq!(url.path(), "/authorize");
}

#[test]
fn uses_response_type_code() {
    let url = Url::parse(&build_authorize_url(&test_config(), "state-xyz")).unwrap();
    assert_eq!(pairs(&url).get("response_type"), Some(&"code".to_string()));
}

#[test]
fn never_emits_implicit_grant_response_type() {
    let raw = build_authorize_url(&test_config(), "state-xyz");
    assert!(
        !raw.contains("response_type=token"),
        "Implicit Grant forbidden by spec criterion 16; got {raw}",
    );
}

#[test]
fn passes_through_client_id_redirect_and_state() {
    let url = Url::parse(&build_authorize_url(&test_config(), "csrf-token-xyz")).unwrap();
    let p = pairs(&url);
    assert_eq!(p.get("client_id"), Some(&"client123".to_string()));
    assert_eq!(
        p.get("redirect_uri"),
        Some(&"https://musicapi.yudhyapw.com/auth/spotify/callback".to_string()),
    );
    assert_eq!(p.get("state"), Some(&"csrf-token-xyz".to_string()));
}

#[test]
fn contains_all_six_required_scopes_and_no_extras() {
    let url = Url::parse(&build_authorize_url(&test_config(), "state-xyz")).unwrap();
    let scope = pairs(&url)
        .get("scope")
        .cloned()
        .expect("scope query param present");
    let actual: Vec<&str> = scope.split_whitespace().collect();
    let expected = [
        "user-read-playback-state",
        "user-read-recently-played",
        "user-top-read",
        "user-read-private",
        "playlist-read-private",
        "user-follow-read",
    ];

    assert_eq!(
        actual.len(),
        expected.len(),
        "criterion 1: exactly the 6 spec scopes (no extras). got: {actual:?}"
    );
    for s in &expected {
        assert!(actual.contains(s), "missing required scope {s}; got {actual:?}");
    }
    // SCOPES constant agreement
    assert_eq!(SCOPES.len(), expected.len());
    for s in SCOPES {
        assert!(expected.contains(s), "SCOPES drifted from spec: {s}");
    }
}

#[test]
fn excludes_disallowed_scopes() {
    let url = Url::parse(&build_authorize_url(&test_config(), "state-xyz")).unwrap();
    let scope = pairs(&url).get("scope").cloned().unwrap();
    for forbidden in [
        "user-modify-playback-state",
        "user-read-email",
        "streaming",
        "playlist-modify-public",
        "playlist-modify-private",
    ] {
        assert!(
            !scope.contains(forbidden),
            "scope must NOT include {forbidden}; got {scope:?}"
        );
    }
}

#[test]
fn static_grep_no_implicit_grant_in_src_tree() {
    use std::path::Path;

    fn walk(dir: &Path, hits: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                walk(&path, hits);
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                let body = std::fs::read_to_string(&path).unwrap();
                if body.contains("response_type=token") {
                    hits.push(path.display().to_string());
                }
            }
        }
    }

    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = vec![];
    walk(&src, &mut hits);
    assert!(
        hits.is_empty(),
        "criterion 16 static-grep: response_type=token forbidden in src/, but found in: {hits:#?}"
    );
}
