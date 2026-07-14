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
| 🟢 | Actor document | Opt-in `GET /users/:username` exposes local actors and public keys. |
| 🟡 | Outbox | `GET /users/:username/outbox` exposes local public activities. |
| 🟢 | Status object pages | Public local Notes are available at `/users/:username/statuses/:id`. |
| 🟢 | Actor keys | RSA signing keys are encrypted at rest and the public key is published in actor documents. |

### Inbox and Delivery

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | Inbox | Signed inbox handling supports follows and public/unlisted status lifecycle activities. |
| 🟡 | Signed HTTP requests | Legacy Mastodon-compatible HTTP signatures with `Digest` are verified and emitted; RFC 9421 is not implemented. |
| 🟡 | Outbound delivery | Durable jobs deliver follow responses and public/unlisted local status lifecycle activities, with retry until the configured maximum age. |
| 🟡 | Remote fetch/cache | Policy-controlled remote actor discovery and signed public/unlisted Note caching are available; refresh and client projections remain incomplete. |

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
| 🟢 | `GET/POST /oauth/authorize` | Local authorization flow. |
| 🟢 | `POST /oauth/token` | Authorization code and Elk-compatible token flow. |
| 🟢 | `POST /oauth/revoke` | Bearer token revocation. |

### Accounts and Preferences

| Support | Area | Details |
| --- | --- | --- |
| 🟢 | `GET /api/v1/accounts/verify_credentials` | Returns local credential account. |
| 🟡 | `PATCH /api/v1/accounts/update_credentials` | Profile basics, avatar/header images, and posting defaults. |
| 🟢 | `GET /auth/edit`, `PUT/PATCH /auth` | Signed-in users can change their password through Mastodon's browser settings flow. |
| 🟢 | `GET /api/v1/preferences` | Posting defaults and basic reading preferences. |
| 🟡 | `GET /api/v1/accounts/search` | Local username/display-name search only. |
| 🟡 | `GET /api/v1/accounts/lookup` | Local username/address lookup only; no WebFinger resolution. |
| 🟢 | Status metadata | Local `statuses_count` and `last_status_at` are populated. |
| 🔴 | `POST /api/v1/accounts` | Public registration is missing; local users are operator-created with the admin CLI. |
| 🟢 | `GET /api/v1/accounts/:id` | Public local account lookup. |
| 🟡 | Account statuses | `GET /api/v1/accounts/:id/statuses` returns local account statuses with media and hashtag filters; pinned statuses are missing. |
| 🟡 | Follow graph | Local follow/unfollow, relationships, followers, and following with cursor pagination are implemented; remote follows are missing. |
| 🟡 | Mutes and blocks | Local mute/unmute, block/unblock, relationship state, mute duration, and paginated collections work; remote and domain policy are missing. |

### Search

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v2/search` | Local account results and local hashtag prefix results; status search and remote resolution are missing. |
| 🔴 | Remote account resolution | `resolve=true` does not fetch remote accounts until WebFinger exists. |

### Statuses

| Support | Area | Details |
| --- | --- | --- |
| 🟢 | `POST /api/v1/statuses` | Local text statuses only. |
| 🟢 | `GET /api/v1/statuses/:id` | Local, non-deleted statuses. |
| 🟡 | `GET /api/v1/statuses/:id/context` | Local ancestors and descendants only. |
| 🟡 | `PUT /api/v1/statuses/:id` | Owner-only local text, sensitivity, spoiler, language, media IDs, and media alt/focus edits; polls and edit history are missing. |
| 🟢 | `DELETE /api/v1/statuses/:id` | Owner-only soft delete. |
| 🟡 | Replies | Reply targets are validated and reply metadata includes the target account mention. |
| 🟡 | Mentions | Local `@username` mentions render as links, populate `mentions`, and create local notifications; remote mentions are missing. |
| 🟡 | Hashtags | Local `#tag` text is stored, linked in rendered status HTML, and returned in status `tags`; followed tags and remote tags are missing. |
| 🟡 | Conversations | Local direct-message conversations list/read/delete and direct stream events work for direct statuses with local participants; remote conversations are missing. |
| 🟡 | Visibility semantics | Public/unlisted URL reads work; direct reads work for local conversation participants; private remains owner-only until follow graph support exists. |
| 🟢 | `GET /api/v1/favourites` | Returns authenticated user's local favourites with cursor pagination. |
| 🟢 | Favourites | Favourite/unfavourite APIs and status counts are implemented for local statuses. |
| 🟢 | `GET /api/v1/bookmarks` | Returns authenticated user's local bookmarks with cursor pagination. |
| 🟡 | Boosts | Local reblog/unreblog APIs, `reblogs_count`, viewer `reblogged`, `reblogged_by`, home timeline boost entries, and reblog notifications are implemented; ActivityPub `Announce` is missing. |
| 🟢 | Bookmarks | Bookmark/unbookmark APIs are implemented for local statuses. |

