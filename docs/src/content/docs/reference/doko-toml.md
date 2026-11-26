---
title: "doko.toml reference"
description: "Every table and key doko.toml accepts."
---

`doko.toml` is the one file you maintain. `doko config` (and `dev`, `deploy` and `migrate`) turns it into `wrangler.toml` and a typed `src/bindings.rs`. Unknown keys are rejected with a message naming them.

## Top-level tables

| Table | What | Guide |
|---|---|---|
| `[app]` | `name`, `compatibility_date` (both required), `request_timeout_secs`, `renamed_classes`, `deleted_classes` | [Your first app](/doko/getting-started/first-app/), [Durable Objects](/doko/guides/durable-objects/) |
| `[config]` | any TOML, read with `ctx.config::<T>()` | [Config](/doko/guides/configuration/) |
| `[secrets]` | `required = [..]`: names of the secrets the app needs, never their values | [Config](/doko/guides/configuration/) |
| `[tasks]` | `name = "cron schedule"`, run by `Hooks::tasks` | [Jobs and cron](/doko/guides/jobs-and-cron/) |
| `[auth]` | `session_ttl_secs`, `session_cookie`, `jwt_issuer`, `jwt_audience`, `jwks_url` | [Auth](/doko/guides/auth/) |
| `[middleware]` | `secure_headers`, `body_limit_bytes`, and `[middleware.cors]` | [Handlers](/doko/guides/handlers/#middleware) |
| `[wrangler]` | any wrangler setting doko has no table for, copied as written | [Config](/doko/guides/configuration/#other-wrangler-settings) |
| `[env.<name>]` | per-environment overrides of the tables above | see below |

## Bindings

Each binding is a table named after the binding: `[kv.CACHE]` becomes the constant `CACHE` in `src/bindings.rs`.

| Table | Keys | Used with |
|---|---|---|
| `[kv.NAME]` | `id` (optional locally) | `ctx.kv(NAME)` |
| `[r2.NAME]` | `bucket_name` | `ctx.bucket(NAME)` |
| `[d1.NAME]` | `database_name`, `database_id` (optional locally), `migrations_dir` | `ctx.db(NAME)` |
| `[queues.NAME]` | `queue`, `consume` (default `true`), `max_batch_size`, `max_batch_timeout`, `max_retries`, `retry_delay`, `dead_letter_queue`, `max_concurrency` | `ctx.enqueue(NAME, &job)` |
| `[do.NAME]` | `class_name` | `ctx.durable(NAME)` |
| `[email.NAME]` | `destination_address`, `allowed_destination_addresses`, `allowed_sender_addresses`, `remote` | `ctx.mail(NAME)` |
| `[hyperdrive.NAME]` | `id`, `local_connection_string` | `ctx.pg(NAME)` (feature `pg`) |
| `[ai]` | no keys | `ctx.ai()` |

## Environments

No `--env` means `dev`. Under `[env.<name>]`:

| Table | How it combines with the top level |
|---|---|
| `[env.<name>.config]` | deep-merged over `[config]` |
| `[env.<name>.middleware]` | deep-merged over `[middleware]` |
| `[env.<name>.wrangler]` | deep-merged over `[wrangler]` |
| `[env.<name>.tasks]` | each task replaces the top-level schedule of the same name |
| `[env.<name>.auth]` | each key replaces the top-level `[auth]` key of the same name |
| `[env.<name>.kv.NAME]` and other bindings | each binding replaces the top-level one of the same name |

## A full example

```toml
[app]
name = "my-app"
compatibility_date = "2026-11-01"
request_timeout_secs = 30

[config]
greeting = "hello"

[secrets]
required = ["STRIPE_KEY"]

[kv.CACHE]

[d1.DB]
database_name = "my-db"

[queues.JOBS]
queue = "my-app-jobs"
max_retries = 3
dead_letter_queue = "my-app-dlq"

[tasks]
prune = "0 3 * * *"

[env.prod.config]
greeting = "hello (prod)"

[env.prod.kv.CACHE]
id = "<namespace id>"
```
