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
| 🟡 | `PATCH /api/v1/accounts/update_credentials` | Profile basics and posting defaults. |
| 🟢 | `GET /api/v1/preferences` | Posting defaults and basic reading preferences. |
| 🟢 | Status metadata | Local `statuses_count` and `last_status_at` are populated. |
| 🔴 | `GET /api/v1/accounts/:id` | Public account lookup is missing. |
| 🔴 | Account statuses | `GET /api/v1/accounts/:id/statuses` is missing. |

### Statuses

| Support | Area | Details |
| --- | --- | --- |
| 🟢 | `POST /api/v1/statuses` | Local text statuses only. |
| 🟢 | `GET /api/v1/statuses/:id` | Local, non-deleted statuses. |
| 🟡 | `GET /api/v1/statuses/:id/context` | Local ancestors and descendants only. |
| 🟢 | `DELETE /api/v1/statuses/:id` | Owner-only soft delete. |
| 🟡 | Replies | Reply targets are validated and reply metadata includes the target account mention. |
| 🟡 | Visibility semantics | Public/unlisted URL reads work; private/direct are owner-only until follow graph support exists. |
| 🟡 | `GET /api/v1/favourites` | Returns authenticated user's local favourites; no cursor headers yet. |
| 🟢 | Favourites | Favourite/unfavourite APIs and status counts are implemented for local statuses. |
| 🔴 | Boosts | Reblog/unreblog APIs are missing. |
| 🔴 | Bookmarks | Bookmark/unbookmark APIs are missing. |

### Timelines

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v1/timelines/home` | Authenticated user's own local statuses; no follow graph yet. |
| 🟡 | `GET /api/v1/timelines/public` | Local public statuses only. |
| 🔴 | `GET /api/v1/timelines/tag/:tag` | Hashtag timeline is missing. |
| 🟢 | Cursor pagination | `max_id`, `since_id`, `min_id`, and `Link` headers are supported for local timelines. |

### Notifications and Markers

| Support | Area | Details |
| --- | --- | --- |
| 🔴 | `GET /api/v1/notifications` | Placeholder currently returns an empty list. |
| 🔴 | `GET /api/v1/markers` | Placeholder currently returns an empty object. |
| 🔴 | Persisted notifications | No notification records yet. |
| 🔴 | Notification read state | Marker updates are missing. |

### Tags, Push, and Media

| Support | Area | Details |
| --- | --- | --- |
| 🔴 | `GET /api/v1/followed_tags` | Placeholder currently returns an empty list. |
| 🔴 | `GET /api/v1/push/subscription` | Placeholder currently returns authenticated `404`. |
| 🔴 | Push subscriptions | Create/update/delete APIs are missing. |
| 🔴 | Media upload | `POST /api/v2/media` is missing. |
| 🔴 | Custom emojis | `GET /api/v1/custom_emojis` is missing. |

### Streaming

| Support | Area | Details |
| --- | --- | --- |
| 🟡 | `GET /api/v1/streaming` | WebSocket auth works; in-process only. |
| 🟢 | `GET /api/v1/streaming/health` | Returns `OK`. |
| 🟢 | `update` events | Sent after local status creation with `stream`, `event`, and JSON-string `payload`. |
| 🟡 | Subscribe controls | Basic subscribe/unsubscribe messages are accepted. |
| 🔴 | `notification` and `delete` events | Not emitted yet. |
| 🔴 | Multi-process fan-out | No Redis/Postgres pub-sub backend yet. |

## TODO

- [ ] Add WebFinger, actor documents, inbox, and outbox.
- [ ] Add federation delivery and inbound activity processing.
- [ ] Add follow graph and real home timeline membership.
- [ ] Add conversation endpoint support for replies.
- [ ] Add favourites pagination, boosts, and bookmarks.
- [ ] Add persisted notifications and notification streaming.
- [ ] Add media upload and attachment responses.
- [ ] Add moderation APIs and domain policy.
