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
| 1. Backend API | `Hooks::routes()`, `Ctx` (env, config, `wait_until`), per-env config, request id, logging, `doko::Error` → JSON, `generate controller` | No request timeout (platform enforces limits); middleware is plain `.layer()` |
| 2. Storage | `ctx.kv::<T>()`, `ctx.bucket()`, `ctx.db()` with `query_as`, `batch()`, `doko migrate` | R2 uploads buffered, multipart only via `.raw()`; no per-env KV prefix |
| 3. Cache | `get_or_set`, `.cache(Policy::...)` on routes, stale-while-revalidate, tag purges | No `Vary`; no KV second tier |
| 4. Background work | `Job` trait + `enqueue`, queue dispatch by `NAME`, retries/DLQ, named cron `Tasks` | Batches run one message at a time; queue is always produced and consumed by the same worker |
| 5. DX | `doko new`, `generate job\|migration\|controller`, `doko::test::App`, `doko deploy --env` | Depends on doko by git; tests need Node and `--ignored` |
| 6. Durable Objects | `[do.NAME]` with generated bindings and append-only class migrations, `DurableObject` trait exported by `doko::app!(App, objects: [..])`, `ctx.durable(NAME)?.get(name)?.fetch(req)`, `RateLimiter` + `.rate_limit(RateLimit::new(..))` | Stubs speak `fetch` only (no RPC helper); no WebSocket handlers; limits key on client IP only; class deletions and renames are hand-written migrations |
| 7. Auth | `doko::auth`: `Session` extractor (signed cookie, KV or `SessionStore` object), `Claims<T>` for HS256/RS256 JWTs, `sign()`, `hash_password`/`verify_password`, `.require_auth()` | Wired by fixed names (`SESSIONS`, `SESSION_SECRET`, `JWT_SECRET`, `JWT_PUBLIC_KEY`); session TTL fixed at 7 days; one RS256 key, no JWKS or `iss`/`aud` checks; no roles |
| 10. Email | `[email.NAME]` → `send_email` binding, `ctx.mail(NAME)?.send(&Message { .. })`, `Hooks::on_email` with `InboundEmail::read()` (decoded subject, text, html), `generate mailer` | Plain addresses only; attachments, cc/bcc, custom headers and replies via `.raw()`; no template engine; routing rules are set up outside doko |

## Next

Each phase ends with the `hello` example using the feature and an integration test through `doko::test::App`.

Phases 8 and 9 are on hold until the foundation is solid: hardening and the backlog below come first.

### Phase 8: Hyperdrive / Postgres
- `[hyperdrive.NAME]` binding; `ctx.pg("NAME")` giving a connection over the Hyperdrive connection string.
- Same `query_as::<Row>()` shape as D1, so handlers read the same either way.
- Migrations for Postgres in `migrations/pg/`, run by `doko migrate --pg`.
- **Milestone:** one query against a local Postgres through `wrangler dev`.

### Phase 9: Workers AI
- `[ai]` binding; `ctx.ai().run::<In, Out>(model, input)` with serde types, plus streaming text responses as an axum body.
- **Milestone:** a summarize route in `hello`.

## Backlog

Limits from the shipped phases, picked up when a user needs them:
- Publish to crates.io so `doko new` stops depending on git.
- Streaming R2 uploads and a multipart helper.
- `Vary` support in the response cache.
- Concurrent processing of queue batches.
- Rate limit keys other than client IP (per user), and WebSocket handlers on Durable Objects.
- Email: named addresses, attachments and cc/bcc on `Message`; an HTML-escaping helper or templates for mailers.
- Auth: configurable session TTL and cookie name, JWKS with key rotation, `iss`/`aud` checks.
- A proc-macro crate, only if a feature can't be done with `macro_rules!` and traits.

## Risks
- `!Send` friction grows with each new binding (DO stubs, Postgres sockets); every accessor must keep returning `Send` futures.
- Generated `wrangler.toml` must stay in sync as DO classes and migrations are added.
- Postgres drivers on wasm32 may need a fork or a TCP-socket shim; prove this before building Phase 8 on top.
