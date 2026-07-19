# Compatibility

Legend: 🟢 implemented, 🟡 usable with limits, 🔴 missing.

## ActivityPub and Federation

### Discovery

| Support | Area | Details |
| --- | --- | --- |
| 🟢 | WebFinger | Opt-in `/.well-known/webfinger` serves local `acct:` identities. |
| 🟢 | `/.well-known/nodeinfo` | Advertises NodeInfo 2.1. |
| 🟡 | `/nodeinfo/2.0`, `/nodeinfo/2.1` | Static local instance metadata; counts are placeholders. |

### Actors and Objects

| Support | Area | Details |
| --- | --- | --- |
| 🟢 | Actor document | Opt-in `GET /users/:username` exposes local actors, public profile URLs, service types, discovery preferences, profile creation timestamps/fields, public keys, configured avatar/header image URLs, and the actor's `featured` collection. |
| 🟡 | Outbox | `GET /users/:username/outbox` exposes local public activities. |
| 🟢 | Status object pages | Public local Notes and explicitly pinned unlisted Notes are available at `/users/:username/statuses/:id`. |
| 🟢 | Featured posts | `/users/:username/collections/featured` embeds up to five pinned public/unlisted Notes newest-pin-first. Remote same-origin featured collections are refreshed durably through at most four pages and cached to 20 Notes; signed replay-safe `Add`/`Remove` updates use the same per-actor reconciliation lock. |
| 🟢 | Actor keys | RSA signing keys are encrypted at rest and the public key is published in actor documents. |

### Inbox and Delivery

| Support | Area | Details |
| --- | --- | --- |
| 🟢 | Inbox integrity | Every supported durable signed activity requires an absolute HTTPS ID at the verified actor origin. Canonical-JSON SHA-256 replay records make exact deliveries idempotent and ignore reused IDs with a different signer or payload. Transient ID-less activities are intentionally unsupported. |
| 🟡 | Signed HTTP requests | Legacy Mastodon-compatible HTTP signatures with `Digest` are verified and emitted; RFC 9421 is not implemented. |
| 🟡 | Outbound delivery | Durable jobs deliver follow responses, public/unlisted/follower-only local status lifecycle activities, pin `Add`/`Remove` activities, and local actor profile `Update` activities, with retry until the configured maximum age. Follower-only activities also reach explicitly mentioned remote actors. |
| 🟡 | Remote fetch/cache | Policy-controlled remote actor discovery and signed public/unlisted/follower-only Note caching are available; follower-only acceptance uses the actor's validated exact `followers` URL and requires a current local follower or explicit local mention. Profile creation dates use actor `published` when supplied and fall back to first-seen time. Discovered remote avatar/header URLs are cached through a same-origin proxy, eagerly for followed accounts and lazily on request for other actors. Profile images must be supported, decodable raster images and refresh stale-while-serving. Remote status images are validated, cached with PNG previews and Mastodon metadata, and refreshed stale-while-serving; video/audio remain proxy-only. Signed Actor `Update`, `Delete`, and reciprocal `Move` activities refresh, tombstone, or redirect cached profiles; moves do not migrate follows. Missing private deliveries are never fetched or backfilled. |

### Moderation and Safety

| Support | Area | Details |
| --- | --- | --- |
| 🟢 | Domain policy | Remote discovery and delivery use an operator allow/block policy. Configured blocks suspend a domain and its subdomains, including cached exposure, startup relationship reconciliation, and queued-delivery dropping. |
| 🟢 | SSRF protections | Remote discovery and delivery require HTTPS, reject unsafe resolved addresses, disable redirects, and enforce timeouts and response limits. |
| 🟡 | Federation moderation | Per-account signed Block/Undo is delivered and accepted with replay-safe identity matching. Configured domain suspension is enforced; reports and administrative moderation APIs remain out of scope. |

## Mastodon API

### Instance and Discovery

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `/api/v1/instance`, `/api/v2/instance` | Enough metadata for Elk startup; `version` reports the Roosty release version, while counts and capabilities are minimal. |
| ✅ | `/api/v1/version` | Roosty extension exposing package and build-time Git identity. |

### OAuth

