//! Domain layer. Pure types and traits — knows nothing about HTTP frameworks
//! or storage backends. Per spec §5.10 / criterion 20, no direct database
//! query calls live here; only the `TokenRepository` trait that infra
//! implements. Cycle 8 extends the rule symmetrically to outbound HTTP:
//! the `SpotifyClient` trait lives here; the reqwest/governor stack lives
//! under `infra`. The repository-pattern and reqwest-isolation static-grep
//! tests enforce this.
pub mod auth_state;
pub mod oauth_client;
pub mod spotify;
pub mod tokens;
