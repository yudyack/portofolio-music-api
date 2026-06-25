//! Architect-flagged caveats survive cycle 7's extraction:
//!   1. SqliteConnectOptions in lib.rs::init() must set busy_timeout and
//!      journal_mode(WAL) so cycle 7+'s single-flight refresher doesn't
//!      hit SQLITE_BUSY against an OAuth callback upsert.
//!   2. Criterion 21 says migrations apply before the listener binds.
//!      Cycle 7 split this into two orderings:
//!        (a) within lib.rs: run_migrations() must precede AppState
//!            construction (so the repo never points at an unmigrated DB)
//!        (b) main.rs: init() must precede TcpListener::bind()
//!
//! These are static-source regression guards, not behavioral assertions.
//! main.rs and lib.rs::init() aren't unit-tested through their async fn
//! bodies — these catch the most likely regression: someone reordering
//! the lines during a refactor.

use std::path::Path;

fn read_src(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel))
        .unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

#[test]
fn lib_rs_configures_sqlite_busy_timeout() {
    let src = read_src("src/lib.rs");
    assert!(
        src.contains(".busy_timeout("),
        "src/lib.rs must configure SqliteConnectOptions::busy_timeout (architect cycle 0-6 refactor 2) — \
         without it cycle 7+'s single-flight refresher will hit SQLITE_BUSY"
    );
}

#[test]
fn lib_rs_configures_sqlite_wal_journal_mode() {
    let src = read_src("src/lib.rs");
    assert!(
        src.contains("SqliteJournalMode::Wal"),
        "src/lib.rs must set SqliteConnectOptions::journal_mode(SqliteJournalMode::Wal) \
         (architect cycle 0-6 refactor 2)"
    );
}

#[test]
fn lib_rs_runs_migrations_before_constructing_app_state() {
    // Within-init ordering (criterion 21, fine-grained). Catches a reorder
    // that moves run_migrations() AFTER `Arc::new(SqliteTokenRepository::new(...))`
    // — harmless today but conceptually a regression (repo would briefly
    // point at an unmigrated DB).
    let src = read_src("src/lib.rs");
    let migrate_pos = src
        .find("run_migrations(")
        .expect("lib.rs::init() must call run_migrations");
    let app_state_pos = src
        .find("AppState::new(")
        .expect("lib.rs::init() must call AppState::new");
    assert!(
        migrate_pos < app_state_pos,
        "criterion 21 (within lib.rs): run_migrations() at byte {migrate_pos} must precede AppState::new() at byte {app_state_pos}"
    );
}

#[test]
fn main_rs_runs_init_before_binding_listener() {
    // Cross-file ordering (criterion 21, coarse-grained). After cycle 7
    // extracted init() into lib.rs, main.rs delegates startup to it —
    // the bind call must still come after.
    let src = read_src("src/main.rs");
    let init_pos = src
        .find("music_api::init(")
        .expect("main.rs must call music_api::init");
    let bind_pos = src
        .find("TcpListener::bind(")
        .expect("main.rs must call TcpListener::bind");
    assert!(
        init_pos < bind_pos,
        "criterion 21 (main.rs): music_api::init() at byte {init_pos} must precede TcpListener::bind() at byte {bind_pos}"
    );
}

#[test]
fn main_rs_uses_eprintln_exit_pattern_for_bind_failure() {
    // Regression guard: bare `.expect("bind")` / `.expect("serve")` must
    // not return after architect cycle 0-6 refactor 3.
    let src = read_src("src/main.rs");
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
