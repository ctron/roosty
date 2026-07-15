# Roadmap

## Full Social Graph

### Available now

- Local profiles, lookup/search, follows/unfollows, relationships, followers/following, and local account moderation.
- Local public/unlisted home-timeline fan-out, replies, mentions, favourites, boosts, notifications, and direct conversations.
- Opt-in local ActivityPub identity: WebFinger, actor documents with avatar/header URLs, encrypted actor keys, public Notes, outboxes, and follower/following collection metadata.
- Safe operator-policy-controlled remote actor discovery through `resolve=true` account lookup, including WebFinger and validated actor caching. Policies can allow exact domains or all public domains with `*`, with explicit blocks taking precedence.
- Signed inbound remote-follow handling: `Follow` and `Undo(Follow)` update remote-follower state, with durable `Accept`/`Reject` responses for local actors.
- Signed outbound delivery of local public and unlisted status lifecycle activities (`Create`, `Update`, and `Delete`) plus local actor profile `Update` activities to accepted remote followers.

### Federation gaps

- [ ] Refresh cached remote actors before expiry, with bounded retries and observability.
- [ ] Include cached and resolved remote accounts in Mastodon account search, with pagination and ranking.
- [x] Add signed outbound `Follow` and `Undo(Follow)` delivery, including durable delivery jobs, destination deduplication, retries, and permanent-failure diagnostics.
- [x] Process inbound `Accept` and `Reject` for locally initiated remote follows; signed inbound `Follow`/`Undo(Follow)` and locked-account requests are available.
- [x] Persist local-to-remote follow/relationship state and expose accepted local and remote relationships through Mastodon and ActivityPub follower/following collections.
- [x] Deliver local public and unlisted `Create`, `Update`, and `Delete` activities to accepted remote followers.
- [x] Verify signed inbound public/unlisted `Create`, `Update`, and `Delete` activities and cache remote Notes by canonical object ID.
- [x] Project cached remote Notes into local home timelines.
- [x] Stream cached remote Note create/update/delete events to local home timelines.
- [ ] Repair remote home timelines after missed delivery, cache expiry, deletion, and follow-state changes.
- [ ] Implement remote visibility semantics beyond public/unlisted Notes, including replies and follower-only addressing.
- [ ] Fetch and expose paginated remote followers/following collection contents.
- [x] Deliver and process public/unlisted replies and mentions, including recipient addressing, remote-object resolution, and local notification visibility.
- [ ] Build remote reply contexts and conversation/thread traversal across cached local and remote parents.
- [ ] Support remote reply delivery and addressing for non-public visibility, including follower-only replies.
- [x] Deliver and process public/unlisted favourites (`Like`/`Undo`), including remote counters, local notifications, and mixed favourites collections.
- [x] Deliver and process public/unlisted boosts (`Announce`/`Undo`), including remote timeline entries, local counters, and notifications.
- [x] Commit supported inbox side effects, idempotency markers, and durable follow/favourite/boost delivery jobs atomically; publish streams only after commit.
- [ ] Repair cached-status timelines and notification references after signed remote Deletes.
- [ ] Process Actor `Update`, `Delete`, and `Move` activities for remote profile lifecycle and account migration.
- [ ] Safely fetch, validate, cache, expire, and render remote media attachments.
- [ ] Federate follow, mention, reply, favourite, and boost notifications to remote recipients.
- [ ] Add grouped notifications and Web Push integration.
- [ ] Apply local mute/block and domain-policy decisions consistently to discovery, inbox processing, delivery, and notifications.
- [ ] Deliver remote mute/block activities and project their relationship state.
- [ ] Support remote direct conversations, including encrypted/private addressing policy and timeline projection.
- [ ] Support remote account migration, redirects, and moved-account relationship updates.
- [ ] Add remote hashtag discovery, status/tag projections, and featured/profile tags.
- [ ] Complete deferred Mastodon actor extensions: shared inboxes, group actors, indexability, featured collections/tags, custom emoji, and account migration metadata.
- [ ] Add replay protection for reused or absent remote activity IDs beyond current canonical-ID idempotency.
- [ ] Support multi-process streaming fan-out and federation-worker coordination.

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
- [x] Add cursor pagination and `Link` headers to remote follow-request listing.
- [x] Retry federation deliveries with exponential backoff until the operator-configured `ROOSTY_FEDERATION_DELIVERY_MAX_AGE` horizon, then record permanent failures and emit diagnostics.
- [x] Add signed two-instance end-to-end tests for inbound Follow, locked-account approval/rejection, Accept/Reject delivery, Undo, public-status fan-out, and failed-delivery retry scheduling.
- [x] Add end-to-end worker tests for permanent-failure classification and expired-claim recovery.
- [ ] Expand accepted remote follower collections from count-only metadata to paginated remote actor items.
- [ ] Apply local mute/block policy to remote follow requests and remote follow notifications.
- [ ] Enrich remote account projections with remote relationship/status counts as those data become available.
- [x] Add remote public status Create/Update/Delete delivery to accepted remote followers.
- [x] Add signed inbound public/unlisted remote status caching with Create/Update/Delete handling.
- [x] Add public/unlisted remote replies and mentions, including addressing, object resolution, and local mention/reply notifications.
- [ ] Add remote profile lifecycle (`Update`, `Delete`, and `Move`) and safe remote media caching.
- [ ] Add remote notifications and direct conversations.
- [ ] Fill Mastodon client startup gaps found by Elk and browser logs.
- [ ] Improve local account administration now that multiple local users can be operator-created.
- [ ] Extend the safe, policy-controlled WebFinger remote-account lookup to account search and controlled cache refresh.
- [ ] Add cursor pagination for account status collections.
- [ ] Expand local direct conversations toward remote conversation support.
- [ ] Add remote hashtag discovery and featured/profile tags.
- [ ] Extend media support with video/audio validation, async processing, and object storage.
- [ ] Keep compatibility documentation updated with every implemented or intentionally deferred API.
- [x] Expose persisted local account creation dates through Mastodon account responses.
- [ ] Account images/header/avator remote processing (both directions) still seems to not work properly
