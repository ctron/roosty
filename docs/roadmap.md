# Roadmap

## Full Social Graph

### Available now

- Local profiles, lookup/search, follows/unfollows, relationships, followers/following, and local account moderation.
- Local public/unlisted home-timeline fan-out, replies, mentions, favourites, boosts, notifications, and direct conversations.
- Mastodon-compatible browser OAuth, including PKCE, callback redirects, and out-of-band authorization codes for CLI clients such as toot.
- Opt-in local ActivityPub identity: WebFinger, actor documents with avatar/header URLs, encrypted actor keys, public Notes, outboxes, and follower/following collection metadata.
- Safe operator-policy-controlled remote actor discovery through `resolve=true` account lookup, including WebFinger and validated actor caching. Policies can allow exact domains or all public domains with `*`, with explicit blocks taking precedence.
- Mastodon-compatible mixed account search exposes cached remote actors, resolves exact remote handles, and links through remote profiles to the locally cached public/unlisted status subset.
- Signed inbound remote-follow handling: `Follow` and `Undo(Follow)` update remote-follower state, with durable `Accept`/`Reject` responses for local actors.
- Signed outbound delivery of local public, unlisted, and follower-only status lifecycle activities (`Create`, `Update`, and `Delete`) plus local actor profile `Update` activities to accepted remote followers and explicit mentions.

### Federation gaps

- [x] Refresh expired cached remote actors during exact-handle resolution, with bounded discovery observability and cross-process deduplication.
- [x] Include cached and resolved remote accounts in Mastodon account search, with pagination and deterministic ranking.
- [x] Add signed outbound `Follow` and `Undo(Follow)` delivery, including durable delivery jobs, destination deduplication, retries, and permanent-failure diagnostics.
- [x] Process inbound `Accept` and `Reject` for locally initiated remote follows; signed inbound `Follow`/`Undo(Follow)` and locked-account requests are available.
- [x] Persist local-to-remote follow/relationship state and expose accepted local and remote relationships through Mastodon and ActivityPub follower/following collections.
- [x] Deliver local public and unlisted `Create`, `Update`, and `Delete` activities to accepted remote followers.
- [x] Verify signed inbound public/unlisted `Create`, `Update`, and `Delete` activities and cache remote Notes by canonical object ID.
- [x] Project cached remote Notes into local home timelines.
- [x] Stream cached remote Note create/update/delete events to local home timelines.
- [x] Match Mastodon's push-based federation behavior: missed inbox deliveries are not backfilled by polling remote outboxes; deletion and follow-state cache repairs remain local operations.
- [x] Implement remote follower-only Notes and replies using validated actor collection URLs, current follow relationships, explicit mentions, and cache-only traversal.
- [ ] Fetch and expose paginated remote followers/following collection contents.
- [x] Deliver and process public/unlisted replies and mentions, including recipient addressing, remote-object resolution, and local notification visibility.
- [x] Build cache-only remote reply contexts and conversation/thread traversal across local and cached remote parents, with Mastodon-compatible access limits and no outbox backfill.
- [x] Support remote reply delivery and addressing for follower-only visibility.
- [x] Deliver and process public/unlisted favourites (`Like`/`Undo`), including remote counters, local notifications, and mixed favourites collections.
- [x] Deliver and process public/unlisted boosts (`Announce`/`Undo`), including remote timeline entries, local counters, and notifications.
- [x] Commit supported inbox side effects, idempotency markers, and durable follow/favourite/boost delivery jobs atomically; publish streams only after commit.
- [x] Repair cached-status timelines, interactions, notifications, reply links, and direct-conversation projections atomically after signed remote status and actor Deletes.
- [x] Process signed Actor `Update`, `Delete`, and `Move` activities for remote profile lifecycle; moves expose a replacement account without automatically migrating follows.
- [x] Safely fetch, validate, cache, expire, and render remote image attachments, with preview metadata and stale-while-refresh proxying; video/audio remain passthrough-only.
- [ ] Federate follow, mention, reply, favourite, and boost notifications to remote recipients.
- [ ] Add grouped notifications and Web Push integration.
- [ ] Apply local mute/block and domain-policy decisions consistently to discovery, inbox processing, delivery, and notifications.
- [ ] Deliver remote mute/block activities and project their relationship state.
- [x] Complete remote direct conversations: persist mixed local/remote participants, support replies to cached direct Notes, and repair conversation state after remote updates/deletes.
- [x] Add recipient-scoped direct-message audiences, per-account last-status projection, local/remote deletion repair, and transactional notification creation.
- [x] Finish transactional remote notifications for Follow and Announce flows, matching the existing transactional mention/reply and Like handling.
- [ ] Replace stringly persisted status visibility with the typed `StatusVisibility` model at persistence and wire boundaries.
- [ ] Support remote account migration, redirects, and moved-account relationship updates.
- [ ] Add remote hashtag discovery, timelines, and featured/profile tags.
- [ ] Complete deferred Mastodon actor extensions: shared inboxes, group actors, indexability, featured collections/tags, and account migration metadata.
- [x] Enforce durable absolute-HTTPS activity IDs from the verified actor origin and reject payload/signer reuse through a canonical-JSON replay ledger.
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
- [x] Add canonical-payload replay protection for reused IDs and reject ID-less, non-HTTPS, or cross-origin durable activities.
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
- [x] Add remote profile lifecycle (`Update`, `Delete`, and `Move`) and safe remote profile-media caching.
- [ ] Fill Mastodon client startup gaps found by Elk and browser logs.
- [ ] Improve local account administration now that multiple local users can be operator-created.
- [x] Extend the safe, policy-controlled WebFinger remote-account lookup to account search and controlled cache refresh.
- [x] Add cursor pagination for local and cached-remote account status collections.
- [ ] Add remote hashtag discovery, timelines, and featured/profile tags.
- [ ] Extend media support with video/audio validation, async processing, and object storage.
- [ ] Keep compatibility documentation updated with every implemented or intentionally deferred API.
- [ ] Replace manually formatted Prometheus metrics and global atomic counters with standardized instrumentation and an exporter; evaluate OpenTelemetry for correlated metrics and traces.
- [x] Expose persisted local account creation dates through Mastodon account responses.
- [x] Harden local and remote avatar/header processing with MIME/byte validation, signed Actor `Update` delivery assertions, and stale-while-refresh proxy caching.
