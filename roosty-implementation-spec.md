# Roosty Implementation Specification

## Purpose

Build a standalone federated social server in Rust.

The project must be able to run its own Mastodon-compatible instance from scratch, with no dependency on Mastodon's Ruby, Rails, Sidekiq, Redis, Node.js, database schema, or deployment stack.

The initial browser clients to support are:

- Phanpy
- Elk

They are external clients. Do not embed, fork, package, or serve either frontend as part of this project.

The server should expose enough Mastodon-compatible APIs, OAuth behavior, streaming behavior, and federation behavior for those clients to use the instance.

---

## Core principles

1. **Standalone implementation**
   - Own Rust codebase.
   - Own PostgreSQL schema and migrations.
   - No reuse of Mastodon's Rails schema or runtime.
   - No Ruby code at runtime or build time.

2. **Protocol and API compatibility**
   - Target Mastodon-compatible REST APIs, OAuth 2.0 behavior, streaming APIs, WebFinger, NodeInfo, and ActivityPub.
   - Do not reproduce Mastodon's internal implementation details.

3. **Small operational footprint**
   - PostgreSQL is required.
   - Redis is not required initially.
   - Use Postgres for durable jobs.
   - One binary can run both HTTP server and worker for small deployments.

4. **Durability first**
   - Federation delivery, inbound processing, media processing, notifications, and timeline fan-out must be durable and retryable.
   - Do not rely on detached in-process tasks that disappear on restart.

5. **Explicit compatibility**
   - Treat Phanpy and Elk behavior as integration tests.
   - A client failure should generally be investigated as an API or protocol compatibility issue.

---

## Non-goals for the first implementation

Do not implement these before the core local-instance and federation flows work:

- Mastodon Ruby/Rails compatibility
- Mastodon database import support
- Sidekiq compatibility
- Redis requirement
- Elasticsearch/OpenSearch requirement
- Kafka, NATS, or other message brokers
- Kubernetes-only deployment
- Multi-region deployment
- Full Mastodon endpoint parity
- Full Mastodon web UI compatibility
- JWT access tokens
- OIDC provider support
- Full moderation/admin feature parity
- Full-text search and trends
- Push notifications
- Account migration/import/export
- Polls, lists, scheduled posts, quote posts, edits, custom emoji, and advanced filters

---

## Target stack

| Area | Choice |
|---|---|
| Language | Rust |
| Async runtime | Tokio |
| CLI | Clap |
| HTTP framework | Axum |
| Middleware | Tower |
| Database | PostgreSQL |
| Database access | SQLx for explicit queries; SeaORM may be used for simple CRUD where useful |
| Migrations | SQLx migrations |
| Background jobs | PostgreSQL-backed durable queue |
| OAuth 2 authorization server | `oxide-auth` plus project-owned Postgres persistence and compatibility logic |
| Browser sessions | `tower-sessions` or equivalent |
| Password hashing | `argon2` |
| Optional external OIDC login | `openidconnect`, later only |
| HTTP client | `reqwest` with Rustls |
| JSON | `serde`, `serde_json` |
| Streaming | Axum WebSockets; SSE when useful |
| Object storage | `aws-sdk-s3`; local filesystem implementation for development |
| Media processing | `ffmpeg` subprocesses run by jobs |
| Templates | Askama for login, consent, bootstrap, and simple server-owned pages |
| Logging/tracing | `tracing` |
| Metrics | Prometheus-compatible metrics |
| Telemetry | OpenTelemetry, later if useful |
| Deployment | OCI container image; Podman Compose for development; OpenShift/Kubernetes later |

### Dependency direction

```text
server
  ├── api
  ├── oauth
  ├── accounts
  ├── posts
  ├── timelines
  ├── federation
  ├── streaming
  ├── media
  ├── jobs
  └── db

all domain crates
  └── core
```

Avoid circular dependencies. HTTP handlers should call domain services rather than contain database or federation rules directly.

---

