# Compatibility

Legend: 🟢 implemented, 🟡 usable with limits, 🔴 missing.

## ActivityPub and Federation

### Discovery

| Support | Area | Details |
| --- | --- | --- |
| 🔴 | WebFinger | Needed for remote account discovery. |
| 🟢 | `/.well-known/nodeinfo` | Advertises NodeInfo 2.1. |
| 🟡 | `/nodeinfo/2.0`, `/nodeinfo/2.1` | Static local instance metadata; counts are placeholders. |

### Actors and Objects

| Support | Area | Details |
| --- | --- | --- |
| 🔴 | Actor document | `GET /users/:username` is missing. |
| 🔴 | Outbox | `GET /users/:username/outbox` is missing. |
| 🔴 | Status object pages | ActivityPub object endpoints are missing. |
| 🔴 | Actor keys | No public signing keys yet. |

### Inbox and Delivery

| Support | Area | Details |
| --- | --- | --- |
| 🔴 | Inbox | `POST /users/:username/inbox` is missing. |
| 🔴 | Signed HTTP requests | No signature verification or signing yet. |
| 🔴 | Outbound delivery | No remote delivery jobs yet. |
| 🔴 | Remote fetch/cache | Remote actors and objects are not fetched. |

### Moderation and Safety

| Support | Area | Details |
| --- | --- | --- |
| 🔴 | Domain policy | No allow/block policy yet. |
| 🔴 | SSRF protections | Required before remote fetches. |
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
| 🟢 | `GET /api/v1/preferences` | Posting defaults and basic reading preferences. |
| 🟡 | `GET /api/v1/accounts/search` | Local username/display-name search only. |
| 🟡 | `GET /api/v1/accounts/lookup` | Local username/address lookup only; no WebFinger resolution. |
| 🟢 | Status metadata | Local `statuses_count` and `last_status_at` are populated. |
| 🔴 | `POST /api/v1/accounts` | Public registration is missing; local users are operator-created with the admin CLI. |
| 🟢 | `GET /api/v1/accounts/:id` | Public local account lookup. |
| 🟡 | Account statuses | `GET /api/v1/accounts/:id/statuses` returns local account statuses; media/tag/pin filters are empty until those features exist. |
| 🟡 | Follow graph | Local follow/unfollow, relationships, followers, and following with cursor pagination are implemented; remote follows are missing. |

### Search

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v2/search` | Local account results for `type=accounts`; statuses and hashtags are empty. |
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
| 🟡 | Visibility semantics | Public/unlisted URL reads work; private/direct are owner-only until follow graph support exists. |
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
| 🔴 | `GET /api/v1/timelines/tag/:tag` | Hashtag timeline is missing. |
| 🟢 | Cursor pagination | `max_id`, `since_id`, `min_id`, and `Link` headers are supported for implemented timeline and collection endpoints. |

### Notifications and Markers

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v1/notifications` | Local `mention`, `favourite`, `reblog`, and `follow` notifications with cursor pagination and basic filters. |
| 🔴 | `GET /api/v1/markers` | Placeholder currently returns an empty object. |
| 🟡 | Persisted notifications | Local notifications are stored and can be dismissed or cleared; remote, grouped, policy, and request flows are missing. |
| 🔴 | Notification read state | Marker updates are missing. |

### Tags, Push, and Media

| Support | Area | Details |
| --- | --- | --- |
| 🔴 | `GET /api/v1/followed_tags` | Placeholder currently returns an empty list. |
| 🔴 | `GET /api/v1/push/subscription` | Placeholder currently returns authenticated `404`. |
| 🔴 | Push subscriptions | Create/update/delete APIs are missing. |
| 🟡 | Media upload | `POST /api/v1/media`, `POST /api/v2/media`, media lookup/update/delete, status attachments, thumbnails, dimensions, `meta.small`, previews, and blurhash work for local image formats advertised by `/api/v2/instance`. Video, audio, async processing, and object storage are missing. |
| 🔴 | Custom emojis | `GET /api/v1/custom_emojis` is missing. |

### Streaming

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v1/streaming` | WebSocket auth works; in-process only. |
| 🟢 | `GET /api/v1/streaming/health` | Returns `OK`. |
| 🟢 | `update` events | Sent after local status creation to matching `user`, `public`, and `public:local` streams. |
| 🟡 | Subscribe controls | Basic subscribe/unsubscribe messages are accepted. |
| 🟡 | `notification` events | Local `mention`, `favourite`, and `follow` notifications are emitted to recipient `user` and `user:notification` streams. |
| 🟡 | `delete` events | Emitted for local status deletes and removed local boost timeline entries. |
| 🔴 | Multi-process fan-out | No Redis/Postgres pub-sub backend yet. |

## TODO

- [ ] Add WebFinger, actor documents, inbox, and outbox.
- [ ] Add federation delivery and inbound activity processing.
- [ ] Add remote follow graph and full private-status home timeline semantics.
- [ ] Add conversation endpoint support for replies.
- [ ] Add remote ActivityPub `Announce` support.
- [ ] Add notification markers, grouped notifications, push integration, and remote notification events.
- [ ] Add video/audio media handling, async processing, and object storage.
- [ ] Add moderation APIs and domain policy.
