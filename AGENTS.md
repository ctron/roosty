# Repository Working Notes

## Verification

After making Rust code or manifest changes, run:

```sh
cargo fmt --all
cargo clippy --all-targets
```

Keep this as the default verification command for changes in this repository.

## Workspace Conventions

- Keep dependency versions in the root `Cargo.toml` under `[workspace.dependencies]`.
- Keep package metadata in the root `Cargo.toml` under `[workspace.package]`, including project version, Rust version, and license.
- Current license: `Apache-2.0`.
- Use SeaORM migrations as the canonical migration system from the start.
- Keep SQLx available for explicit query paths where direct SQL is the clearer fit.
- Prefer file-backed Rust modules over nested inline modules. Use nested inline modules only when they are very small and local to their parent.

## Documentation

- Document exported Rust functions, types, and modules with rustdoc comments (`///` or `//!`).
- Document all non-trivial Rust functions, even when private. Trivial glue, accessors, and one-line helpers do not need comments.
- For larger or non-obvious function bodies, add concise internal comments that explain the major steps or invariants.
- Document non-obvious tests with concise comments that state the compatibility behavior or invariant being protected.

## Git

- Use Conventional Commits for commit messages.