## Repository layout

Use a Cargo workspace.

```text
roosty/
├── Cargo.toml
├── Cargo.lock
├── crates/
│   ├── core/                # domain identifiers, shared errors, common types
│   ├── db/                  # SQLx migrations, query modules, transaction helpers
│   ├── accounts/            # local users, remote actors, profiles, sessions
│   ├── oauth/               # OAuth apps, authorization codes, PKCE, tokens, scopes
│   ├── posts/               # statuses, replies, favourites, boosts, visibility
│   ├── timelines/           # public/home/tag timelines and fan-out rules
│   ├── federation/          # ActivityPub, WebFinger, NodeInfo, signatures, delivery
│   ├── streaming/           # WebSocket/SSE subscriptions and event delivery
│   ├── media/               # uploads, storage, variants, ffmpeg jobs
│   ├── jobs/                # durable job queue and workers
│   ├── api/                 # Mastodon REST DTOs, handlers, compatibility behavior
│   └── server/              # axum application, routes, configuration, main binary
├── migrations/
├── tests/
│   ├── api/
│   ├── federation/
│   ├── integration/
│   └── fixtures/
├── deploy/
│   ├── compose.yaml
│   ├── Containerfile
│   └── openshift/
├── docs/
└── xtask/                   # optional developer automation
```

Start with fewer crates if needed. Avoid premature crate fragmentation. A practical first pass is:

```text
crates/
  core/
  db/
  server/
```

Split domain crates when boundaries become clear.

---

## CLI

Use Clap.

Initial command shape:

```text
roosty serve
roosty serve --with-worker
roosty worker
roosty migrate
roosty admin bootstrap --username alice --email alice@example.com
```

Suggested CLI structure:

```rust
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "roosty")]
#[command(about = "Standalone Rust ActivityPub server")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the HTTP server.
    Serve {
        /// Run durable background jobs in the same process.
        #[arg(long)]
        with_worker: bool,

        #[arg(long, default_value = "0.0.0.0:3000")]
        listen: std::net::SocketAddr,
    },

    /// Run only durable background jobs.
    Worker,

    /// Run database migrations.
    Migrate,

    /// Administrative commands.
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum AdminCommand {
    /// Create the initial local administrator account.
    Bootstrap {
        #[arg(long)]
        username: String,

        #[arg(long)]
        email: String,
    },
}
```

### Combined server and worker mode

For initial deployments, support:

```text
roosty serve --with-worker
```

This starts:

- HTTP and API server
- browser login and OAuth routes
- WebSocket/SSE streaming
- in-process durable job worker

Use the same database pool and shared application configuration.

The worker must still operate through the durable Postgres job table. It must not use non-durable `tokio::spawn` work as its only mechanism.

Later, allow independent scaling:

```text
roosty serve
roosty worker
```

---

## Runtime architecture

```text
External clients
  ├── Phanpy
  ├── Elk
  └── other Mastodon-compatible clients
          │
          │ OAuth 2 + PKCE, REST, WebSocket/SSE
          ▼
Rust server
  ├── Axum HTTP routes
  ├── Mastodon-compatible API
  ├── OAuth authorization server
  ├── browser login/session pages
  ├── streaming service
  ├── ActivityPub endpoints
  └── optional in-process worker
          │
          ├── PostgreSQL
          ├── local filesystem or S3-compatible object storage
          ├── SMTP service
          └── remote ActivityPub instances
```

---

## Configuration

Configuration should come from:

1. Environment variables
2. Optional config file
3. CLI arguments where appropriate

Do not require configuration compiled into the binary.

Initial configuration needs:

```text
DATABASE_URL
PUBLIC_BASE_URL
LISTEN_ADDR
SESSION_SECRET
TOKEN_PEPPER
OBJECT_STORAGE_BACKEND
MEDIA_ROOT
S3_BUCKET
S3_REGION
S3_ENDPOINT
SMTP_URL
REGISTRATION_MODE
FEDERATION_ENABLED
INSTANCE_NAME
INSTANCE_DESCRIPTION
```

