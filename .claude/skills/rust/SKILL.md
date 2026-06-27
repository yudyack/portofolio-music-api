---
name: rust
description: Rust coding style for music-api. Apply when writing or reviewing Rust code in this repo.
allowed-tools: Read, Edit, Write, Bash, Grep, Glob
---

# Rust style — music-api

Mostly a lift from the [pgdog Rust skill](https://github.com/pgdogdev/pgdog/blob/main/.claude/skills/rust/SKILL.md), trimmed for this single-tenant axum service. Three sections are **LOCAL OVERRIDES** of upstream guidance because they conflict with the established convention in this repo — flagged inline.

## Core principles

1. **Leverage the type system** — make invalid states unrepresentable.
2. **Prefer compile-time checks** — catch errors before runtime.
3. **Be explicit about ownership** — don't fight the borrow checker.
4. **Pass `fmt`/`clippy` first, not after fixing.**

## Error handling — use `thiserror`

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum FetchError {
    #[error("upstream: {0}")]
    Upstream(String),
    #[error("needs reauth")]
    NeedsReauth,
}
```

### Never `.unwrap()` in `src/`

`.expect("with a reason")` is acceptable when `None` is genuinely impossible after construction; everything else uses `?` with a typed error.

`.unwrap()` in `tests/` is fine — assertions are the point.

## Ownership

### Prefer borrowing

```rust
// BAD
fn process(data: String) { ... }
process(my_string.clone());

// GOOD
fn process(data: &str) { ... }
process(&my_string);
```

### `impl Into<String>` for owned-string constructors

```rust
impl User {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}
```

## API design

### Newtype for type safety

```rust
// BAD - easy to mix up
fn transfer(from: i64, to: i64, amount: i64) { ... }

// GOOD - compile-time safety
pub struct AccountId(i64);
pub struct Amount(i64);
fn transfer(from: AccountId, to: AccountId, amount: Amount) { ... }
```

Used in this repo: `EndpointKind` (typed snapshot key), `AuthState` (boolean wrapped behind methods).

### Config: `from_lookup` over builder pattern  *(LOCAL OVERRIDE)*

For a single-tenant service, `Config::from_lookup` (env-var driven) is simpler than a builder. Reserve builders for genuinely complex objects with many optional fields and validation rules. None today.

### `pub(crate)` for internal APIs

Heavy usage in `src/app/scheduler.rs` and `src/infra/spotify_*` — public to the crate, hidden from external consumers. Use `pub` only when integration tests in `tests/` need access (e.g., `AppState::snapshots`, `AppState::activity`).

## Collections & iterators

Prefer iterator pipelines over manual loops:

```rust
// BAD
let mut results = Vec::new();
for item in items {
    if item.is_valid() {
        results.push(item.transform());
    }
}

// GOOD
let results: Vec<_> = items
    .into_iter()
    .filter(|item| item.is_valid())
    .map(|item| item.transform())
    .collect();
```

In-repo: `app/v1_mapper.rs` (artists/items mapping).

## Async patterns

### `tokio` is the runtime

```rust
#[tokio::main]
async fn main() -> Result<()> { ... }
```

### Never block in async code

```rust
// BAD - blocks the executor
async fn bad() {
    std::thread::sleep(Duration::from_secs(1));
}

// GOOD
async fn good() {
    tokio::time::sleep(Duration::from_secs(1)).await;
}

// CPU work: spawn_blocking
let result = tokio::task::spawn_blocking(|| heavy_compute()).await?;
```

## Testing  *(LOCAL OVERRIDE)*

Upstream pgdog says "unit tests in same file via `#[cfg(test)] mod tests`". **We default to `tests/` integration tests instead.**

Why: most code paths exercise `AppState` + an axum router + mock `SpotifyClient` + `AuthState` together. That's integration-shaped. Inline unit tests would have to fake all of it. `tests/` lets us share fixtures across files and test through the public surface.

Use inline `#[cfg(test)] mod tests` only when:
- The module is a tiny pure-function unit (no `AppState`, no axum).
- The functions under test are private to the module.
- Examples in-repo: [`src/app/state_store.rs`](../../../src/app/state_store.rs), [`src/infra/spotify_backoff.rs`](../../../src/infra/spotify_backoff.rs).

Integration test layout in `tests/`:
- One file per module-under-test or per behavior cluster.
- Filename mirrors the module: `tests/v1_now.rs` ↔ `routes::v1::now`.
- Use programmed test doubles (`CountingSpotify`, `MemRepo`) — not full HTTP wiremocks unless testing infra concerns.

Use `assert!`, `assert_eq!`, `assert!(result.is_ok())` directly.

## Module organization

Top-level layout in `src/`:

```
src/
├── lib.rs        — public crate surface + middleware factories
├── main.rs       — bootstrap (init, spawn schedulers, bind axum)
├── config.rs     — Config + ConfigError + from_lookup
├── state.rs      — AppState (shared by handlers + schedulers)
├── oauth.rs      — OAuth helpers (authorize URL, scopes)
├── app/          — application services (scheduler, snapshots, activity, …)
├── domain/       — pure traits + types (TokenRepository, SpotifyClient, …)
├── infra/        — IO impls (reqwest client, sqlx token repo, …)
└── routes/       — axum handlers
```

Layer rules:

- `domain/` depends on **nothing** outside `std` + serde — pure types/traits.
- `infra/` implements `domain/` traits; isolated from `app/` and `routes/`.
- `app/` orchestrates `domain/` traits — never reaches into `infra/`, `reqwest`, or `sqlx` directly.
- `routes/` is the only layer that touches axum extractors / responses.

This boundary is what makes the `infra/` swap (`SqliteTokenRepository` → hypothetical `PostgresTokenRepository`) a one-line wiring change. Don't break it.

## Documentation

Doc-comment public items at the crate / module / type level. Function-level docstrings only when the name + signature aren't self-evident.

```rust
/// Refreshed Spotify tokens returned by a token exchange.
///
/// `scope` may be absent when Spotify echoes nothing new.
pub struct RefreshedTokens { ... }
```

Avoid `# Examples` blocks that compile-test against private state — they break offline builds.

## Skip list  *(LOCAL: not adopted from pgdog)*

- `#[must_use]` — add only if a function's return is silently-droppable AND dropping would be a real bug.
- `Cow<'_, str>` — no current use case.
- Builder pattern — covered above.

## Quality gates (every commit)

```sh
cargo build
cargo fmt --check
cargo clippy --lib -- -D warnings
cargo test
```

`cargo clippy --all-targets -- -D warnings` may surface pre-existing test-only lints — verify by stashing your changes and re-running before attributing them to your commit.
