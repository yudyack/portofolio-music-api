//! HTTP route handlers. `auth` holds the owner OAuth bootstrap
//! (`/auth/spotify/login` + `/callback`); `v1` holds the anonymous
//! `/v1/*` data-plane consumed by the leptos frontend.

pub mod auth;
pub mod v1;
