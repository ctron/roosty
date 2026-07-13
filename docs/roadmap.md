# Roadmap

## Full Social Graph

### Available now

- Local profiles, lookup/search, follows/unfollows, relationships, followers/following, and local account moderation.
- Local public/unlisted home-timeline fan-out, replies, mentions, favourites, boosts, notifications, and direct conversations.
- Opt-in local ActivityPub identity: WebFinger, actor documents, encrypted actor keys, public Notes, outboxes, and follower/following collection metadata.
- Safe operator-policy-controlled remote actor discovery through `resolve=true` account lookup, including WebFinger and validated actor caching. Policies can allow exact domains or all public domains with `*`, with explicit blocks taking precedence.

### Federation gaps

- [ ] Add controlled remote actor cache refresh and remote account search projections.
- [ ] Add signed outbound `Follow` and `Undo(Follow)` delivery, including durable delivery jobs, destination deduplication, retries, and permanent-failure diagnostics.
- [ ] Add signed inbound `Follow`, `Undo(Follow)`, `Accept`, and `Reject` processing, including idempotency and locked-account follow requests.
- [ ] Persist remote follow/relationship state and expose it through Mastodon follow, unfollow, relationship, follower, and following APIs.
- [ ] Deliver local public `Create`, `Update`, and `Delete` activities to remote followers.
- [ ] Verify signed inbound `Create`, `Update`, and `Delete`, cache remote public Notes, and fan them into local home timelines.
- [ ] Add remote home-timeline repair, visibility semantics, and remote follower collection contents.
- [ ] Add remote replies, mentions, favourites, boosts, deletes, and notifications.
- [ ] Add remote mute/block behavior and apply domain policy to inbox processing and delivery.
- [ ] Add remote direct conversations and account migration.

## Long Term

- ActivityPub federation: signed delivery, inbound processing, remote actor/object cache, and moderation policy enforcement. Opt-in local WebFinger, actor, Note, outbox, and collection endpoints are available.
- Production-grade timelines: follow graph fan-out, repair jobs, cursor pagination, remote statuses, and scalable streaming fan-out.
- Media support: local and S3-compatible storage, validation, thumbnails, processing jobs, and remote media fetch limits.
- Moderation and operations: account suspension, local status removal, domain policy, admin tools, metrics, and audit-friendly workflows.
- Compatibility hardening: broader Mastodon API coverage, versioned response DTOs, pagination headers, scope enforcement, and client regression tests.

## Medium Term

- Social graph APIs: remote follow delivery, follow requests, and remote mute/block delivery.
- Status interactions: replies, favourites, bookmarks, boosts, and delete streaming events.
- Notifications: grouped notifications, push integration, and remote notification events.
- Account/profile APIs beyond current credentials: public account lookup, profile pages, and status collections.
- Streaming channels: `public`, `public:local`, `user`, `user:notification`, and bounded slow-client handling.

## Short Term

- [x] Harden inbound remote follow handling with signed HTTP `Date` freshness checks and activity-ID idempotency.
- [ ] Add replay protection beyond activity-ID idempotency where remote actors reuse or omit canonical activity IDs.
- Add cursor pagination and `Link` headers to remote follow-request listing.
- [x] Retry federation deliveries with exponential backoff until the operator-configured `ROOSTY_FEDERATION_DELIVERY_MAX_AGE` horizon, then record permanent failures and emit diagnostics.
- Add signature, retry, and two-instance end-to-end tests for inbound Follow, locked-account approval/rejection, Accept/Reject delivery, and Undo.
- Expand accepted remote follower collections from count-only metadata to paginated remote actor items.
- Apply local mute/block policy to remote follow requests and remote follow notifications.
- Enrich remote account projections with profile media and remote relationship/status counts as those data become available.
- Implement local-to-remote Follow and Undo(Follow) initiation and relationship state.
- Add remote public status Create/Update/Delete delivery, caching, and home-timeline fan-out.
- Add remote replies, mentions, favourites, boosts, notifications, and direct conversations.
- Fill Mastodon client startup gaps found by Elk and browser logs.
- Improve local account administration now that multiple local users can be operator-created.
- Extend the safe, policy-controlled WebFinger remote-account lookup to account search and controlled cache refresh.
- Add cursor pagination for account status collections.
- Expand local direct conversations toward remote conversation support.
- Add remote ActivityPub `Announce` support for boosts.
- Add remote hashtag discovery and featured/profile tags.
- Extend media support with video/audio validation, async processing, and object storage.
- Keep compatibility documentation updated with every implemented or intentionally deferred API.
