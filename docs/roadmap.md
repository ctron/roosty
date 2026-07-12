# Roadmap

## Long Term

- ActivityPub federation: WebFinger, actors, signed delivery, inbound processing, remote actor/object cache, and moderation policy enforcement.
- Production-grade timelines: follow graph fan-out, repair jobs, cursor pagination, remote statuses, and scalable streaming fan-out.
- Media support: local and S3-compatible storage, validation, thumbnails, processing jobs, and remote media fetch limits.
- Moderation and operations: account suspension, local status removal, domain policy, admin tools, metrics, and audit-friendly workflows.
- Compatibility hardening: broader Mastodon API coverage, versioned response DTOs, pagination headers, scope enforcement, and client regression tests.

## Medium Term

- Social graph APIs: follow, unfollow, mute, unmute, block, and unblock for local accounts.
- Status interactions: replies, favourites, boosts, bookmarks, and delete streaming events.
- Notifications: persisted notification records, read markers, and streaming `notification` events.
- Account/profile APIs beyond current credentials: public account lookup, profile pages, and status collections.
- Streaming channels: `public`, `public:local`, `user`, `user:notification`, and bounded slow-client handling.

## Short Term

- Fill Mastodon client startup gaps found by Elk and browser logs.
- Add cursor pagination for account status collections.
- Add a conversation endpoint for replies.
- Add favourites, boosts, and bookmarks.
- Keep compatibility documentation updated with every implemented or intentionally deferred API.
