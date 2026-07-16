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
| 🟢 | Actor document | Opt-in `GET /users/:username` exposes local actors, public profile URLs, service types, discovery preferences, profile creation timestamps/fields, public keys, and configured avatar/header image URLs. |
| 🟡 | Outbox | `GET /users/:username/outbox` exposes local public activities. |
| 🟢 | Status object pages | Public local Notes are available at `/users/:username/statuses/:id`. |
| 🟢 | Actor keys | RSA signing keys are encrypted at rest and the public key is published in actor documents. |

### Inbox and Delivery

| Support | Area | Details |
| --- | --- | --- |
| 🟢 | Inbox integrity | Every supported durable signed activity requires an absolute HTTPS ID at the verified actor origin. Canonical-JSON SHA-256 replay records make exact deliveries idempotent and ignore reused IDs with a different signer or payload. Transient ID-less activities are intentionally unsupported. |
| 🟡 | Signed HTTP requests | Legacy Mastodon-compatible HTTP signatures with `Digest` are verified and emitted; RFC 9421 is not implemented. |
| 🟡 | Outbound delivery | Durable jobs deliver follow responses, public/unlisted local status lifecycle activities, and local actor profile `Update` activities, with retry until the configured maximum age. |
| 🟡 | Remote fetch/cache | Policy-controlled remote actor discovery and signed public/unlisted Note caching are available; profile creation dates use actor `published` when supplied and fall back to first-seen time. Discovered remote avatar/header URLs are cached through a same-origin proxy, eagerly for followed accounts and lazily on request for other actors. Profile images must be supported, decodable raster images and refresh stale-while-serving. Remote status images are validated, cached with PNG previews and Mastodon metadata, and refreshed stale-while-serving; video/audio remain proxy-only. Signed Actor `Update`, `Delete`, and reciprocal `Move` activities refresh, tombstone, or redirect cached profiles; moves do not migrate follows. |

### Moderation and Safety

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | Domain policy | Remote discovery and delivery use an operator allow/block policy. It supports exact domains or `*` for all public domains, with blocks taking precedence. |
| 🟢 | SSRF protections | Remote discovery and delivery require HTTPS, reject unsafe resolved addresses, disable redirects, and enforce timeouts and response limits. |
| 🔴 | Federation moderation | No remote report, reject, or suspend flow yet. |

## Mastodon API

### Instance and Discovery

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `/api/v1/instance`, `/api/v2/instance` | Enough metadata for Elk startup; counts and capabilities are minimal. |

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
| 🟡 | Account statuses | `GET /api/v1/accounts/:id/statuses` returns local account statuses and the locally cached public/unlisted subset for remote actors, with cursor pagination, `Link` headers, media/reply/tag filters, and viewer state; pinned statuses are missing. |
| 🟡 | Follow graph | Local and remote follow/unfollow, relationships, followers, and following with cursor pagination are implemented; remote graph fetching remains missing. |
| 🟢 | `GET /api/v1/follow_requests` | Authenticated pending remote follow requests support `limit`, `max_id`, `since_id`, `min_id`, and Mastodon `Link` headers. |
| 🟡 | Mutes and blocks | Local mute/unmute, block/unblock, relationship state, mute duration, and paginated collections work; remote and domain policy are missing. |

