# doko roadmap

doko is a Rails/Loco-style framework for Cloudflare Workers in Rust, built on `worker` 0.9 (features `axum`, `http`, `d1`, `queue`) and `worker-build` 0.9.

## Core decisions

1. **Build on axum, not `worker::Router`.** doko is a thin layer over axum's extractors and tower middleware, not a router of its own.
2. **`doko.toml` is the single source of truth.** It declares the app, its environments and its bindings. `doko` generates `wrangler.toml` and typed binding constants from it, so names never drift between config and code.
3. **Convention over configuration.** Fixed folders (`src/controllers`, `src/jobs`, `src/tasks`, `migrations/`) and one `Hooks` trait as the entry point.
4. **Hide `!Send`.** Context methods return `Send` futures (via `worker::send`) so handlers never hit the `!Send` error.
5. **New products follow the same pattern:** a `[product.NAME]` table in `doko.toml`, a generated binding constant, a `ctx.product("NAME")` accessor, and a generator in `doko-cli` where one helps.

## Shipped

| Phase | What landed | Known limits |
|---|---|---|
| 0. Scaffold | Workspace, `doko::app!(App)`, `doko dev` generating `wrangler.toml` | `app!` is `macro_rules!`; no proc-macro crate |
| 1. Backend API | `Hooks::routes()`, `Ctx` (env, config, `wait_until`), per-env config, request id, logging, `doko::Error` → JSON, `generate controller`, `[app] request_timeout_secs` (504) | The timeout covers the time to the response's headers, not a streamed body; middleware is plain `.layer()` |
| 2. Storage | `ctx.kv::<T>()`, `ctx.bucket()`, `ctx.db()` with `query_as`, `batch()`, `doko migrate`, `Bucket::put_stream` (streamed, multipart past 8 MiB) | Resumable (client-driven) multipart only via `.raw()`; no R2 range requests in `serve` |
| 3. Cache | `get_or_set`, `.cache(Policy::...)` on routes, stale-while-revalidate, tag purges, `Policy::vary` `Cache::tier` (KV behind the value cache) | A response whose own `Vary` the policy doesn't cover is passed through uncached rather than keyed; the KV tier is for values, not whole responses |
| 4. Background work | `Job` trait + `enqueue`, queue dispatch by `NAME`, retries/DLQ, named cron `Tasks`, concurrent batches, `consume = false` producer-only queues | No consumer-only queues (every `[queues.*]` is also a producer binding); no per-job concurrency cap below `max_batch_size` |
| 5. DX | `doko new`, `generate job\|migration\|controller`, `doko::test::App`, `doko deploy --env`, published as `doko-rs` and `doko-cli` on crates.io | The library is `doko-rs` on crates.io (`doko` was taken); tests need Node and `--ignored` |
| 6. Durable Objects | `[do.NAME]` with generated bindings and append-only class migrations, `DurableObject` trait exported by `doko::app!(App, objects: [..])`, `ctx.durable(NAME)?.get(name)?.fetch(req)`, `RateLimiter` + `.rate_limit(RateLimit::new(..).key_by(..))`, hibernating WebSockets (`accept_websocket`, `websocket_message/close/error`) `Stub::call` (JSON over `fetch`), `[app] renamed_classes` / `deleted_classes` | `Stub::call` is JSON over `fetch`, not Workers JS RPC (that needs per-method exports, so a proc macro) |
| 7. Auth | `doko::auth`: `Session` extractor (signed cookie, KV or `SessionStore` object), `Claims<T>` for HS256/RS256 JWTs, `sign()`, `hash_password`/`verify_password`, `.require_auth()`, `[auth]` in doko.toml (session TTL and cookie name, `iss`/`aud`, JWKS with key rotation), `.require_role()` | Binding and secret names are fixed by convention (`SESSIONS`, `SESSION_SECRET`, `JWT_SECRET`, `JWT_PUBLIC_KEY`); RS256 only for third-party tokens (no ES256); roles are read from `role`/`roles`, with no hierarchy or permissions |
| 10. Email | `[email.NAME]` → `send_email` binding, `ctx.mail(NAME)?.send(&Message { .. })` with cc/bcc, headers, attachments and named `from`/`reply_to`, `escape_html`, `render_html` / `render_text` templates, `Hooks::on_email` with `InboundEmail::read()` (decoded subject, text, html, attachments), `generate mailer` | Recipients are plain addresses (the binding takes no names for them); inline images and replies via `.raw()`; templates substitute values only (no loops or conditions); routing rules are set up outside doko |
| 8. Postgres | `[hyperdrive.NAME]`, `ctx.pg(NAME).await?` behind the `pg` feature with D1's `all`/`first`/`run`/`statement`+`batch`, `migrations/pg/` with `doko migrate --pg` and `generate migration <name> --pg` | `numeric`, arrays, intervals and timestamp params need a cast in SQL; one connection per `ctx.pg()` call (Hyperdrive pools them); migrations need `psql`; no interactive transactions beyond `batch` |
| 9. Workers AI | `[ai]`, `ctx.ai()?.run::<In, Out>(model, &input)`, `.stream(..)` as server-sent events | No local inference: `doko dev` and tests call Cloudflare and are billed; image and audio models via `.raw()`; no AI Gateway or Vectorize bindings |
| 11. Escape hatch | `[wrangler]` and `[env.NAME.wrangler]` copied into `wrangler.toml` (routes and custom domains, assets, placement, limits, service bindings), `[wrangler.vars]` for plain vars, `[observability]` on by default | Keys doko writes itself are rejected, not merged; the list of keys wrangler allows only at the top level is kept by hand |