Validate configuration at startup.

`PUBLIC_BASE_URL` must be canonical and stable. It determines public account, post, ActivityPub actor, and object identifiers.

---

## Database and ID model

Use PostgreSQL as the source of truth.

Use project-owned migrations and schema. Do not mirror Mastodon's schema.

Internal identifiers can be UUIDs or sortable UUIDs. API IDs should be opaque strings. ActivityPub identifiers must be stable public URLs.

Initial conceptual tables:

```text
instance_config
local_account
remote_actor
actor_key
oauth_application
oauth_authorization_code
oauth_access_token
oauth_refresh_token
oauth_consent
status
status_attachment
status_mention
status_tag
favourite
boost
follow
block
mute
notification
timeline_entry
inbound_activity
outbound_delivery
domain_policy
media_attachment
job
job_attempt
```

### Token storage

Access and refresh tokens should be opaque random values.

Never store raw bearer tokens in PostgreSQL.

Store:

```text
token_hash
account_id
application_id
scopes
issued_at
expires_at
revoked_at
```

Use a server-side pepper in addition to a hash where appropriate.

Do not use JWT access tokens initially.

---

## Durable job system

Use a Postgres-backed queue.

Minimum job table fields:

```sql
create table job (
    id uuid primary key,
    kind text not null,
    payload jsonb not null,
    deduplication_key text,
    run_after timestamptz not null default now(),
    attempts integer not null default 0,
    locked_at timestamptz,
    locked_by text,
    last_error text,
    created_at timestamptz not null default now(),
    completed_at timestamptz
);

create unique index job_deduplication_key_idx
    on job (kind, deduplication_key)
    where deduplication_key is not null and completed_at is null;
```

Claim jobs with `FOR UPDATE SKIP LOCKED`.

Requirements:

- Jobs must have retry/backoff behavior.
- Jobs must be idempotent.
- Job claims must expire or be recoverable after process crashes.
- Failures must be inspectable.
- The worker must support graceful shutdown: stop claiming new work, finish or safely release currently claimed jobs.

Initial job kinds:

```text
deliver_activity
process_inbound_activity
fanout_status
process_media
send_notification
```

### Transactional outbox pattern

When a user creates a local status:

1. Insert the status.
2. Insert required timeline/fan-out/delivery jobs in the same database transaction.
3. Commit.
4. Return a successful API response.

Do not commit a status then attempt to enqueue a delivery job separately.

---

## Authentication and OAuth 2

The server must provide a Mastodon-compatible OAuth 2 authorization server.

It does not need to be an OIDC provider.

### Required routes

```text
POST /api/v1/apps
GET  /oauth/authorize
POST /oauth/token
POST /oauth/revoke
```

### Initial supported flows

Implement:

- Authorization Code grant
- PKCE, especially for public/browser clients
- Refresh tokens
- Token revocation
- Mastodon-compatible scopes

Do not implement implicit flow.

### Browser authentication

The Rust server owns browser login and session behavior.

Initial server-owned HTML routes:

```text
GET  /login
POST /login
POST /logout
GET  /oauth/authorize
POST /oauth/authorize
GET  /admin/bootstrap
POST /admin/bootstrap
```

Use Askama or equivalent templates.

Future routes include:

```text
/register
/password/reset
/password/change
/email/confirm
/settings/security
```

### OAuth requirements

- Authorization codes are short-lived and one-time use.
- Require and validate `state`.
- Require PKCE for public clients.
- Validate redirect URIs strictly.
- Store scopes on tokens.
- Support bearer token validation for API routes.
- Do not use permissive CORS for browser-cookie authenticated routes.
- External clients should use bearer tokens, not browser session cookies.

---

## Initial API scope

The initial API target is enough for Phanpy and Elk to authenticate and use a basic local instance.

### Discovery