### Search

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v2/search` | Public basic mixed account and local hashtag search. A user token is required for exact-handle `resolve=true`, nonzero `offset`, or `following=true`; status search remains missing. |
| 🟢 | Remote account resolution | Exact remote handles can be resolved synchronously through policy-controlled WebFinger and validated actor fetching, with cross-process advisory-lock deduplication. URLs are not resolved. |

### Statuses

| Support | Area | Details |
| --- | --- | --- |
| 🟢 | `POST /api/v1/statuses` | Local text statuses only. |
| 🟢 | `GET /api/v1/statuses/:id` | Visible local and locally cached remote statuses. |
| 🟢 | `GET /api/v1/statuses/:id/context` | Visible local and cached-remote ancestors and descendants, with Mastodon-compatible anonymous and authenticated traversal limits. Remote context is cache-only and does not fetch or backfill missing posts. |
| 🟡 | `PUT /api/v1/statuses/:id` | Owner-only local text, sensitivity, spoiler, language, media IDs, and media alt/focus edits; polls and edit history are missing. |
| 🟢 | `DELETE /api/v1/statuses/:id` | Owner-only soft delete. |
| 🟡 | Replies | Reply targets are validated and reply metadata includes the target account mention. |
| 🟡 | Mentions | Local `@username` mentions render as links, populate `mentions`, and create local notifications; remote mentions are missing. |
| 🟡 | Hashtags | Local `#tag` text is stored, linked in rendered status HTML, and returned in status `tags`. Cached remote Notes expose valid ActivityPub Hashtag tags with their origin URLs; remote discovery, timelines, and follows are missing. |
| 🟡 | Conversations | Direct-message conversations list/read/delete and direct stream events support recipient-scoped local/cached-remote direct Notes, mention-audience replacement on edits, unresolved remote participant IDs, replies to cached direct Notes, remote media fetching, and local/remote update/delete repair. Conversation deletion is account-local, and the direct stream emits only complete `conversation` payloads. Broader private visibility remains missing. |
| 🟡 | Visibility semantics | Public/unlisted URL reads work; direct reads work for local conversation participants; private remains owner-only until follow graph support exists. |
| 🟢 | `GET /api/v1/favourites` | Returns authenticated user's local and cached-remote favourites with cursor pagination. |
| 🟡 | Favourites | Public/unlisted local and cached remote statuses support favourite/unfavourite. Signed ActivityPub `Like`/`Undo` updates local counts and notifications; remote favourite counts are not fetched. |
| 🟢 | `GET /api/v1/bookmarks` | Returns authenticated user's local bookmarks with cursor pagination. |
| 🟡 | Boosts | Public/unlisted local and cached-remote statuses support reblog/unreblog. Signed ActivityPub `Announce`/`Undo` is delivered and processed, with remote boost counters, mixed `reblogged_by`, home timeline entries, and local notifications; quote posts and private boosts are missing. |
| 🟢 | Bookmarks | Bookmark/unbookmark APIs are implemented for local statuses. |

### Timelines

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v1/timelines/home` | Authenticated user's own statuses, followed local public/unlisted statuses, local boosts, cached remote statuses, and inbound remote boosts. |
| 🟡 | `GET /api/v1/timelines/public` | Local public statuses only. |
| 🟡 | `GET /api/v1/timelines/tag/:tag` | Local public hashtag timeline with `any[]`, `all[]`, `none[]`, `only_media`, cursor pagination, and `Link` headers; remote hashtag timelines are missing. |
| 🟢 | Cursor pagination | `max_id`, `since_id`, `min_id`, and `Link` headers are supported for implemented timeline and collection endpoints. |

### Notifications and Markers

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v1/notifications` | Local `mention`, `favourite`, `reblog`, and `follow` notifications with cursor pagination and basic filters. |
| 🟢 | `GET/POST /api/v1/markers` | Persists local home and notification read positions. |
| 🟡 | Persisted notifications | Local and signed-remote `mention`, `favourite`, `reblog`, `follow`, and locked-account `follow_request` notifications are stored transactionally and can be dismissed or cleared; grouping and policy flows remain missing. |
| 🟡 | Notification read state | Local home and notification markers work; grouped and remote notification state is missing. |

