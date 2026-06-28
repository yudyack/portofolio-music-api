//! In-memory snapshot store for the scheduler-push data plane.
//!
//! Each `/v1/*` endpoint has one `ArcSwap<Option<Value>>` cell. The
//! per-endpoint scheduler tick fetches Spotify, maps into the spec §5.7
//! shape, and `store`s the new snapshot. Handlers `load()` it, return
//! 200 if `Some`, and fall back to a synchronous fetch when `None` (the
//! cold-start case before the first scheduler tick).
//!
//! Per spec §5.6, Spotify content is never persisted to disk — these
//! cells live only in process memory and die on restart by design.

use std::sync::Arc;

use arc_swap::ArcSwap;
use serde_json::Value;

/// Discriminator for the per-endpoint snapshot cell. Keeps the scheduler
/// dispatch + `Snapshots::set` symmetric and avoids stringly-typed keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointKind {
    Now,
    Recent,
    Top,
    Profile,
    Playlists,
}

/// One `ArcSwap` cell per `/v1/*` endpoint. Cloning is cheap (all fields
/// are `Arc`); the scheduler clones to its task and the handler clones
/// from `AppState`.
pub struct Snapshots {
    pub now: Arc<ArcSwap<Option<Value>>>,
    pub recent: Arc<ArcSwap<Option<Value>>>,
    pub top: Arc<ArcSwap<Option<Value>>>,
    pub profile: Arc<ArcSwap<Option<Value>>>,
    pub playlists: Arc<ArcSwap<Option<Value>>>,
}

impl Default for Snapshots {
    fn default() -> Self {
        Self {
            now: Arc::new(ArcSwap::from_pointee(None)),
            recent: Arc::new(ArcSwap::from_pointee(None)),
            top: Arc::new(ArcSwap::from_pointee(None)),
            profile: Arc::new(ArcSwap::from_pointee(None)),
            playlists: Arc::new(ArcSwap::from_pointee(None)),
        }
    }
}

impl Snapshots {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the cell for a given endpoint. Useful when a caller wants to
    /// hold the `Arc` (e.g. the scheduler closure) without owning the full
    /// `Snapshots`.
    pub fn cell(&self, kind: EndpointKind) -> Arc<ArcSwap<Option<Value>>> {
        match kind {
            EndpointKind::Now => self.now.clone(),
            EndpointKind::Recent => self.recent.clone(),
            EndpointKind::Top => self.top.clone(),
            EndpointKind::Profile => self.profile.clone(),
            EndpointKind::Playlists => self.playlists.clone(),
        }
    }

    /// Load the current snapshot for a given endpoint.
    pub fn get(&self, kind: EndpointKind) -> Option<Value> {
        self.cell(kind).load().as_ref().clone()
    }

    /// Store a new snapshot for a given endpoint. Passing `None` clears
    /// the cell (e.g. on a future degradation path that wants to force a
    /// re-fetch).
    pub fn set(&self, kind: EndpointKind, value: Option<Value>) {
        self.cell(kind).store(Arc::new(value));
    }
}