```text
GET /.well-known/nodeinfo
GET /nodeinfo/2.1
GET /api/v2/instance
GET /api/v1/instance
```

### OAuth

```text
POST /api/v1/apps
GET  /oauth/authorize
POST /oauth/token
POST /oauth/revoke
```

### Account/session

```text
GET   /api/v1/accounts/verify_credentials
PATCH /api/v1/accounts/update_credentials
GET   /api/v1/preferences
GET   /api/v1/accounts/:id
```

### Timelines

```text
GET /api/v1/timelines/public
GET /api/v1/timelines/home
GET /api/v1/timelines/tag/:tag
```

### Statuses

```text
POST /api/v1/statuses
GET  /api/v1/statuses/:id
POST /api/v1/statuses/:id/favourite
POST /api/v1/statuses/:id/unfavourite
POST /api/v1/statuses/:id/reblog
POST /api/v1/statuses/:id/unreblog
POST /api/v1/statuses/:id/bookmark
POST /api/v1/statuses/:id/unbookmark
DELETE /api/v1/statuses/:id
```

### Social graph

```text
POST /api/v1/accounts/:id/follow
POST /api/v1/accounts/:id/unfollow
POST /api/v1/accounts/:id/mute
POST /api/v1/accounts/:id/unmute
POST /api/v1/accounts/:id/block
POST /api/v1/accounts/:id/unblock
```

### Notifications and media

```text
GET  /api/v1/notifications
POST /api/v2/media
GET  /api/v1/custom_emojis
```

### Streaming

```text
GET /api/v1/streaming
```

Implement WebSocket first if it is sufficient for Phanpy and Elk. Add SSE where it materially improves compatibility.

### API response compatibility

API response DTOs should be deliberately versioned and tested.

Pay attention to:

- Field names
- Nullability
- Omitted versus empty fields
- Pagination headers and cursor behavior
- Error response shape
- Scope errors
- Rate-limit headers
- ID serialization
- Timestamps
- HTML content and sanitized markup
- Visibility semantics

---

## Local instance MVP behavior

The first runnable standalone instance should support:

- Initial admin bootstrap
- Local password login
- OAuth app registration and authorization
- Local profiles
- Public timeline
- Home timeline
- Text posts
- Replies
- Boosts
- Favourites
- Follows
- Blocks and mutes
- Simple media uploads
- Notifications
- Basic account settings
- Basic moderation action: delete local status and suspend local account

Federation may be disabled by default initially, but the system should be designed so it can be enabled without changing the core data model.

---

## Timelines

Start simple.

### Public timeline

Use indexed Postgres queries over visible local and remote public statuses.

### Home timeline

Use a hybrid design eventually, but start with a simple and correct implementation.

Initial acceptable approach:

- Insert timeline entries for followers on write.
- Use lightweight `timeline_entry` records referencing status IDs.
- Do not copy status bodies per follower.

Later:

- Fan-out-on-read for high-follower accounts.
- Per-account fan-out thresholds.
- Rebuild jobs and repair tooling.

---

## Federation

ActivityPub support must be treated as a separate, defensive subsystem.

### Required discovery and actor routes

```text
GET /.well-known/webfinger
GET /users/:username
GET /users/:username/outbox
POST /users/:username/inbox
```

Add NodeInfo as described in the API scope.

### Inbound processing

The inbox request handler should:

1. Enforce method/body size/request limits.
2. Validate signature and request metadata.
3. Normalize and persist the raw activity.
4. Deduplicate by stable activity identity.
5. Insert a `process_inbound_activity` job.
6. Return promptly, normally `202 Accepted`.

Do not synchronously fetch remote actors, remote objects, media, or followers from the inbox request handler.

### Outbound processing

When local activity needs federation:

1. Build the ActivityPub activity.
2. Insert `deliver_activity` jobs transactionally.
3. Worker signs HTTP requests.
4. Worker delivers to remote inboxes.
5. Worker retries with backoff.
6. Worker records permanent failure state.

