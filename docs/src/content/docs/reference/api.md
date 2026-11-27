---
title: "Crate API"
description: "The doko crate's main types, Ctx methods and Cargo features."
---

The library is published on crates.io as [`doko-rs`](https://crates.io/crates/doko-rs) and imported as `doko`. The full, versioned API documentation is on **[docs.rs/doko-rs](https://docs.rs/doko-rs)**. This page is a map of it.

```toml
[dependencies]
doko = { package = "doko-rs", version = "0.1" }
worker = "..."   # required directly: worker's #[event] macro expands to ::worker:: paths

[lib]
crate-type = ["cdylib"]
```

## Cargo features

| Feature | Enables | Notes |
|---|---|---|
| `pg` | `doko::Pg`, `PgStatement` and `ctx.pg(..)` | Postgres through Hyperdrive. Off by default: the driver adds to the wasm size. |
| `test` | `doko::test` | Integration tests against `wrangler dev`. Enable in `[dev-dependencies]` only. |

## The app

| Item | What |
|---|---|
| `doko::app!(App)` / `doko::app!(App, objects: [Class, ..])` | Exports the worker's fetch, scheduled, queue and email handlers, and any Durable Object classes |
| `trait Hooks` | `routes()` (required), `jobs()`, `tasks()`, `on_email(ctx, email)`, `on_error(ctx, error)` |
| `Ctx` | The request context, an axum extractor. See below |
| `Error`, `Result<T>` | JSON errors: `Error::not_found()`, `bad_request(..)`, `unauthorized()`, `forbidden()`, `Error::new(status, ..)` |
| `doko::axum`, `doko::worker` | Re-exports of the crates doko is built on |

## `Ctx` methods

| Method | Returns | Guide |
|---|---|---|
| `env_name()`, `var(name)`, `secret(name)`, `config::<T>()` | the active env, plain vars, secrets, typed config | [Config](/doko/guides/configuration/) |
| `kv(KV)`, `bucket(R2)`, `db(D1)` | `Kv`, `Bucket`, `Db` | [Storage](/doko/guides/storage/) |
| `pg(HYPERDRIVE).await` | `Pg` (feature `pg`) | [Postgres](/doko/guides/postgres/) |
| `cache()` | `Cache`: `get_or_set`, `purge_tag`, `tier` | [Cache](/doko/guides/cache/) |
| `enqueue(QUEUE, &job)`, `enqueue_in(QUEUE, &job, delay_secs)` | sends a `Job` | [Jobs and cron](/doko/guides/jobs-and-cron/) |
| `durable(DO)` | `Durable`, whose `get(name)` gives a `Stub` | [Durable Objects](/doko/guides/durable-objects/) |
| `mail(EMAIL)` | `Mailer` | [Email](/doko/guides/email/) |
| `ai()` | `Ai`: `run`, `stream` | [Workers AI](/doko/guides/workers-ai/) |
| `analytics(DATASET)` | `Analytics`: `write` | [Analytics Engine](/doko/guides/analytics-engine/) |
| `wait_until(fut)` | runs work after the response is sent | [Handlers](/doko/guides/handlers/) |
| `env()` | the raw `worker::Env` | |

## Modules and types

| Area | Items |
|---|---|
| Requests | `Page`, `Paged<T>`, `Valid<T>`, `Validate`, `Errors` |
| Routing | `RouterExt`: `.cache(Policy)`, `.rate_limit(RateLimit)`, `.require_auth()`; `Policy` |
| Storage | `Kv`, `Bucket`, `Db`, `Statement`, `bool_from_sql`, and the binding types `KvBinding`, `R2Binding`, `D1Binding`, `QueueBinding`, `DoBinding`, `EmailBinding`, `HyperdriveBinding`, `AnalyticsBinding` |
| Jobs | `Job`, `Jobs`, `Tasks` |
| Durable Objects | `DurableObject`, `Durable`, `Stub`, `accept_websocket`, `RateLimiter`, `RateLimit` |
| Auth (`doko::auth`) | `Session`, `SetCookie`, `Claims<T>`, `SessionStore`, `sign`, `hash_password`, `verify_password`, `token`, `read_token` |
| Email | `Message`, `Attachment`, `Mailer`, `InboundEmail`, `EmailBody`, `render_html`, `render_text`, `escape_html` |
| Workers AI | `Ai` |
| Analytics Engine | `Analytics`, `DataPoint`, `analytics::query` |
| Postgres (feature `pg`) | `Pg`, `PgStatement` |
| Tests (feature `test`) | `doko::test::App` |

## The CLI

The `doko` command is a separate crate, [`doko-cli`](https://crates.io/crates/doko-cli). See [The doko command](/doko/reference/cli/) and [Generators](/doko/guides/generators/).
