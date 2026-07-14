# Roosty

Roosty is a standalone Rust federated social server targeting Mastodon-compatible clients and ActivityPub federation.

The project is early. The current local setup brings up the Rust server, PostgreSQL, infrastructure endpoints, and Elk as an external Mastodon-compatible UI for compatibility testing as API support is implemented.

## Builds and Releases

Every commit pushed to `main` publishes a multi-architecture (`linux/amd64`, `linux/arm64`) container image to `ghcr.io/ctron/roosty`, tagged as both `main` and `sha-<commit>`. Pushing a Git tag creates a GitHub release only when the tag is either `<workspace-version>` or `v<workspace-version>` from the `roosty` Cargo package; the release includes an `x86_64-unknown-linux-gnu` binary archive and SHA-256 checksum.

## Local Development

Prerequisites:

- Rust 1.96+
- Podman with Compose support

Start the local stack:

```sh
podman compose -f deploy/compose.yaml up --build
```

The compose command starts Roosty with `serve --with-migrations --with-worker`, so database migrations run automatically before the server begins listening.

The local stack uses Caddy with an internal development certificate so Roosty and Elk can run over HTTPS. Your browser may ask you to accept the local certificate.

This starts:

- Roosty API server through Caddy: `https://roosty.localhost:4000`
- Roosty infrastructure endpoints: `http://localhost:3001`
- Elk web client through Caddy: `https://localhost:4001`
- Phanpy web client through Caddy: `https://localhost:4002`
- PostgreSQL 18

The local PostgreSQL development credentials are `roosty` for the role,
database, and password. Connect from the container with:

```sh
podman compose -f deploy/compose.yaml exec postgres psql -U roosty -d roosty
```

Fresh local volumes are initialized with these values. If you have a volume
created before the Roosty rename, migrate its role and database before starting
the application.

To run migrations manually instead:

```sh
podman compose -f deploy/compose.yaml exec roosty /usr/local/bin/roosty migrate
```

Bootstrap the first administrator:

```sh
podman compose -f deploy/compose.yaml exec roosty /usr/local/bin/roosty admin bootstrap --username admin --email admin@example.com
```

Create another local user:

```sh
podman compose -f deploy/compose.yaml exec roosty /usr/local/bin/roosty admin create-user --username alice --email alice@example.com
```

Create another local administrator:

```sh
podman compose -f deploy/compose.yaml exec roosty /usr/local/bin/roosty admin create-user --username moderator --email moderator@example.com --admin
```

Users can change their own password through the instance's account settings page:

```text
https://roosty.localhost:4000/auth/edit
```

An operator can also reset a local user's password and print a temporary replacement:

```sh
podman compose -f deploy/compose.yaml exec roosty /usr/local/bin/roosty admin reset-password --username alice
```

Elk is preset to use the local Roosty instance. If it asks for an instance URL, use:

```text
https://roosty.localhost:4000
```

If Elk keeps trying an old saved instance, open this URL once to clear its local browser state:

```text
https://localhost:4001/reset
```

Phanpy is served by the existing Caddy container, so it adds no separate local
service. Its pinned release is baked into that image at build time, rather than
being proxied through `phanpy.social`. Open it preselected for the local
instance with:

```text
https://localhost:4002/#/login?instance=roosty.localhost:4000
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

Remove local volumes, including PostgreSQL data, uploaded media, and Elk client settings:

```sh
podman compose -f deploy/compose.yaml down -v
```

## Production deployment

The Ansible deployment reuses the Roosty/PostgreSQL/media Compose topology with
the published Roosty image and Caddy-managed public TLS. It targets Debian/Ubuntu
hosts with APT package names for Docker Engine and the Compose v2 plugin.

Copy the example production inventory, point `roosty.example.com` at the
target, and run:

```sh
cd deploy/ansible
ansible-playbook site.yml
```

The included example deploys `https://roosty.example.com` with a Let's Encrypt
certificate. Set `roosty_acme_email` to an operator contact address, ensure the
host's public DNS A/AAAA records point to the target, and allow inbound ports 80
and 443 before Caddy obtains its certificate. Set
`roosty_federation_enabled: true` only after configuring its allow-list and key
encryption secret. The allow-list accepts exact domains or `"*"` to permit all
public domains; entries in `roosty_federation_blocked_domains` always take
precedence. HTTPS, public-DNS, redirect, timeout, and response-size checks still
apply in wildcard mode.

Elk is enabled by default at `https://elk.roosty.example.com`. Add a DNS A/AAAA
record for that subdomain before deployment. Set `roosty_elk_enabled: false` to
omit the Elk service and Caddy route; its persistent local browser state volume
is retained.

Set `roosty_phanpy_enabled: true` to make the existing Caddy container serve
Phanpy at `https://phanpy.roosty.example.com`. Add a DNS A/AAAA record for that
subdomain before deployment so Caddy can obtain its certificate. This remains
disabled by default and does not add another container. The Ansible role pins
the release with `roosty_phanpy_version`; override that role default in
inventory to upgrade it. Its root URL redirects to Phanpy's login route with
the Roosty base domain preselected.

## Verification

After Rust code or manifest changes, run:

```sh
cargo fmt --all
cargo clippy --all-targets
```

## Federation

Federation is disabled by default. To expose local ActivityPub identities, set
`ROOSTY_FEDERATION_ENABLED=true`, use an absolute HTTPS `ROOSTY_PUBLIC_BASE_URL`,
and provide a distinct `ROOSTY_FEDERATION_KEY_ENCRYPTION_SECRET` of at least 32
bytes. Roosty uses this secret to encrypt per-account signing keys at rest. Set
`ROOSTY_FEDERATION_ALLOWED_DOMAINS` to a comma-separated exact list of remote
DNS domains permitted for discovery; `ROOSTY_FEDERATION_BLOCKED_DOMAINS` can
exclude domains from that list. Configure `ROOSTY_FEDERATION_DELIVERY_MAX_AGE`
with a human-readable duration such as `7d`, `12h`, or `30m`; failed delivery
jobs retry with exponential backoff until this age is exceeded.

This surface provides WebFinger, local actor documents, public Notes, outboxes,
follower/following collections, policy-controlled remote `resolve=true` lookup,
signed inbox processing, and signed follow delivery. With a public HTTPS base
URL and an allow-list containing the follower's domain (or `*`), a
Mastodon-compatible account can follow `@user@your-domain`; Roosty verifies the
Follow, queues an Accept response, and delivers later public/unlisted posts.

### Federation subscription readiness

Before inviting remote followers, verify that the public base domain has DNS
and a valid HTTPS certificate, Roosty (rather than a web client) serves the
base domain, and the worker is healthy. For a local account named `admin`:

```sh
curl --fail --header 'Accept: application/jrd+json' \
  'https://your-domain/.well-known/webfinger?resource=acct%3Aadmin%40your-domain'
curl --fail --header 'Accept: application/activity+json' \
  https://your-domain/users/admin
```

Both requests must return `200`; the WebFinger self link must name the actor
URL from the second request. To allow followers from any public instance, set
`ROOSTY_FEDERATION_ALLOWED_DOMAINS=*`. Explicit blocked domains still take
precedence. Keep the federation key-encryption secret and PostgreSQL data
persistent across deployments so Roosty can continue signing Accept and status
delivery activities.