### Security requirements

Federation is untrusted input.

Implement early:

- SSRF prevention
- DNS/IP filtering for private/reserved networks
- Redirect controls
- Response-size limits
- Request timeouts
- Domain concurrency limits
- Per-domain backoff/circuit breaker behavior
- Strict media fetch limits
- Signature verification
- JSON parsing limits
- Activity deduplication
- Domain block/allow policies

### Initial federation milestones

1. WebFinger and actor document.
2. Actor key publication.
3. Outbound `Create` for local public status.
4. Signed outbound delivery.
5. Remote actor fetch/cache.
6. Inbound signed `Create`.
7. Follow/Accept/Undo basics.
8. Delete and Update handling.
9. Moderation/domain policy enforcement.

---

## Media

Initial requirements:

- Upload endpoint
- File-size limits
- MIME type validation
- Local filesystem backend
- S3-compatible backend
- Image thumbnail/variant generation
- Asynchronous video/audio handling through `ffmpeg`
- Explicit processing state in API responses

Do not run expensive media processing inside upload request handlers.

Use media jobs such as:

```text
process_media
delete_media
fetch_remote_media
```

---

## Streaming

The streaming subsystem delivers events after database state commits.

Initial channels:

```text
public
public:local
user
user:notification
hashtag:<tag>
```

Initial events:

```text
update
notification
delete
```

Requirements:

- Authenticate user streams with OAuth bearer tokens.
- Enforce authorization before subscribing.
- Handle slow clients and bounded buffers.
- Do not let a slow client block a transaction or job worker.
- Begin with in-process pub/sub for combined mode.
- Define an abstraction so later multi-process operation can use Postgres notifications, Redis, or another transport.

---

## HTTP routing and server-owned HTML

The server does not embed Phanpy or Elk.

It should serve only its own small HTML surface:

```text
/login
/oauth/authorize
/admin/bootstrap
/healthz
/readyz
/metrics
```

Potential future public HTML pages:

```text
/@username
/@username/:status_id
/about
/terms
/privacy
```

Do not use a broad SPA fallback. Reserve and explicitly route:

```text
/api
/oauth
/.well-known
/nodeinfo
/users
/media
/streaming
/healthz
/readyz
/metrics
```

---

## Security baseline

Implement these early:

- Argon2 password hashing
- Strong session cookie settings
- CSRF protection for browser form endpoints
- Rate limiting for login, OAuth, posting, and federation routes
- Password reset tokens with expiry, later
- Secure random token generation
- Opaque access tokens stored hashed
- Authorization checks in every protected API route
- Content sanitization for HTML status bodies
- Upload limits and MIME validation
- SSRF protections for federation/media
- Audit logging for admin/security-sensitive actions
- Secret validation at startup
- Security headers for server-rendered pages

---

## Observability

Use structured `tracing` from the beginning.

Metrics to expose:

```text
http_requests_total
http_request_duration_seconds
job_claimed_total
job_completed_total
job_failed_total
job_retry_total
federation_delivery_total
federation_delivery_failures_total
federation_inbound_total
streaming_connections
streaming_messages_total
database_pool_connections
media_processing_duration_seconds
```

Log fields should include correlation IDs where possible:

```text
request_id
account_id
status_id
job_id
remote_domain
activity_id
oauth_application_id
```

Provide:

```text
GET /healthz
GET /readyz
GET /metrics
```

---

## Testing strategy

### Unit tests

Cover:

- Scope parsing
- PKCE verification
- Token hashing and validation
- Visibility rules
- Follow/block/mute behavior
- Timeline ordering
- Job retry rules
- HTTP signature construction/verification
- Activity parsing
- URL/SSRF validation
- HTML sanitization

### Database integration tests

Use an isolated PostgreSQL database.

Cover:

- Migrations
- Concurrent job claiming
- Transactional outbox behavior
- Idempotency constraints
- Token revocation
- Timeline queries
- Visibility and authorization queries

