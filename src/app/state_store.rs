//! Server-side store of live OAuth `state` values (CSRF). `/login` issues
//! one; `/callback` consumes it. Single-use, TTL-expired. In-memory — a
//! restart drops pending logins, which is fine (states live ≤10 min).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rand::Rng;

const DEFAULT_TTL: Duration = Duration::from_secs(600); // 10 min (criterion 1)

pub struct StateStore {
    ttl: Duration,
    entries: Mutex<HashMap<String, Instant>>, // state -> expiry
}

impl Default for StateStore {
    fn default() -> Self {
        Self::with_ttl(DEFAULT_TTL)
    }
}

impl StateStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Issue a fresh random state and record its expiry.
    pub fn issue(&self) -> String {
        let state = random_state();
        self.entries
            .lock()
            .unwrap()
            .insert(state.clone(), Instant::now() + self.ttl);
        state
    }

    /// True iff `state` was a live (unexpired) issuance. Single-use: the
    /// entry is removed whether or not it had expired.
    pub fn consume(&self, state: &str) -> bool {
        match self.entries.lock().unwrap().remove(state) {
            Some(expiry) => Instant::now() < expiry,
            None => false,
        }
    }
}

fn random_state() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_state_consumes_true_once_then_false() {
        let store = StateStore::new();
        let s = store.issue();
        assert!(store.consume(&s), "first consume of a live state is true");
        assert!(!store.consume(&s), "single-use: second consume is false");
    }

    #[test]
    fn unknown_state_is_false() {
        assert!(!StateStore::new().consume("never-issued"));
    }

    #[test]
    fn expired_state_is_rejected() {
        let store = StateStore::with_ttl(Duration::ZERO);
        let s = store.issue();
        assert!(!store.consume(&s), "a zero-TTL state is already expired");
    }

    #[test]
    fn issued_states_are_unique() {
        let store = StateStore::new();
        assert_ne!(store.issue(), store.issue());
    }
}
