//! HTTP route handlers. `auth` holds the owner OAuth bootstrap
//! (`/auth/spotify/login` + `/callback`); `admin` holds owner-only
//! control endpoints (today: the Spotify kill switch); `v1` holds the
//! anonymous `/v1/*` data-plane consumed by the leptos frontend.

pub mod admin;
pub mod auth;
pub mod v1;
