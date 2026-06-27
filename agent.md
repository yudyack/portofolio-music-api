# Agent guide — music-api

This is a Rust crate that talks to Spotify on behalf of the portfolio frontend.
It is a **submodule** of `portofolio-services`. Read this before committing.

## Atomic commits

Each commit is one cohesive logical change that builds + tests green on its own.
A reviewer can land any prefix of the branch and the repo still works.

Split rules of thumb:

- **Deps** (`Cargo.toml` / `Cargo.lock`) get their own commit even if it is one line.
- **`cargo fmt`** noise on pre-existing files gets its own commit. Do not bury
  formatting churn inside a logic diff.
- **Additive primitives land before the cutover that uses them.** New module →
  `feat: add X primitives`. Wiring + deletion of the old path → `refactor: cut
  over to X`. This lets a reviewer read the new code in isolation before judging
  the replacement.
- **Spec changes** that describe code in this submodule live in the parent
  repo's `.agent/specifier/specifier--music-api--spec.md` and are bumped together
  there — never in this submodule's commits.

If you cannot split cleanly without fabricating an intermediate state, do not
fabricate it. Ship the big commit and explain why in the body.

## Commit message format

```
[<role>] <feature-name>: <one-line summary>

<body explaining the why and the key design choices — not the what,
which the diff already shows>

Co-Authored-By: <name> <email>
```

`<role>` is one of `specifier`, `coder`, `architect`, `qa` — matches the skill
that produced the change. `<feature-name>` groups related commits across a
refactor; pick once and reuse it for every commit on the same branch.

Examples from `git log --oneline`:

- `[specifier] music-api: lock decisions, unblock coder`
- `[coder] music-api-now: /v1/now handler with 204 → playing:false (criterion 17)`
- `[coder] music-api-scheduler: replace TTL cache with scheduler-push + sync first-fetch`

## Verification (every commit)

```sh
cargo build            # clean
cargo test             # all pass; pre-existing tripwires may stay ignored
cargo fmt --check      # clean
cargo clippy --lib -- -D warnings    # clean for the library
```

`cargo clippy --all-targets -- -D warnings` may surface pre-existing test-only
lints that are not your problem — verify by stashing your changes and re-running.

## Submodule etiquette

Workflow when shipping work that spans both repos:

1. Branch + atomic commits + push + PR in **this** repo first.
2. In the parent: branch, bump the submodule pointer to your new commit, add any
   spec changes under `.agent/specifier/`, open a companion PR. The parent
   commit message follows `<feature-name>: bump submodule + .agent artifacts`.
3. Land the music-api PR first (or in parallel), then the parent — so the
   submodule pointer in the parent resolves cleanly on `main`.

Do not edit `.agent/specifier/specifier--music-api--spec.md` from inside this
submodule. That file lives in the parent's tree.

## When in doubt

Look at the most recent merged PR for this repo or its parent. Mimic the
structure unless you have a reason to deviate.
