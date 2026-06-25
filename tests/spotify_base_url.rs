//! Cycle-8 QA add: regression guard for the `base_url` convention on
//! `ReqwestSpotifyClient`.
//!
//! Production wires `ReqwestSpotifyClient::new("https://api.spotify.com")`
//! — EXCLUDES `/v1`. Callers pass the full Spotify path including the
//! version segment (e.g. `"/v1/me"`). A regression to
//! `"https://api.spotify.com/v1"` would compose `/v1/v1/me` and silently
//! 404 in production. The existing `tests/spotify_pacing.rs` happens to
//! exercise the convention against a wiremock base URL, but it uses the
//! test-only `with_quota` constructor — it cannot catch a regression in
//! `init()`'s hardcoded production base URL.
//!
//! This static-source check catches the cheap-typo regression NOW.
//! Surfaced as architect cycles 7-8 review caveat #5.

use std::path::Path;

#[test]
fn lib_rs_wires_spotify_client_with_versionless_base_url() {
    let src = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
    )
    .expect("read src/lib.rs");

    // Production must reference exactly `"https://api.spotify.com"` and NOT
    // any path-bearing variant. The check is intentionally narrow: forbids
    // the three most-likely typo regressions; doesn't try to parse strings.
    let forbidden_forms = [
        "\"https://api.spotify.com/v1\"",
        "\"https://api.spotify.com/v1/\"",
        "\"https://api.spotify.com/\"",
    ];
    for bad in &forbidden_forms {
        assert!(
            !src.contains(bad),
            "src/lib.rs contains forbidden base_url form `{bad}` — base_url EXCLUDES \
             /v1; callers pass full path including version (architect cycles 7-8 caveat #5)",
        );
    }
    assert!(
        src.contains("\"https://api.spotify.com\""),
        "src/lib.rs must reference the literal `\"https://api.spotify.com\"` so \
         init() constructs ReqwestSpotifyClient with the versionless base URL \
         (architect cycles 7-8 caveat #5)"
    );
}