| Support | Area | Details |
| --- | --- | --- |
| 🟢 | `POST /api/v1/apps` | OAuth app registration. |
| 🟢 | `GET/POST /oauth/authorize` | Local authorization flow, including the trailing-slash form and out-of-band code display used by CLI clients such as toot. |
| 🟢 | `POST /oauth/token` | Authorization code and Elk-compatible token flow. |
| 🟢 | `POST /oauth/revoke` | Bearer token revocation. |

### Accounts and Preferences

| Support | Area | Details |
| --- | --- | --- |
| 🟢 | `GET /api/v1/accounts/verify_credentials` | Returns local credential account. |
| 🟡 | `PATCH /api/v1/accounts/update_credentials` | Profile basics, avatar/header images, and posting defaults. |
| 🟢 | `GET /auth/edit`, `PUT/PATCH /auth` | Signed-in users can change their password through Mastodon's browser settings flow. |
| 🟢 | `GET /api/v1/preferences` | Posting defaults and basic reading preferences. |
| 🟢 | `GET /api/v1/accounts/search` | Authenticated mixed local/cached-remote account search with deterministic ranking, follow filtering, pagination, and exact-handle resolution. |
| 🟢 | `GET /api/v1/accounts/lookup` | Public local and cached-remote address lookup; `resolve=true` performs policy-controlled WebFinger resolution. |
| 🟢 | Account metadata | Local `created_at`, `statuses_count`, and `last_status_at` are populated; remote `created_at` uses ActivityPub profile `published` with first-seen fallback, remote status metadata reflects active locally cached posts, and remote avatar/header fields use locally cached proxy URLs when advertised. |
| 🔴 | `POST /api/v1/accounts` | Public registration is missing; local users are operator-created with the admin CLI. |
| 🟢 | `GET /api/v1/accounts/:id` | Public local and active cached-remote account lookup. |
| 🟢 | Account statuses | `GET /api/v1/accounts/:id/statuses` returns statuses authorized for the viewer, including local or cached-remote `pinned=true` results, with cursor pagination, `Link` headers, media/reply/tag filters, and viewer state. |
| 🟡 | Follow graph | Local and remote follow/unfollow, relationships, followers, and following with cursor pagination are implemented. Local and remote follows honor `reblogs` and `notify`; remote graph fetching and language filters remain missing. |
| 🟢 | `GET /api/v1/follow_requests` | Authenticated pending remote follow requests support `limit`, `max_id`, `since_id`, `min_id`, and Mastodon `Link` headers. |
| 🟢 | Mutes and blocks | Local and remote mute/unmute and block/unblock, relationship state, mute duration/expiry, and mixed cursor-paginated collections work. Remote mutes remain local-only; blocks federate. |
| 🟢 | Lists | Private list CRUD, local and cached-remote followed-account membership, account-to-list lookup, cursor-paginated member collections, reply policies, exclusive home-feed filtering, and mixed list timelines are implemented. |

