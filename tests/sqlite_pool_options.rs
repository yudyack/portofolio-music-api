//! Architect-flagged caveat (cycles 0-6 review):
//!   1. SqliteConnectOptions in main.rs must set busy_timeout and
//!      journal_mode(WAL) so cycle 7's single-flight refresher doesn't hit
//!      SQLITE_BUSY against an OAuth callback upsert.
//!   2. Criterion 21 says migrations apply before the listener binds. The
//!      coder-cycle test only proves migrations work on a fresh pool; this
//!      file adds a lightweight ordering guard.
//!
//! main.rs's async fn isn't unit-tested through its body — these are
//! static-source regression guards, not behavioral assertions. They catch
//! the most likely regression: a refactor that reorders the lines.

use std::path::Path;

fn read_main_rs() -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"))
        .expect("read src/main.rs")
}

#[test]
fn main_rs_configures_sqlite_busy_timeout() {
    let src = read_main_rs();
    assert!(
        src.contains(".busy_timeout("),
        "src/main.rs must configure SqliteConnectOptions::busy_timeout (architect cycle 0-6 refactor 2) — \
         without it cycle 7's single-flight refresher will hit SQLITE_BUSY"
    );
}

#[test]
fn main_rs_configures_sqlite_wal_journal_mode() {
    let src = read_main_rs();
    assert!(
        src.contains("SqliteJournalMode::Wal"),
        "src/main.rs must set SqliteConnectOptions::journal_mode(SqliteJournalMode::Wal) \
         (architect cycle 0-6 refactor 2) — without WAL, readers serialize against writers"
    );
}

#[test]
fn main_rs_runs_migrations_before_binding_listener() {
    // Criterion 21 ordering: migrations apply BEFORE the HTTP server binds.
    // A binary-spawning harness would be the true test (spec-evolution
    // ask #3); this byte-position check catches the cheap regression
    // (someone reordering the calls during a refactor).
    let src = read_main_rs();
    let migrate_pos = src
        .find("run_migrations(")
        .expect("main.rs must call run_migrations");
    let bind_pos = src
        .find("TcpListener::bind(")
        .expect("main.rs must call TcpListener::bind");
    assert!(
        migrate_pos < bind_pos,
        "criterion 21: run_migrations(...) at byte {migrate_pos} must precede TcpListener::bind(...) at byte {bind_pos}"
    );
}

#[test]
fn main_rs_uses_eprintln_exit_pattern_for_bind_failure() {
    // Architect cycle 0-6 refactor 3 replaced .expect("bind") / .expect("serve")
    // with the eprintln + exit(1) pattern used at every other startup-failure
    // site. Regression guard: bare `.expect("bind")` must not return.
    let src = read_main_rs();
    assert!(
        !src.contains(".expect(\"bind\")"),
        "src/main.rs must not use .expect(\"bind\") — replaced with \
         eprintln + std::process::exit(1) pattern in architect cycle 0-6 refactor"
    );
    assert!(
        !src.contains(".expect(\"serve\")"),
        "src/main.rs must not use .expect(\"serve\") — replaced with \
         eprintln + std::process::exit(1) pattern in architect cycle 0-6 refactor"
    );
}
