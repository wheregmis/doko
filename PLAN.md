# doko — a Rails/Loco-style framework for Cloudflare Workers (Rust)

Built on `worker` 0.9 (features `axum`, `http`, `d1`, `queue`) and `worker-build` 0.9.

## Core decisions

1. **Build on axum, not `worker::Router`.** `worker` 0.9 has an `axum` feature, which gives extractors, tower middleware and Loco-like ergonomics for free. doko is a thin layer over it, not a router of its own.
2. **`doko.toml` is the single source of truth.** It declares the app, its environments and its bindings. `doko` generates `wrangler.toml` from it plus a typed `Bindings` struct, so binding names never drift between config and code.
3. **Convention over configuration.** Fixed folders (`src/controllers`, `src/jobs`, `src/tasks`, `migrations/`) and one `Hooks` trait as the app entry point, like Loco.
4. **Hide `!Send`.** Worker JS types aren't `Send`, but axum handlers must be. Context methods return `Send` futures (via `worker::send`) so user handlers never hit this error.

## Workspace layout

```
doko/
  crates/
    doko/          # runtime lib (wasm32): App, Hooks, Ctx, storage, cache, jobs
    doko-macros/   # #[doko::app], #[doko::job], bindings codegen
    doko-cli/      # native bin: new, generate, dev, deploy, migrate
  examples/blog/
```

## Phases

### Phase 0: scaffold
- Turn the repo into the workspace above, targeting `wasm32-unknown-unknown`.
- `#[doko::app]` expands to the `#[event(fetch)]`, `#[event(scheduled)]` and `#[event(queue)]` handlers and dispatches them to the `Hooks` impl.
- `doko dev` turns `doko.toml` into `wrangler.toml`, then runs `wrangler dev`.
- **Milestone:** a `GET /` hello route deployed from `doko.toml`.

### Phase 1: Workers backend API
- `Hooks` trait: `routes() -> axum::Router<Ctx>`, `middlewares()`, `on_scheduled()`, `on_queue()`.
- `Ctx` holds `Env`, typed `Bindings`, config, and `wait_until` for fire-and-forget work.
- Typed config per environment (`[env.dev]`, `[env.prod]`). Secrets come from `env.secret()`, never the toml.
- Built-in middleware: request id, structured logging (`console_log`), CORS, JSON error type (`doko::Error` → `IntoResponse`), request timeouts.
- `doko generate controller posts` produces the module and registers its routes.

**Status (Phases 0–1 done):**
- `doko::app!(App)` is a `macro_rules!` macro, so there's no proc-macro crate yet.
- Queue events move to Phase 4.
- There's no `middlewares()` hook, since `.layer()` on the router does the same job.
- CORS is any tower layer.
- There's no request timeout, because the Workers platform enforces its own limits and tokio timers don't run on wasm.
- `typed Bindings` moves to Phase 2.

### Phase 2: storage

| Product | What doko adds on top of `worker` |
|---|---|
| KV | `ctx.kv::<T>("NAME")` with serde get/put, TTL, key prefix per env |
| R2 | `ctx.bucket("NAME")` with streaming upload/download, multipart, `FileResponse` (content-type/ETag) |
| D1 | `ctx.db()` with typed `query_as::<Row>()`, plain SQL migrations in `migrations/` run by `doko migrate` (wraps `wrangler d1 migrations`), transactions via `batch()` |

No ORM in v1: D1 is SQLite, so serde row mapping plus raw SQL covers most needs.

### Phase 3: cache
- Cache API wrapper: `ctx.cache().get_or_set(key, ttl, || async { ... })`.
- `CacheLayer` tower middleware caching GET responses by route, with TTL, `Vary`, and stale-while-revalidate via `wait_until`.
- Two-tier helper: Cache API as L1 (per colo), KV as L2 (global), configured per route in `doko.toml`.
- Invalidation by key or tag (tags stored in KV).

### Phase 4: background workers
- `#[doko::job]` on a struct implementing `perform(ctx)`, enqueued with `MyJob::enqueue(&ctx, args)`. Runs on Queues; `on_queue` routes each message to its job by name.
- Retries and dead-letter queue settings in `doko.toml`.
- Cron tasks declared in `doko.toml`, mapped to named handlers.

### Phase 5: developer experience
- `doko new <app>` templates; `generate job|migration|controller`.
- Integration tests through `wrangler dev` / Miniflare, with a `doko::test` helper that starts a local worker and an HTTP client.
- `doko deploy --env prod`.

### Later
Durable Objects (sessions, rate limiting), auth (JWT/sessions), Hyperdrive/Postgres, Workers AI, email.

## Target app

```rust
#[doko::app]
struct App;

impl Hooks for App {
    fn routes() -> Router<Ctx> {
        Router::new().route("/posts/{id}", get(posts::show))
            .layer(CacheLayer::ttl(60))
    }
}

async fn show(ctx: Ctx, Path(id): Path<i64>) -> doko::Result<Json<Post>> {
    let post = ctx.db().query_as("SELECT * FROM posts WHERE id = ?1", [id]).one().await?;
    Ok(Json(post))
}
```

## Risks
- `!Send` friction between worker types and axum handlers.
- Keeping generated `wrangler.toml` in sync with code.

Phases 0–1 must prove both before storage work starts.
