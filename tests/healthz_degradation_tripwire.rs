//! Tripwire for spec criterion 15's degradation half. The cycle-8 `healthz`
//! handler hardcodes `status: "ok"` (it derives only `token_state` from
//! `AppState`); the cycle 0-6 QA report's coverage matrix flagged the
//! degradation path as architecturally absent. Cycle 7 added the
//! `AppState` seam but kept `status` placeholder pending cycle 10's
//! refresher / upstream probe. Cycle 10 owns the work of flipping
//! `status` to `"degraded"` (when upstream is unreachable but tokens
//! exist) or `"needs_reauth"` (when refresh fails with `invalid_grant`).
//!
//! This test is `#[ignore]`'d today. The cycle-10 coder unignores it
//! when wiring the refresher; the failing assertion forces the
//! `"degraded"` status string to appear in `src/`. Forgetting to
//! unignore is the failure mode this guards against — same recipe as
//! the existing `tests/auth_constant_time.rs` tripwires.
//!
//! Architect cycles 7-8 review caveat #3 flagged this — see
//! `.agent/architect/architect--music-api--review.md`.

use std::path::{Path, PathBuf};

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
#[ignore = "Tripwire: unignore when cycle 10 wires the refresher / upstream-probe degradation signal. src/ must reference the \"degraded\" status literal so healthz can emit it."]
fn healthz_emits_degraded_status_literal() {
    let files = src_files();
    let has_degraded_literal = files.iter().any(|(_, body)| body.contains("\"degraded\""));
    assert!(
        has_degraded_literal,
        "no src/ file references the literal \"degraded\" — healthz status is hardcoded \
         \"ok\" today; cycle 10's refresher must introduce the degradation path \
         (architect cycles 7-8 caveat #3, criterion 15 degradation half)"
    );
}

#[test]
#[ignore = "Tripwire: unignore when cycle 10 wires the needs_reauth path. src/ must reference the \"needs_reauth\" status literal so healthz can emit it."]
fn healthz_emits_needs_reauth_status_literal() {
    let files = src_files();
    let has_needs_reauth_literal = files
        .iter()
        .any(|(_, body)| body.contains("\"needs_reauth\""));
    assert!(
        has_needs_reauth_literal,
        "no src/ file references the literal \"needs_reauth\" — healthz status is hardcoded \
         \"ok\" today; cycle 10's refresher must introduce the needs_reauth path \
         (criterion 6, criterion 15 degradation half)"
    );
}