### API compatibility tests

Store JSON fixtures for important Mastodon-compatible responses.

Test:

- Success responses
- Pagination
- Error bodies
- OAuth error responses
- Nullability and field behavior

### Client integration tests

Automate the following against a disposable local instance:

1. Register an OAuth app.
2. Log in through authorization code + PKCE.
3. Fetch `verify_credentials`.
4. Fetch public/home timeline.
5. Create a status.
6. Favourite and boost it.
7. Receive a streaming event.
8. Upload a media attachment.

Run manual compatibility checks with:

- Phanpy
- Elk

### Federation interop tests

Use disposable test instances where practical.

Test:

- Local actor discoverability
- WebFinger
- Signed `Create`
- Inbound signed `Create`
- Follow/Accept
- Undo
- Delete
- Retry behavior for failed delivery

### Replay tooling

Add admin tooling eventually:

```text
roosty admin replay-activity <id>
roosty admin redeliver <id>
roosty admin inspect-job <id>
```

---

## Deployment

### Development

Provide Podman Compose configuration:

```text
services:
  roosty:
    build: .
    command: ["serve", "--with-worker"]
    environment:
      DATABASE_URL: postgres://...
      PUBLIC_BASE_URL: http://localhost:3000
    ports:
      - "3000:3000"
    depends_on:
      - postgres

  postgres:
    image: postgres:17
```

Use local filesystem media storage by default in development.

### Production

Initial production deployment can still be one process:

```text
roosty serve --with-worker
```

Later split:

```text
roosty serve
roosty worker
```

Keep all processes stateless except for:

- PostgreSQL
- object storage
- configured persistent secrets

---

## Implementation phases

### Phase 0: foundation

Deliverables:

- Cargo workspace
- Clap CLI
- Configuration loading/validation
- PostgreSQL pool
- SQLx migrations
- health/readiness/metrics endpoints
- initial bootstrap command
- container build
- Podman Compose development setup

Success criteria:

```text
podman compose up
roosty migrate
roosty admin bootstrap --username admin --email admin@example.com
```

### Phase 1: local accounts and OAuth

Deliverables:

- Local account model
- Password login
- Browser sessions
- OAuth applications
- Authorization Code flow
- PKCE
- Opaque access tokens
- `verify_credentials`
- basic account API

Success criteria:

- A browser-based OAuth client can register, authorize, receive a token, and call authenticated endpoints.

### Phase 2: local social MVP

Deliverables:

- Status create/read/delete
- Replies
- Favourites
- Boosts
- Follows
- Public and home timelines
- Notifications
- Basic media upload
- streaming events
- combined `serve --with-worker` mode

Success criteria:

- A local user can use Phanpy and Elk for login, posting, reading timelines, reacting, and receiving updates.

### Phase 3: discovery and outbound federation

Deliverables:

- WebFinger
- NodeInfo
- Actor documents
- actor key management
- outbox
- signed outbound `Create`
- durable delivery and retry

Success criteria:

- A local public post reaches a remote Mastodon-compatible server.

### Phase 4: inbound federation

Deliverables:

- inbox
- signature verification
- remote actor cache
- inbound `Create`
- follow/accept/undo
- remote timeline rendering
- basic delete/update handling

Success criteria:

- A remote user can follow a local account, and local users can see remote content.

### Phase 5: hardening

Deliverables:

- domain policies
- rate limiting
- SSRF protection
- media limits
- operational dashboards/metrics
- repair/replay tools
- load and failure tests

Success criteria:

- A small public instance can operate safely with predictable retries and observability.

---

# Long term

## Scale-out

When a single combined process is no longer enough:

```text
API deployment:
  roosty serve

Worker deployment:
  roosty worker

Optional streaming deployment:
  roosty streaming
```

Keep the same Postgres job model initially. Add Redis or another pub/sub layer only after a measured need for multi-process streaming fan-out or cache pressure.

