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

The compose command starts Roost with `serve --with-migrations --with-worker`, so database migrations run automatically before the server begins listening.

The local stack uses Caddy with an internal development certificate so Roost and Elk can run over HTTPS. Your browser may ask you to accept the local certificate.

This starts:

- Roost API server through Caddy: `https://roost.localhost:4000`
- Roost infrastructure endpoints: `http://localhost:3001`
- Elk web client through Caddy: `https://localhost:4001`
- PostgreSQL 18

To run migrations manually instead:

```sh
podman compose -f deploy/compose.yaml exec roost /usr/local/bin/roost migrate
```

Bootstrap the first administrator:

```sh
podman compose -f deploy/compose.yaml exec roost /usr/local/bin/roost admin bootstrap --username admin --email admin@example.com
```

Elk is preset to use the local Roost instance. If it asks for an instance URL, use:

```text
https://roost.localhost:4000
```

If Elk keeps trying an old saved instance, open this URL once to clear its local browser state:

```text
https://localhost:4001/reset
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
