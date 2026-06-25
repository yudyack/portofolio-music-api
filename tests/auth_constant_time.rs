//! Tripwire for spec criterion 23 (Basic-auth on /auth/spotify/login):
//! the password comparison MUST go through `subtle::ConstantTimeEq` to
//! avoid leaking the admin password via timing. Criterion 23 has no
//! implementation yet (lands in cycle 7+), so this file ships TWO
//! `#[ignore]`-gated red tests that the cycle-7 coder MUST unignore
//! when wiring the Basic-auth middleware. Forgetting to unignore is
//! the failure mode this guards against — see QA report.
//!
//! Architect's QA envelope (`.agent/inbox/qa/music-api--from-architect.md`)
//! flagged: `subtle = "2"` for constant-time Basic-auth compare; coder
//! must use `subtle::ConstantTimeEq` when criterion 23 lands.

use std::path::{Path, PathBuf};

fn cargo_toml() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .expect("read Cargo.toml")
}

fn src_files() -> Vec<(PathBuf, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = vec![];
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                let body = std::fs::read_to_string(&path).unwrap();
                out.push((path, body));
            }
        }
    }
    walk(&src, &mut out);
    out
}

#[test]
#[ignore = "Tripwire: unignore when cycle 7+ lands the Basic-auth handler. Cargo.toml must list `subtle` and src/ must import subtle::ConstantTimeEq."]
fn auth_basic_password_compare_uses_subtle_constant_time() {
    let manifest = cargo_toml();
    assert!(
        manifest.contains("subtle"),
        "Cargo.toml must declare `subtle` as a direct dependency for criterion 23 (Basic-auth constant-time compare)",
    );

    let src = src_files();
    let imports_constant_time = src.iter().any(|(_, body)| {
        body.contains("subtle::ConstantTimeEq")
            || body.contains("use subtle::")
            || body.contains("use subtle;")
    });
    assert!(
        imports_constant_time,
        "no src/ file imports subtle — Basic-auth credential compare must go through constant-time equality (criterion 23)",
    );
}

#[test]
#[ignore = "Tripwire: unignore when cycle 7+ lands the Basic-auth handler. Forbids naive string equality on the password field."]
fn auth_basic_password_does_not_use_string_equality() {
    let src = src_files();
    let forbidden_patterns = [
        "auth_basic_password ==",
        "auth_basic_password.eq(",
        "config.auth_basic_password ==",
        "config.auth_basic_password.eq(",
    ];
    for (path, body) in src {
        for forbidden in &forbidden_patterns {
            assert!(
                !body.contains(forbidden),
                "{} contains `{}` — Basic-auth must use subtle::ConstantTimeEq, not string equality (criterion 23)",
                path.display(),
                forbidden,
            );
        }
    }
}