### Timelines

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v1/timelines/home` | Authenticated user's own statuses, followed local public/unlisted statuses, and followed local boosts when enabled. |
| 🟡 | `GET /api/v1/timelines/public` | Local public statuses only. |
| 🟡 | `GET /api/v1/timelines/tag/:tag` | Local public hashtag timeline with `any[]`, `all[]`, `none[]`, `only_media`, cursor pagination, and `Link` headers; remote hashtag timelines are missing. |
| 🟢 | Cursor pagination | `max_id`, `since_id`, `min_id`, and `Link` headers are supported for implemented timeline and collection endpoints. |

### Notifications and Markers

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v1/notifications` | Local `mention`, `favourite`, `reblog`, and `follow` notifications with cursor pagination and basic filters. |
| 🟢 | `GET/POST /api/v1/markers` | Persists local home and notification read positions. |
| 🟡 | Persisted notifications | Local notifications are stored and can be dismissed or cleared; remote, grouped, policy, and request flows are missing. |
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
| 🔴 | Custom emojis | `GET /api/v1/custom_emojis` is missing. |

### Streaming

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v1/streaming` | WebSocket auth works; in-process only. |
| 🟡 | `GET /api/v1/streaming/direct` | Local direct conversation updates emit `conversation` events; remote direct messages are missing. |
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
| 🟡 | Remote discovery and profile projections | `resolve=true` lookup performs policy-controlled WebFinger discovery, validates and caches HTTPS actor documents, and returns UUID-backed remote account projections. Search integration and refresh jobs are missing. |
| 🟡 | Outbound public status lifecycle | Public and unlisted local status creates, edits, and deletes are queued as signed ActivityPub deliveries to accepted remote followers. |
| 🟡 | Inbound public status lifecycle | Signed public/unlisted `Create`, `Update`, and `Delete` activities are cached with canonical object IDs, reply references, and author ownership checks. Cached Notes from accepted remote follows appear in home timelines and are streamed to those followers; media and interactions remain missing. |
| 🟡 | Follow graph federation | Signed inbound/outbound follows, undo, accept, and reject are persisted and delivered through retrying jobs. Mastodon and paged public ActivityPub follower/following collections include accepted local and remote relationships. Remote collection fetching remains unavailable. |
| 🔴 | Remote timeline fan-out | Remote home-timeline delivery, repair, and remote visibility semantics are missing. |
| 🟡 | Remote replies and mentions | Public/unlisted replies and resolved mentions are delivered with `inReplyTo` and typed Mention tags, cached inbound, and generate idempotent local mention/reply notifications. Remote favourites, boosts, mutes, and blocks remain missing. |
| 🔴 | Remote conversations and moderation | Direct conversations, account migration, signed inbox processing, domain-policy moderation, and delivery are missing. |

## TODO

- [x] Add opt-in WebFinger, actor documents, public Notes, outbox, and public follower/following collection metadata.
- [ ] Add safe remote actor discovery/cache refresh and remote profile projections.
- [ ] Add signed inbound Follow, Undo, Accept, Reject, and locked-account follow-request processing.
- [ ] Add signed outbound Follow, Undo, Accept, Reject, Create, Update, and Delete delivery with retries.
- [ ] Add remote follower home-timeline fan-out, repair jobs, and visibility semantics.
- [ ] Add remote replies, mentions, favourites, boosts, deletes, notifications, mutes, and blocks.
- [ ] Add remote direct conversations, account migration, and domain-policy moderation.
- [ ] Expand conversation support beyond local direct messages.
- [ ] Add remote ActivityPub `Announce` support.
- [ ] Add remote hashtag support.
- [ ] Add grouped notifications, push integration, and remote notification events.
- [ ] Support multiple Roosty processes with shared streaming fan-out and cross-process coordination.
- [ ] Add video/audio media handling, async processing, and object storage.
- [ ] Add moderation APIs and domain policy.