### Search

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v2/search` | Public basic mixed account and observed hashtag search, including tags indexed from cached remote Notes. A user token is required for exact-handle `resolve=true`, nonzero `offset`, or `following=true`; status search remains missing. |
| 🟢 | Remote account resolution | Exact remote handles can be resolved synchronously through policy-controlled WebFinger and validated actor fetching, including WebFinger domains delegated to a different actor origin, with cross-process advisory-lock deduplication. URLs are not resolved. |

### Statuses

| Support | Area | Details |
| --- | --- | --- |
| 🟢 | `POST /api/v1/statuses` | Local text and consent-aware quote statuses; quote-plus-media is rejected to match Mastodon. |
| 🟢 | `GET /api/v1/statuses/:id` | Visible local and locally cached remote statuses. |
| 🟢 | `GET /api/v1/statuses/:id/source` | Authenticated plain-text source lookup for visible local statuses, including content warnings. |
| 🟢 | `GET /api/v1/statuses/:id/history` | Visible local and cached-remote status revisions, oldest first, with immutable content, warning, sensitivity, emoji, and media projections. Legacy edits expose their known current state because pre-upgrade text cannot be reconstructed. |
| 🟢 | `GET /api/v1/statuses/:id/context` | Visible local and cached-remote ancestors and descendants, with Mastodon-compatible anonymous and authenticated traversal limits. Remote context is cache-only and does not fetch or backfill missing posts. |
| 🟡 | `PUT /api/v1/statuses/:id` | Owner-only local text, sensitivity, spoiler, language, media IDs, and media alt/focus edits with transactional history; polls are missing. No-op edits produce no revision, delivery, or streaming event. |
| 🟢 | `DELETE /api/v1/statuses/:id` | Owner-only soft delete. |
| 🟢 | Status pins | Authenticated idempotent `POST /api/v1/statuses/:id/pin` and `/unpin` support owned public/unlisted posts, enforce five pins transactionally across processes, expose `Status.pinned`, and advertise the limit in instance configuration. |
| 🟢 | Replies | Reply targets are validated and reply metadata includes the target account mention. ActivityPub replies to cached remote Notes address and deliver to the parent author even without an explicit text mention. |
| 🟡 | Mentions | Local and resolved remote mentions render as links and populate `mentions`. Local recipients are tracked independently from notifications; active addressed mentions receive idempotent notifications and subsequent edit/delete streaming. |
| 🟢 | Hashtags | Local `#tag` text and valid Hashtag tags from cached remote Notes share a normalized namespace. Status projections use local canonical tag URLs; mixed search, seven-day history, origin-filtered tag timelines, and followed-tag home/user-stream fan-out are available. Remote delivery remains cache-only and push-driven. |
| 🟢 | Status links | Explicit local `http://` and `https://` URLs render as Mastodon-compatible safe anchors in status, history, streaming, and federation projections. Bare domains and non-web URI schemes remain plain text; rich preview cards are not implemented. |
| 🟡 | Conversations | Direct-message conversations list/read/delete and direct stream events support recipient-scoped local/cached-remote direct Notes, mention-audience replacement on edits, unresolved remote participant IDs, replies to cached direct Notes, remote media fetching, and local/remote update/delete repair. Conversation deletion is account-local, and the direct stream emits only complete `conversation` payloads. |
| 🟢 | Visibility semantics | Public/unlisted reads work; follower-only local and cached remote posts and replies are visible to owners, current followers, and explicitly mentioned accounts; direct reads remain recipient-scoped. Anonymous and unrelated access returns `404`, and inaccessible thread nodes stop cache-only traversal. |
| 🟢 | `GET /api/v1/favourites` | Returns authenticated user's local and cached-remote favourites with cursor pagination. |
| 🟡 | Favourites | Authorized public/unlisted/follower-only local and cached remote statuses support favourite/unfavourite. Signed ActivityPub `Like`/`Undo` updates local counts and notifications; remote favourite counts are not fetched. |
| 🟢 | `GET /api/v1/bookmarks` | Returns authenticated user's local bookmarks with cursor pagination. |
| 🟡 | Boosts | Public/unlisted local and cached-remote statuses support reblog/unreblog. Follower-only boosts are rejected except for author self-boosts representable by the existing model. Signed ActivityPub `Announce`/`Undo` supports public/unlisted statuses. |
| 🟢 | Quote posts | Local and cached-remote targets support per-status policies, viewer-aware `quote`/`quote_approval`/`quotes_count`, edit history, cursor-paginated quote listing, revocation, notifications, FEP-044f QuoteRequest/Accept/Reject, policy-controlled remote authorization dereferencing, dereferenceable local authorizations, durable delivery, and authorization deletion. Remote manual policies stay pending until authorized. |
| 🟢 | Bookmarks | Bookmark/unbookmark APIs are implemented for local statuses. |

### Timelines

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v1/timelines/home` | Authenticated user's own statuses, followed and explicitly addressed local/cached-remote follower-only statuses, public/unlisted followed statuses, local boosts, and inbound remote boosts. |
| 🟢 | `GET /api/v1/timelines/public` | Chronological local and already-cached remote public statuses with `local`, `remote`, `only_media`, cursor pagination, `Link` headers, federation-domain policy, and authenticated mute/block filtering. Replies and boosts are excluded to match Mastodon's live feeds. |
| 🟢 | `GET /api/v1/timelines/tag/:tag` | Mixed local and cached-remote public hashtag timeline with `local`, `remote`, `any[]`, `all[]`, `none[]`, `only_media`, cursor pagination, `Link` headers, domain policy, and viewer moderation filtering. |
| 🟢 | `GET /api/v1/timelines/list/:list_id` | Authenticated mixed local/cached-remote list timelines honor ownership, reply policy, moderation, boosts, cursor pagination, and `Link` headers. |
| 🟢 | Cursor pagination | `max_id`, `since_id`, `min_id`, and `Link` headers are supported for implemented timeline and collection endpoints. |

### Notifications and Markers

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v1/notifications` | Local `mention`, `favourite`, `reblog`, `follow`, followed-account `status`, and boosted-status `update` notifications with cursor pagination and basic filters. |
| 🟢 | `GET/POST /api/v1/markers` | Persists local home and notification read positions. |
| 🟡 | Persisted notifications | Local and signed-remote `mention`, `favourite`, `reblog`, `follow`, followed-account `status`, boosted-status `update`, and locked-account `follow_request` notifications are stored transactionally and can be dismissed or cleared; grouping and Mastodon notification-policy APIs remain missing. Web Push delivery policies are implemented separately. |
| 🟡 | Notification read state | Local home and notification markers work; grouped and remote notification state is missing. |

