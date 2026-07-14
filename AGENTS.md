# Repository Working Notes

## Project Target

- Roosty targets compatibility with the ActivityPub specification and Mastodon-compatible client APIs.
- The focus is on the backend, allowing integration with UIs
- APIs should align with the ActivityPub spec and Mastodon APIs.

## Verification

After making Rust code or manifest changes, run:

```sh
cargo fmt --all
cargo clippy --all-targets
cargo test
```

Keep this as the default verification command for changes in this repository.

## Workspace Conventions

- Keep dependency versions in the root `Cargo.toml` under `[workspace.dependencies]`.
- Reference workspace dependencies in crate manifests as `dependency = { workspace = true }`.
- Keep package metadata in the root `Cargo.toml` under `[workspace.package]`, including project version, Rust version,
  and license.
- Current license: `Apache-2.0`.
- Use SeaORM migrations as the canonical migration system from the start.
- Prefer SeaORM entities and query builders for database reads and writes. Use raw SQL only when it is materially clearer
  or required for a database-specific operation such as row locking, partial-index conflict inference, or a complex CTE.
- Model closed `kind`, `type`, `state`, and similar discriminator fields as Rust enums. Convert them to strings only at
  persistence or wire-format boundaries.
- Prefer file-backed Rust modules over nested inline modules. Use nested inline modules only when they are very small
  and local to their parent.
- Before adding a new dependency, check for its most recent version.

## Documentation

- Document all non-trivial Rust functions, types, and modules with rustdoc comments (`///` or `//!`). Trivial glue,
  accessors, and one-line helpers do not need comments.
- For larger or non-obvious function bodies, add concise internal comments that explain the major steps or invariants.
- Document non-obvious tests with concise comments that state the compatibility behavior or invariant being protected
  using Rust doc on the function. Also document `rstest` cases in their `#[case]` lines.
- Document non-obvious tests in a give, when, then style.
- Update `docs/roadmap.md` and `docs/compatibility.md` when adding, removing, or materially changing ActivityPub or
  Mastodon-compatible behavior.
- When adding Mastodon-compatible endpoints that accept `limit`, check the official API shape and implement the
  required cursor or offset pagination parameters and `Link` headers at the same time.

## Git

- Use Conventional Commits for commit messages.
