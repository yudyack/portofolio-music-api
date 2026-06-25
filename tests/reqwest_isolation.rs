//! Criterion-20 symmetric extension: every `reqwest`, `reqwest_middleware`,
//! `reqwest_retry`, and `governor` reference may only appear under
//! `src/infra/`. Domain and app-services depend only on the
//! `SpotifyClient` trait re-exported from [`music_api::domain::spotify`].
//!
//! Cycle 8 ships only `reqwest` + `governor`; `reqwest_middleware` and
//! `reqwest_retry` land in cycle 9. Their needles are pre-armed here so
//! the cycle-9 author cannot accidentally leak them outside infra/.

use std::path::{Path, PathBuf};

fn walk(dir: &Path, hits: &mut Vec<(PathBuf, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            walk(&path, hits);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            let body = std::fs::read_to_string(&path).unwrap();
            for needle in [
                "use reqwest",
                "reqwest::",
                "use reqwest_middleware",
                "reqwest_middleware::",
                "use reqwest_retry",
                "reqwest_retry::",
                "use governor",
                "governor::",
            ] {
                if body.contains(needle) {
                    hits.push((path.clone(), needle.to_string()));
                }
            }
        }
    }
}

#[test]
fn reqwest_and_governor_only_appear_under_src_infra() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let infra = src.join("infra");
    let mut all_hits = Vec::new();
    walk(&src, &mut all_hits);

    let leaks: Vec<_> = all_hits
        .into_iter()
        .filter(|(path, _)| !path.starts_with(&infra))
        .collect();

    assert!(
        leaks.is_empty(),
        "criterion 20 (cycle-8 extension): reqwest/governor symbols leaked outside src/infra/: {leaks:#?}"
    );
}
