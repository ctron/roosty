# Development

Run the local stack:

```sh
podman compose -f deploy/compose.yaml up --build
```

The local stack starts Roosty with `serve --with-migrations --with-worker`, so migrations run before the server begins listening.

The local stack uses Caddy with an internal development certificate so Roosty and Elk can run over HTTPS. Your browser may ask you to accept the local certificate.

The local deployment also starts Elk, an external Mastodon-compatible web client:

```text
https://localhost:4001
```

Elk is configured for Roosty as a single-instance client using `roosty.localhost:4000`.

If Elk keeps trying an old saved instance, open `https://localhost:4001/reset` once to clear its local browser state.

To smoke-test Elk's server-side login handoff to Roosty:

```sh
deploy/test-elk-login.sh
```

To run migrations manually instead:

```sh
podman compose -f deploy/compose.yaml exec roosty /usr/local/bin/roosty migrate
```

Bootstrap the first administrator:

```sh
podman compose -f deploy/compose.yaml exec roosty /usr/local/bin/roosty admin bootstrap --username admin --email admin@example.com
```

Reset a local user's password and print a temporary replacement:

```sh
podman compose -f deploy/compose.yaml exec roosty /usr/local/bin/roosty admin reset-password --username admin
```

The local application listener is exposed through Caddy on `https://roosty.localhost:4000`. When `ROOSTY_INFRA_LISTEN_ADDR` is set, infrastructure endpoints are served only from that listener:

```text
http://localhost:3001/healthz
http://localhost:3001/readyz
http://localhost:3001/metrics
```

Roosty stores uploaded media in the `roosty-media` compose volume. Elk stores local client settings in the `elk-data` compose volume. The backend does not serve, package, or embed Elk.
