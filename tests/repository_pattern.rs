//! Criterion 20 static-grep: `sqlx::query` and `use sqlx::query` may only
//! appear under `src/infra/`. Domain and app-service modules import only the
//! `TokenRepository` trait. The architect runs the same scan; this test
//! catches drift in CI.

use std::path::{Path, PathBuf};

fn walk(dir: &Path, hits: &mut Vec<(PathBuf, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            walk(&path, hits);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            let body = std::fs::read_to_string(&path).unwrap();
            for needle in ["sqlx::query", "use sqlx::query"] {
                if body.contains(needle) {
                    hits.push((path.clone(), needle.to_string()));
                }
            }
        }
    }
}

#[test]
fn sqlx_query_only_appears_under_src_infra() {
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
        "criterion 20: sqlx::query* leaked outside src/infra/: {leaks:#?}"
    );
}