### Tags, Push, and Media

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v1/tags/:name` | Public lookup for observed local/cached-remote hashtags with mixed history and authenticated `following` state; featured tag state is missing. |
| 🟡 | `GET /api/v1/followed_tags` | Lists locally followed hashtags for the authenticated account. |
| 🟢 | Featured hashtags | Authenticated list/create/delete and suggestion APIs, public per-account lookup, a fixed ten-tag limit, and batched visible-status statistics are implemented for local and cached-remote accounts. |
| 🟢 | `POST /api/v1/tags/:name/follow`, `POST /api/v1/tags/:name/unfollow` | Local tag follow state is stored and matching public local or cached-remote posts enter the home timeline and user stream. Remote matching is limited to Notes received through normal federation delivery. |
| 🟢 | `/api/v1/push/subscription` | OAuth `push`-scoped GET/POST/PUT/DELETE manages one subscription per token, including typed delivery policies and the VAPID server key. POST and PUT accept both Elk/Masto.js JSON and Tusky form encoding, including Tusky's `standard=true` registration. Supported alert switches are `mention`, `favourite`, `reblog`, `follow`, `follow_request`, `status`, `update`, `quote`, and `quoted_update`; `poll`, `admin.sign_up`, and `admin.report` remain unsupported because Roosty does not generate those notification types. |
| 🟢 | Push delivery | Transactional UUIDv7 jobs deliver Mastodon payloads using RFC 8291 `aes128gcm` or legacy `aesgcm`, retry transient failures, remove rejected subscriptions, and validate public HTTPS endpoints independently at registration and delivery. Grouped notifications remain separate and unsupported. |
| 🟡 | Media upload | `POST /api/v1/media`, `POST /api/v2/media`, media lookup/update/delete, status attachments, thumbnails, dimensions, `meta.small`, previews, and blurhash work for local image formats advertised by `/api/v2/instance`. Video, audio, async processing, and object storage are missing. |
| 🟡 | Custom emojis | `GET /api/v1/custom_emojis` is public and returns an empty local picker. Cached remote account and status projections expose valid ActivityPub Emoji tags; local emoji management and outbound emoji federation are missing. |

### Streaming

| Support | Area | Details |
| --- | --- | --- |
| 🟢 | `GET /api/v1/streaming` | Authenticated WebSockets use bounded, PostgreSQL-backed multi-process fan-out with connection, send, ping, and idle limits. |
| 🟡 | `GET /api/v1/streaming/direct` | Local and accepted remote direct conversation updates emit recipient-scoped `conversation` events. |
| 🟢 | `GET /api/v1/streaming/health` | Returns `OK`. |
| 🟢 | Public status events | Local and accepted cached-remote create, edit, and delete events reach origin-appropriate `public`, `public:local`, `public:remote`, and media-filtered streams through the multi-process event log. |
| 🟡 | `status.update` events | Local and accepted remote edits reach matching followers and active mention-only recipients. Mention recipients receive the edit on both combined `user` and `user:notification` streams; followed-tag and origin-appropriate public streams receive matching public edits. |
| 🟡 | Subscribe controls | Basic subscribe/unsubscribe messages are accepted. |
| 🟡 | `notification` events | Local and remote `mention`, `favourite`, `reblog`, `follow`, followed-account `status`, boosted-status `update`, and follow-request notifications are emitted to recipient `user` and `user:notification` streams. |
| 🟡 | `delete` events | Emitted for local status deletes, including current followed-tag recipients, and removed local or followed remote boost timeline entries. |
| 🟢 | Multi-process fan-out | PostgreSQL notifications and a retained ordered event log provide reconnect recovery without startup replay. |

## Federation

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | Local ActivityPub identity | Opt-in WebFinger, actor documents with encrypted-at-rest RSA keys, public Note objects, outboxes, follower/following collection metadata, pinned Notes, and featured hashtag collections are available. |
| 🟢 | Remote discovery and profile projections | Lookup and account search perform policy-controlled WebFinger discovery, validate/cache HTTPS actor documents, refresh expired actors, and return navigable UUID-backed remote account projections with proxied actor avatar/header images. |
| 🟡 | Outbound status lifecycle | Public, unlisted, and follower-only local status creates, edits, replies, and deletes are queued as signed ActivityPub deliveries to accepted remote followers and explicit remote mentions. Note content uses the same linked HTML projection exposed through Mastodon APIs. |
| 🟡 | Inbound status lifecycle | Signed public/unlisted/follower-only `Create`, complete-object `Update`, and `Delete` activities are cached with canonical object IDs, exact addressed audiences, reply references, author ownership checks, and transactional material-change revision history. Mention tags do not grant delivery without matching `to`/`cc` addressing. Replay markers and state changes commit atomically; replayed, stale, and no-op updates do not create revisions or events. Signed status and actor Deletes retain tombstones/audit objects while atomically removing stale notifications, favourites, boosts, typed reply links, follow state, delivery jobs, timelines, and conversation projections; captured stream repairs publish after commit. |
| 🟡 | Follow graph federation | Signed inbound/outbound follows, undo, accept, and reject are persisted and delivered through retrying jobs. Automatic and manually approved/rejected inbound follows create their response jobs atomically; Follow and Undo support both common link and embedded-object forms. Mastodon and paged public ActivityPub follower/following collections include accepted local and remote relationships. Remote collection fetching remains unavailable. |
| 🟢 | Remote timeline fan-out | Cached remote posts are pushed to authorized home streams; follower-only access follows current accepted relationships and explicit audiences, with no polling or backfill. |
| 🟡 | Remote replies, mentions, favourites, and boosts | Public/unlisted replies and resolved mentions are delivered with `inReplyTo` and typed Mention tags, cached inbound, and generate idempotent local mention/reply notifications. Signed `Like`/`Undo` and `Announce`/`Undo` are delivered and processed subject to bilateral block and notification-mute policy. Cached public Note hashtags are transactionally indexed for mixed timelines and followed-tag fan-out. |
| 🟢 | Featured content federation | Actor `featured` Notes and `featuredTags` hashtags are discovered through validated same-origin collections, refreshed by bounded durable jobs, cached atomically, and synchronized through signed replay-safe `Add`/`Remove` activities. |
| 🟡 | Remote conversations and moderation | Signed remote direct Notes, mixed participant projection, direct replies, personal-inbox delivery, remote media fetching, per-account moderation, and domain suspension work. Account migration remains missing. |

## TODO

- [x] Add opt-in WebFinger, actor documents, public Notes, outbox, and public follower/following collection metadata.
- [x] Add safe remote actor discovery/cache refresh and remote profile projections.
- [ ] Add signed inbound Follow, Undo, Accept, Reject, and locked-account follow-request processing.
- [ ] Add signed outbound Follow, Undo, Accept, Reject, Create, Update, and Delete delivery with retries.
- [x] Add remote follower home-timeline fan-out, repair jobs, and follower-only visibility semantics.
- [x] Add remote mutes and blocks with notification and recipient filtering. Signed remote delete repair is implemented.
- [ ] Add federated direct-message media fetching and account migration.
- [ ] Expand conversation support beyond local direct messages.
- [x] Add cache-only remote hashtag indexing, timelines, discovery, history, and followed-tag fan-out.
- [ ] Add grouped notifications; Web Push integration is implemented independently.
- [ ] Add poll and administrative notifications, including their Web Push alert switches.
- [x] Support multiple Roosty processes with PostgreSQL-backed streaming fan-out and cross-process coordination.
- [ ] Add video/audio media handling, async processing, and object storage.
- [x] Add per-account moderation APIs and configured suspend-level domain policy.
