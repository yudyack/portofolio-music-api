//! In-memory cache for `/v1/*` upstream responses. Pure-stdlib, lives in
//! the app layer (no `sqlx`, no `reqwest`) — handlers compose it directly.
//!
//! Per spec §5.6 / criterion 11, each `/v1/*` endpoint caches its upstream
//! JSON for a TTL; a second request within the TTL must NOT call Spotify.
//! Per spec §5.6, Spotify content is **never persisted to SQLite** — this
//! cache is in-process only and dies on restart by design.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

struct Entry {
    value: Value,
    expires_at: Instant,
}

pub struct Cache {
    map: Mutex<HashMap<String, Entry>>,
}

impl Cache {
    pub fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
    }

    /// Fetch a value if present AND not expired. Expired entries stay in
    /// the map until overwritten by the next `put` — TTL eviction is lazy.
    pub fn get(&self, key: &str) -> Option<Value> {
        let map = self.map.lock().unwrap();
        let entry = map.get(key)?;
        if entry.expires_at > Instant::now() {
            Some(entry.value.clone())
        } else {
            None
        }
    }

    pub fn put(&self, key: String, value: Value, ttl: Duration) {
        let mut map = self.map.lock().unwrap();
        map.insert(
            key,
            Entry {
                value,
                expires_at: Instant::now() + ttl,
            },
        );
    }
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}
