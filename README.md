# doko

A Rails/Loco-style framework for Cloudflare Workers in Rust, built on [`worker`](https://crates.io/crates/worker) + axum. See [PLAN.md](PLAN.md) for the roadmap.

## Quick start

```bash
rustup target add wasm32-unknown-unknown
cargo install --path crates/doko-cli
cd examples/hello && doko dev
```

`doko dev` / `doko deploy` regenerate `wrangler.toml` from `doko.toml`, then run wrangler. `doko config` only regenerates. Extra args go to wrangler.

## An app

```rust
use doko::{Ctx, Hooks, axum::{Router, routing::get}};

struct App;
impl Hooks for App {
    fn routes() -> Router<Ctx> {
        Router::new().route("/", get(|| async { "hello" }))
    }
}
doko::app!(App);
```

The app crate needs `crate-type = ["cdylib"]` and must depend on `worker` directly, because worker's `#[event]` macro expands to `::worker::` paths.

```toml
# doko.toml
[app]
name = "my-app"
compatibility_date = "2026-11-01"
```

## Config and environments

```toml
[config]                 # any TOML; read with ctx.config::<YourConfig>()
greeting = "hello"

[env.prod.config]        # deep-merged over [config] for `--env prod`
greeting = "hello (prod)"
```

No `--env` means `dev`, and `[env.dev.config]` applies there. `ctx.env_name()` gives the active env. Use `ctx.var(name)` for plain vars, and `ctx.secret(name)` for secrets set with `wrangler secret put` (or `.dev.vars` locally). Never put secrets in doko.toml.

## Handlers

- Take `ctx: Ctx` directly as an argument.
- Return `doko::Result<T>`. `?` on any error gives a JSON 500 whose message is logged but not sent to the client.
- Use `Error::not_found()`, `bad_request(..)`, `unauthorized()`, `forbidden()` or `Error::new(status, ..)` for `{"error": ".."}` responses.
- `ctx.wait_until(fut)` runs work after the response is sent.

Every request is logged as one JSON line, and responses carry `x-request-id` (the `cf-ray` header, or `-` locally). For CORS and other middleware, `.layer(...)` any tower layer onto your router.

## Generators

```bash
doko generate controller posts
```

This writes `src/controllers/posts.rs` and registers it in `src/controllers/mod.rs`. Add `mod controllers;` once, and `.merge(controllers::routes())` in `Hooks::routes`.

## Storage: KV, R2, D1

Declare bindings in doko.toml. The fields match wrangler's, and IDs are optional locally:

```toml
[kv.CACHE]

[r2.FILES]
bucket_name = "my-files"

[d1.DB]
database_name = "my-db"

[env.prod.kv.CACHE]       # replaces the top-level CACHE in prod
id = "<namespace id>"
```

`doko config` (and `dev`, `deploy` and `migrate`) writes every binding into every wrangler env, plus a typed `src/bindings.rs` with one constant per binding. Add `mod bindings;` once:

```rust
use crate::bindings::{CACHE, DB, FILES};

let notes: Vec<Note> = ctx.db(DB)?.all("SELECT * FROM notes WHERE owner = ?1", (owner,)).await?;
ctx.kv(CACHE)?.put_ttl("notes", &notes, 60).await?;
ctx.bucket(FILES)?.serve("logo.png").await   // streams with content-type + etag, or 404
```

| | Methods |
|---|---|
| `Kv` | `get::<T>`, `put`, `put_ttl`, `delete` (values are JSON) |
| `Bucket` | `serve`, `get_bytes`, `put(key, bytes, content_type)`, `delete` |
| `Db` | `all::<T>`, `first::<T>`, `run` (returns `changes`, `last_row_id`), `statement` + `batch` (one transaction) |

D1 params are a tuple bound to `?1`, `?2` and so on: `(id,)`, `(name, age)` or `()`. Each wrapper has `.raw()` for the full `worker` API.

### Migrations

```bash
doko generate migration create_notes    # migrations/0001_create_notes.sql
doko migrate                            # apply locally
doko migrate --remote --env prod        # apply to the prod database
```

## Cache

Two layers on the Workers Cache API, which is per data center:

```rust
use doko::{Policy, RouterExt};

// Whole GET responses, with stale-while-revalidate and tags.
Router::new()
    .route("/notes", get(index).post(create))
    .cache(Policy::ttl(60).stale_while_revalidate(300).tags(CACHE, &["notes"]))

// After a write: invalidate everything tagged "notes", in every data center.
ctx.cache().purge_tag(CACHE, "notes").await?;

// Single values.
let n = ctx.cache().get_or_set("notes:count", 30, async { count(&db).await }).await?;
```

- `.cache()` applies to the routes added before it.
- It caches only 200 GET responses with a known size of 10 MB or less and no `Set-Cookie` or `private`/`no-store` header.
- Requests with `Authorization` are never cached.
- Responses carry `x-doko-cache: HIT | STALE | MISS | BYPASS`.

**How tag purges work.** Each tag stores a version number in the KV namespace you pass, and that version is part of the cache key. A purge bumps the version, so every data center misses on its next lookup. This is global, but takes up to about 60s, which is how long KV writes take to propagate.

**The Cache API does nothing on `*.workers.dev`.** Production needs a custom domain.

## Background jobs and cron tasks

```toml
[queues.JOBS]                       # this worker sends to and consumes this queue
queue = "my-app-jobs"
max_retries = 3                     # wrangler consumer settings: max_batch_size,
dead_letter_queue = "my-app-dlq"    # max_batch_timeout, retry_delay, max_concurrency

[tasks]                             # name = cron schedule
prune = "0 3 * * *"

[env.prod.tasks]
prune = "0 4 * * *"
```

```rust
#[derive(Serialize, Deserialize)]
struct CountWords { id: i64 }

impl Job for CountWords {
    const NAME: &'static str = "count_words";   // stored in each message: don't rename casually
    async fn perform(self, ctx: Ctx) -> Result<()> { /* ... */ Ok(()) }
}

impl Hooks for App {
    fn jobs() -> Jobs { Jobs::new().add::<CountWords>() }
    fn tasks() -> Tasks { Tasks::new().add("prune", prune) }   // async fn prune(ctx: Ctx) -> Result<()>
    // ...
}

ctx.enqueue(JOBS, &CountWords { id }).await?;          // or enqueue_in(JOBS, &job, delay_secs)
```

- **Failed jobs.** A job that returns `Err` is retried, and after `max_retries` it moves to the `dead_letter_queue`. Make `perform` safe to run more than once.
- **Job logs.** Every attempt is logged as JSON, with the job name and attempt number.
- **Cron triggers.** doko writes each env's cron triggers from its `[tasks]` schedules. On every trigger, it runs each task whose schedule matches.
- **Testing a task locally:** `curl "localhost:8787/cdn-cgi/handler/scheduled?cron=0+3+*+*+*"`.
