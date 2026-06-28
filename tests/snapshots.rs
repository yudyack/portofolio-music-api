//! Unit tests for `Snapshots`.
//!
//! Pins the small `Snapshots` API the scheduler tasks and `/v1/*`
//! handlers depend on:
//! - `get` on a fresh cell returns `None`.
//! - `set(Some)` then `get` returns the stored value.
//! - `set(None)` clears the cell back to `None`.
//! - Distinct cells are independent (writing `Now` does not bleed into
//!   `Recent`).
//! - Concurrent reader + writer don't tear or panic — ArcSwap is the
//!   primitive backing the cell precisely for this case.

use std::sync::Arc;

use music_api::app::snapshots::{EndpointKind, Snapshots};
use serde_json::json;

#[test]
fn fresh_cell_is_empty() {
    let s = Snapshots::new();
    for kind in [
        EndpointKind::Now,
        EndpointKind::Recent,
        EndpointKind::Top,
        EndpointKind::Profile,
        EndpointKind::Playlists,
    ] {
        assert!(s.get(kind).is_none(), "fresh cell {kind:?} must be None");
    }
}

#[test]
fn set_then_get_round_trips() {
    let s = Snapshots::new();
    let payload = json!({"playing": true, "track": "X"});
    s.set(EndpointKind::Now, Some(payload.clone()));
    assert_eq!(s.get(EndpointKind::Now), Some(payload));
}

#[test]
fn set_none_clears_cell() {
    let s = Snapshots::new();
    s.set(EndpointKind::Now, Some(json!({"x": 1})));
    s.set(EndpointKind::Now, None);
    assert_eq!(s.get(EndpointKind::Now), None);
}

#[test]
fn cells_are_independent() {
    let s = Snapshots::new();
    s.set(EndpointKind::Now, Some(json!({"who": "now"})));
    s.set(EndpointKind::Recent, Some(json!({"who": "recent"})));
    assert_eq!(s.get(EndpointKind::Now), Some(json!({"who": "now"})));
    assert_eq!(s.get(EndpointKind::Recent), Some(json!({"who": "recent"})));
    assert_eq!(s.get(EndpointKind::Top), None);
}

#[tokio::test]
async fn concurrent_reader_and_writer_do_not_tear() {
    // 10 concurrent readers + 10 writers hitting the same cell. Each
    // reader either sees None or one of the writer-stored payloads — never
    // a malformed value, never a panic.
    let s = Arc::new(Snapshots::new());

    let mut writers = Vec::new();
    for i in 0..10 {
        let s = s.clone();
        writers.push(tokio::spawn(async move {
            for _ in 0..50 {
                s.set(EndpointKind::Now, Some(json!({"writer": i})));
                tokio::task::yield_now().await;
            }
        }));
    }
    let mut readers = Vec::new();
    for _ in 0..10 {
        let s = s.clone();
        readers.push(tokio::spawn(async move {
            let mut last_seen: Option<i64> = None;
            for _ in 0..50 {
                if let Some(v) = s.get(EndpointKind::Now) {
                    let w = v
                        .get("writer")
                        .and_then(|x| x.as_i64())
                        .expect("writer key");
                    assert!((0..10).contains(&w), "writer id {w} out of range");
                    last_seen = Some(w);
                }
                tokio::task::yield_now().await;
            }
            last_seen
        }));
    }
    for w in writers {
        w.await.expect("writer panicked");
    }
    for r in readers {
        r.await.expect("reader panicked");
    }
}
