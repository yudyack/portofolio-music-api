//! Application-service layer. Orchestrates the domain traits
//! (`TokenRepository`, `SpotifyClient`, `TokenExchanger`) and the shared
//! `AuthState` into the behaviors the HTTP handlers consume. Depends ONLY
//! on domain traits — no `sqlx`, no `reqwest` (criterion 20 / reqwest
//! isolation), so a storage or HTTP-client swap never reaches this layer.

pub mod activity;
pub mod scheduler;
pub mod snapshots;
pub mod spotify_service;
pub mod state_store;
pub mod v1_mapper;
