# Roost

Roost is a standalone Rust federated social server targeting Mastodon-compatible clients and ActivityPub federation.

The project is early. The current local setup brings up the Rust server, PostgreSQL, infrastructure endpoints, and Elk as an external Mastodon-compatible UI for compatibility testing as API support is implemented.

## Local Development

Prerequisites:

- Rust 1.96+
- Podman with Compose support

Start the local stack:

```sh
podman compose -f deploy/compose.yaml up --build
```

This starts:

- Roost API server: `http://localhost:3000`
- Roost infrastructure endpoints: `http://localhost:3001`
- Elk web client: `http://localhost:5314`
- PostgreSQL 18

Run migrations against the local database:

```sh
ROOST_DATABASE_URL=postgres://roost:roost@localhost:5432/roost cargo run -p roost -- migrate
```

Bootstrap the first administrator:

```sh
ROOST_DATABASE_URL=postgres://roost:roost@localhost:5432/roost cargo run -p roost -- admin bootstrap --username admin --email admin@example.com
```

When Elk asks for an instance URL, use:

```text
http://localhost:3000
```

Useful local endpoints:

```text
http://localhost:3001/healthz
http://localhost:3001/readyz
http://localhost:3001/metrics
```

Stop the stack:

```sh
podman compose -f deploy/compose.yaml down
```

Remove local volumes, including PostgreSQL data and Elk client settings:

```sh
podman compose -f deploy/compose.yaml down -v
```

## Verification

After Rust code or manifest changes, run:

```sh
cargo fmt --all
cargo clippy --all-targets
```