### Tags, Push, and Media

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v1/tags/:name` | Public lookup for locally observed hashtags with local history and authenticated `following` state; featured tag state is missing. |
| 🟡 | `GET /api/v1/followed_tags` | Lists locally followed hashtags for the authenticated account. |
| 🟡 | `POST /api/v1/tags/:name/follow`, `POST /api/v1/tags/:name/unfollow` | Local tag follow state is stored and matching public local posts enter the home timeline; remote tag delivery is missing. |
| 🔴 | `GET /api/v1/push/subscription` | Placeholder currently returns authenticated `404`. |
| 🔴 | Push subscriptions | Create/update/delete APIs are missing. |
| 🟡 | Media upload | `POST /api/v1/media`, `POST /api/v2/media`, media lookup/update/delete, status attachments, thumbnails, dimensions, `meta.small`, previews, and blurhash work for local image formats advertised by `/api/v2/instance`. Video, audio, async processing, and object storage are missing. |
| 🟡 | Custom emojis | `GET /api/v1/custom_emojis` is public and returns an empty local picker. Cached remote account and status projections expose valid ActivityPub Emoji tags; local emoji management and outbound emoji federation are missing. |

### Streaming

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v1/streaming` | WebSocket auth works; in-process only. |
| 🟡 | `GET /api/v1/streaming/direct` | Local and accepted remote direct conversation updates emit recipient-scoped `conversation` events. |
| 🟢 | `GET /api/v1/streaming/health` | Returns `OK`. |
| 🟢 | `update` events | Sent after local status creation to matching `user`, `public`, and `public:local` streams. |
| 🟡 | Subscribe controls | Basic subscribe/unsubscribe messages are accepted. |
| 🟡 | `notification` events | Local `mention`, `favourite`, and `follow` notifications are emitted to recipient `user` and `user:notification` streams. |
| 🟡 | `delete` events | Emitted for local status deletes and removed local boost timeline entries. |
| 🔴 | Multi-process fan-out | No Redis/Postgres pub-sub backend yet. |

## Federation

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | Local ActivityPub identity | Opt-in WebFinger, actor documents with encrypted-at-rest RSA keys, public Note objects, outboxes, and follower/following collection metadata are available. |
| 🟢 | Remote discovery and profile projections | Lookup and account search perform policy-controlled WebFinger discovery, validate/cache HTTPS actor documents, refresh expired actors, and return navigable UUID-backed remote account projections with proxied actor avatar/header images. |
| 🟡 | Outbound public status lifecycle | Public and unlisted local status creates, edits, and deletes are queued as signed ActivityPub deliveries to accepted remote followers. |
| 🟡 | Inbound public status lifecycle | Signed public/unlisted `Create`, `Update`, and `Delete` activities are cached with canonical object IDs, reply references, and author ownership checks. Replay markers and state changes commit atomically. Signed status and actor Deletes retain tombstones/audit objects while atomically removing stale notifications, favourites, boosts, typed reply links, follow state, delivery jobs, timelines, and conversation projections; captured stream repairs publish after commit. |
| 🟡 | Follow graph federation | Signed inbound/outbound follows, undo, accept, and reject are persisted and delivered through retrying jobs. Automatic and manually approved/rejected inbound follows create their response jobs atomically; Follow and Undo support both common link and embedded-object forms. Mastodon and paged public ActivityPub follower/following collections include accepted local and remote relationships. Remote collection fetching remains unavailable. |
| 🔴 | Remote timeline fan-out | Remote home-timeline delivery, repair, and remote visibility semantics are missing. |
| 🟡 | Remote replies, mentions, favourites, and boosts | Public/unlisted replies and resolved mentions are delivered with `inReplyTo` and typed Mention tags, cached inbound, and generate idempotent local mention/reply notifications. Signed `Like`/`Undo` and `Announce`/`Undo` are delivered and processed for public/unlisted statuses. Remote mutes and blocks remain missing. |
| 🟡 | Remote conversations and moderation | Signed remote direct Notes, mixed participant projection, direct replies, personal-inbox delivery, and remote media fetching work. Account migration and domain-policy moderation remain missing. |

## TODO

- [x] Add opt-in WebFinger, actor documents, public Notes, outbox, and public follower/following collection metadata.
- [x] Add safe remote actor discovery/cache refresh and remote profile projections.
- [ ] Add signed inbound Follow, Undo, Accept, Reject, and locked-account follow-request processing.
- [ ] Add signed outbound Follow, Undo, Accept, Reject, Create, Update, and Delete delivery with retries.
- [ ] Add remote follower home-timeline fan-out, repair jobs, and visibility semantics.
- [ ] Add remote mutes, blocks, and broader recipient notification federation. Signed remote delete repair is implemented.
- [ ] Add federated direct-message media fetching, account migration, and domain-policy moderation.
- [ ] Expand conversation support beyond local direct messages.
- [ ] Add remote hashtag support.
- [ ] Add grouped notifications, push integration, and remote notification events.
- [ ] Support multiple Roosty processes with shared streaming fan-out and cross-process coordination.
- [ ] Add video/audio media handling, async processing, and object storage.
- [ ] Add moderation APIs and domain policy.