Potential future topology:

```text
load balancer
  ├── API instances
  ├── streaming instances
  └── worker instances
        │
        ├── PostgreSQL
        ├── Redis or equivalent pub/sub, optional
        └── S3-compatible object storage
```

## Timeline optimization

Potential long-term improvements:

- Hybrid fan-out-on-write/fan-out-on-read
- High-follower account thresholds
- Batched fan-out jobs
- Timeline partitioning
- Materialized views for public timelines
- Cursor-based pagination tuning
- Repair/rebuild commands

Do not add these before profiling real workloads.

## External identity

Optional future identity capabilities:

- Sign in with external OIDC providers
- Keycloak integration
- GitHub/Google login
- LDAP/SAML integration

Keep this separate from the Mastodon-compatible OAuth authorization server.

The server still issues its own local OAuth tokens to Mastodon-compatible clients.

## Administration and moderation

Future areas:

- Admin web UI
- Moderation queues
- Reports
- Domain block/allow lists
- Federation limits by domain
- Role-based permissions
- Audit log UI
- Account approvals
- Registration modes
- Invite management
- Appeals
- Legal hold and retention policy support

## Feature parity expansion

Potential later Mastodon-compatible features:

- Polls
- Lists
- Bookmarks
- Filters
- Custom emoji
- Search
- Trends
- Announcements
- Scheduled statuses
- Edits and revisions
- Quote posts
- Push notifications
- Featured tags and statuses
- Profile directory
- Account migration
- Data import/export
- Translation integrations
- Content warning policies
- Multi-account client behavior

Prioritize based on actual client compatibility gaps and user demand.

## Federation breadth

Potential future protocol work:

- More ActivityPub object/activity variants
- Shared Inbox support
- Featured collections
- Emoji reactions where interoperable
- Threaded reply improvements
- Better actor/object refresh strategy
- Remote media proxy
- Remote moderation signals
- Federation diagnostics UI
- Compatibility test suite against multiple Fediverse implementations

## Search

Add only when needed.

Potential design:

- Start with PostgreSQL full-text search for local content.
- Keep search APIs behind a project abstraction.
- Add an external search engine only when data size and ranking needs justify it.

## Storage and media

Potential future work:

- Remote media proxy/cache
- Lifecycle policies
- Per-account quotas
- Virus/malware scanning
- Image format conversion
- Video transcoding profiles
- CDN integration
- Media cleanup/garbage collection
- Backup verification

## Reliability and operations

Long-term operations work:

- Automated PostgreSQL backup/restore validation
- Object-storage backup strategy
- Migration rollback/recovery playbooks
- Job dead-letter inspection and replay
- Disaster recovery documentation
- Rate-limit tuning
- Capacity planning
- Multi-instance observability
- SLOs and alerting
- Security incident response procedures

## Optional bundled frontend

Do not do this initially.

If a bundled frontend is later desired, consider a separate static web project with its own release cycle. It should communicate only through the public REST/OAuth/streaming APIs.

Do not make the Rust backend depend on Rails-compatible bootstrapping or Mastodon's React build system.

---

## Acceptance criteria for the first meaningful release

The project is ready for an early public technical preview when all of these are true:

1. It starts from a clean PostgreSQL database using only Rust binaries and project migrations.
2. It can bootstrap an administrator.
3. It can run as one process with:

   ```text
   roosty serve --with-worker
   ```

4. Phanpy can register an OAuth app, authenticate with PKCE, read timelines, create a post, and receive updates.
5. Elk can perform the same basic flow.
6. Local posting, follows, favourites, boosts, notifications, and media uploads persist correctly.
7. Jobs survive process restart and retry safely.
8. WebFinger, NodeInfo, actor documents, outbox, and signed outbound ActivityPub delivery work.
9. Inbound ActivityPub requests are validated, persisted, deduplicated, and processed asynchronously.
10. The project has automated tests for API compatibility, OAuth, job durability, and core federation flows.
