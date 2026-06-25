//! Domain layer. Pure types and traits — knows nothing about HTTP frameworks
//! or storage backends. Per spec §5.10 / criterion 20, no direct database
//! query calls live here; only the `TokenRepository` trait that infra
//! implements. The repository-pattern static-grep test enforces this.
pub mod tokens;