## Next

The products are covered; the next phases build the framework layer around them, where Rails and Loco are still ahead. Each ends, as before, with the `hello` example using the feature and an integration test through `doko::test::App`.

| Phase | Goal | Scope |
|---|---|---|
| 12. Requests | Safe, uniform request handling without tower knowledge | A validating JSON extractor (422 with per-field errors, in doko's error shape); `[middleware]` in doko.toml for CORS, secure headers and a body-size limit, with safe defaults; a built-in `/_health`; `Hooks::on_error` for error reporting |
| 13. `generate auth` | A working account system from one command | Users migration, register, login, logout, verify-email and forgot/reset-password controllers with their mailers, rate-limited, built on `doko::auth` |
| 14. `generate scaffold` | A resource from one command | `doko generate scaffold post title:string body:text`: migration, a model in `src/models/` as plain SQL functions (no ORM), CRUD controller, test; a pagination helper |
| 15. Tests | Tests that are easy to write and run | `doko test` (hides `-- --ignored`), seed files, a cookie jar / login helper, assertions on sent mail and enqueued jobs |
| 16. CLI | The tooling around an app | `doko routes`, `doko doctor` (wrangler, Node, wasm target, `psql`, missing ids and secrets per env), `doko db seed\|reset`, `doko console` (a SQL shell), `[secrets] required = [..]` checked before deploy, pending migrations applied (or refused) on deploy |

## Backlog

More batteries, picked up after the phases above or when a user needs them:
- Bindings `worker` 0.9 already has: service bindings, Analytics Engine, Secrets Store, the native rate-limit binding (a cheaper default than a Durable Object per client).
- A shipped `Channel` Durable Object with `ctx.channel(ROOMS, name).broadcast(&msg)`, next to `RateLimiter` and `SessionStore`.
- Jobs: a dead-letter hook, a failed-jobs listing, idempotency keys.
- Storage: presigned upload and download URLs; range requests in `serve`.
- HTML views: document askama or minijinja for views and mailers rather than growing `render_html`.
- A guides site beyond the README.
- Workflows, Vectorize and Images, once `worker` binds them.

Not planned: an ORM, a REPL console, an asset pipeline, i18n.

Limits from the shipped phases, picked up when a user needs them:
- ES256 JWTs; an integration test for JWKS.
- Postgres: more column and param types (`numeric`, arrays, timestamps as params).
- AI Gateway and Vectorize bindings.
- A proc-macro crate, only if a feature can't be done with `macro_rules!` and traits. Typed Workers RPC for Durable Objects is the first candidate.

## Risks
- `!Send` friction grows with each new binding (DO stubs, Postgres sockets); every accessor must keep returning `Send` futures.
- Generated `wrangler.toml` must stay in sync as DO classes and migrations are added.
- `tokio-postgres` runs on wasm32 over `worker`'s socket with its `js` feature; a major release of either could break that pairing.
