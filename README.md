# Roosty

[![CI](https://github.com/ctron/roosty/actions/workflows/ci.yml/badge.svg)](https://github.com/ctron/roosty/actions/workflows/ci.yml)
[![GitHub Release](https://img.shields.io/github/v/release/ctron/roosty?include_prereleases&sort=semver)](https://github.com/ctron/roosty/releases)

Roosty is a federated social server written in Rust. It implements ActivityPub federation and Mastodon-compatible
client APIs, uses PostgreSQL for durable multi-process operation, and includes a server-rendered Rust/WebAssembly
frontend alongside support for existing Mastodon clients.

To see Roosty on the fediverse, [follow Rita at `@rita@roosty.de`](https://openfollow.social/@rita@roosty.de).

## Project state

Roosty is early-stage and under active development. Federation, timelines, follows, posts, interactions,
notifications, moderation, OAuth, streaming, and Web Push are available across a growing Mastodon-compatible API
surface.

Compatibility is not complete. Explore and trends, scheduled posts, public registration, and parts of the
administration API are still missing. The first-party frontend covers instance information, account flows, an
operations-first administrator dashboard, and server-rendered public profiles and status threads. APIs and deployment
expectations may still change between releases.

See the [documentation](https://ctron.github.io/roosty/) and
[compatibility matrix](https://ctron.github.io/roosty/development/compatibility.html) for detailed client and
federation coverage, and the
[roadmap](docs/roadmap.md) for planned work.

Development, verification, deployment, and release instructions are in [CONTRIBUTING.md](CONTRIBUTING.md).
