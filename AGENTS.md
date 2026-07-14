# Repository Working Notes

## Project Target

- Roosty targets compatibility with the ActivityPub specification and Mastodon-compatible client APIs.
- The focus is on the backend, allowing integration with UIs
- APIs should align with the ActivityPub spec and Mastodon APIs.
- It must be possible that multiple instances of this process run on the same database.

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
- Prefer idiomatic, strongly typed Rust: model domain concepts with dedicated types, structs, and enums instead of
  stringly typed values or dynamically shaped data.
- Model closed `kind`, `type`, `state`, and similar discriminator fields as Rust enums. Convert them to strings only at
  persistence or wire-format boundaries.
- Prefer strongly typed Rust structs with Serde derives over manual JSON processing. Avoid using `serde_json::Value`
  unless the JSON shape is genuinely dynamic or unknown.
- Prefer importing types over repeatedly using fully qualified type paths, except where importing would make the code
  ambiguous or less clear.
- Avoid unnecessary cloning; prefer borrowing, moving, or restructuring ownership when it keeps the code clear.
- Prefer `?` with `From`/`Into` error conversions over manual error mapping; map errors explicitly only when adding
  useful context or translating an error at a boundary.
- Prefer file-backed Rust modules over nested inline modules. Use nested inline modules only when they are very small
  and local to their parent.
- Before adding a new dependency, check for its most recent version.

## Database Transactions

- Prefer database transactions for multi-step reads and writes that must observe or preserve a consistent state.

## Documentation

- Add concise rustdoc (`///` or `//!`) to non-trivial or reused Rust types, functions, and modules; trivial glue,
  accessors, and one-line helpers do not need it. Document the purpose, contract, or compatibility behavior rather
  than restating the name.
- For larger or non-obvious function bodies, add concise internal comments explaining the major steps or invariants.
- Add concise rustdoc to non-obvious tests describing the behavior or invariant protected, preferably in give, when,
  then style. Document `rstest` cases in their `#[case]` lines.
- Update `docs/roadmap.md` and `docs/compatibility.md` when adding, removing, or materially changing ActivityPub or
  Mastodon-compatible behavior.
- When adding Mastodon-compatible endpoints that accept `limit`, check the official API shape and implement the
  required cursor or offset pagination parameters and `Link` headers at the same time.

## Git

- Use Conventional Commits for commit messages.
