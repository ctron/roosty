# Development

Run the local stack:

```sh
podman compose -f deploy/compose.yaml up --build
```

The local deployment also starts Elk, an external Mastodon-compatible web client:

```text
http://localhost:5314
```

Use `http://localhost:3000` as the instance URL when the client asks which server to connect to.

Run migrations against the compose database:

```sh
ROOST_DATABASE_URL=postgres://roost:roost@localhost:5432/roost cargo run -p roost -- migrate
```

Bootstrap the first administrator:

```sh
ROOST_DATABASE_URL=postgres://roost:roost@localhost:5432/roost cargo run -p roost -- admin bootstrap --username admin --email admin@example.com
```

The application listener defaults to port 3000. When `ROOST_INFRA_LISTEN_ADDR` is set, infrastructure endpoints are served only from that listener:

```text
http://localhost:3001/healthz
http://localhost:3001/readyz
http://localhost:3001/metrics
```

Elk stores local client settings in the `elk-data` compose volume. The backend does not serve, package, or embed Elk.
