<p align="center">
  <img src="https://raw.githubusercontent.com/wheregmis/doko/master/assets/doko.svg" width="128" height="128" alt="doko logo: a woven bamboo basket carried by one strap">
</p>

<h1 align="center">doko</h1>

<p align="center"><b>Put everything in the basket. Carry it with one strap.</b><br>
A batteries-included Rust framework for Cloudflare Workers.</p>

---

A **doko** (डोको) is the woven bamboo basket carried across the hills of Nepal. Firewood, grain, a week of supplies: whatever the load, it all goes in the basket, and a single strap, the *namlo*, carries the weight.

doko works the same way for Rust on Cloudflare Workers. Routing, config, storage, caching, background jobs, cron, Durable Objects, auth, email, Postgres and AI all go in the basket. You carry one `doko.toml`, and doko writes the `wrangler.toml`, typed bindings and glue code you'd otherwise maintain by hand.

**What's in the basket**

| | |
|---|---|
| [Routing and handlers](#handlers) | axum routers, a `Ctx` extractor, JSON errors, request logs, validation, CORS and secure headers, an error hook |
| [Config and environments](#config-and-environments) | typed config merged per env, vars and secrets, any wrangler setting through `[wrangler]` |
| [Storage](#storage-kv-r2-d1) | typed KV, R2 and D1 bindings, plus D1 migrations |
| [Cache](#cache) | response caching with stale-while-revalidate and global tag purges |
| [Jobs and cron](#background-jobs-and-cron-tasks) | Queues-backed jobs with retries and a dead-letter queue, named cron tasks |
| [Durable Objects](#durable-objects-and-rate-limits) | a `DurableObject` trait, typed stubs, and a per-route rate limiter built on them |
| [Auth](#auth) | cookie sessions, JWTs, password hashing, `.require_auth()` |
| [Email](#email) | sending through a typed binding, receiving in `Hooks::on_email` |
| [Postgres](#postgres) | Hyperdrive-backed queries in D1's shape, with migrations (feature `pg`) |
| [Workers AI](#workers-ai) | `ctx.ai()` with serde types, and streamed answers |
| [Generators](#generators) | `doko new`, `generate auth \| controller \| job \| mailer \| migration` |
| [Integration tests](#integration-tests) | `doko::test` runs your worker under `wrangler dev` |

doko is built on [`worker`](https://crates.io/crates/worker) and axum, in the spirit of Rails and Loco. See [ROADMAP.md](https://github.com/wheregmis/doko/blob/master/ROADMAP.md) for the roadmap.

## Quick start

```bash
rustup target add wasm32-unknown-unknown
cargo install doko-cli
doko new my-app && cd my-app && doko dev
```

The library is published as `doko-rs` (the name `doko` was taken) and imported as `doko`; `doko new` writes the dependency for you:

```toml
doko = { package = "doko-rs", version = "0.1" }
```

To run the example from a checkout instead: `cargo install --path crates/doko-cli && cd examples/hello && doko dev`.

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
request_timeout_secs = 30    # optional: slower requests get a 504
```

Workers limit CPU time, not waiting, so without `request_timeout_secs` a handler stuck on a slow upstream holds its request open. The timeout cancels the handler and covers the time until the response starts, not a streamed body.

## Config and environments

```toml
[config]                 # any TOML; read with ctx.config::<YourConfig>()
greeting = "hello"

[env.prod.config]        # deep-merged over [config] for `--env prod`
greeting = "hello (prod)"
```

No `--env` means `dev`, and `[env.dev.config]` applies there. `ctx.env_name()` gives the active env. Use `ctx.var(name)` for plain vars, and `ctx.secret(name)` for secrets set with `wrangler secret put` (or `.dev.vars` locally). Never put secrets in doko.toml.

### Other wrangler settings

doko writes `wrangler.toml`, so a wrangler setting doko has no table of its own for goes under `[wrangler]`. It is copied into `wrangler.toml` as written, with wrangler's own key names:

```toml
[wrangler]
workers_dev = false
compatibility_flags = ["nodejs_compat"]

[wrangler.vars]                  # plain vars, read with ctx.var("SUPPORT_EMAIL")
SUPPORT_EMAIL = "help@example.com"

[wrangler.assets]                # static files served from ./public
directory = "public"

[env.prod.wrangler]              # deep-merged over [wrangler] for `--env prod`
routes = [{ pattern = "app.example.com", custom_domain = true }]
```

- **Every env gets the merged result**, like bindings do, so a service binding or var declared once under `[wrangler]` is not lost in `prod`. Tables are merged key by key; arrays and plain values are replaced.
- **Logs are on.** doko writes `[observability] enabled = true` unless `[wrangler.observability]` says otherwise.
- **What doko already writes can't be set here:** the name, bindings that have a doko table (`[kv.*]`, `[d1.*]` and the rest), `triggers`, `migrations`, and vars starting with `DOKO_`. doko.toml is rejected with a message naming the key.
- Write var values as strings: `ctx.var` reads text.

## Handlers

- Take `ctx: Ctx` directly as an argument.
- Return `doko::Result<T>`. `?` on any error gives a JSON 500 whose message is logged but not sent to the client.
- Use `Error::not_found()`, `bad_request(..)`, `unauthorized()`, `forbidden()` or `Error::new(status, ..)` for `{"error": ".."}` responses.
- `ctx.wait_until(fut)` runs work after the response is sent.

Every request is logged as one JSON line, and responses carry `x-request-id` (the `cf-ray` header, or `-` locally). `GET /_health` answers `{"ok": true}` without touching your routes or their auth, for uptime checks. For other middleware, `.layer(...)` any tower layer onto your router.

### Validation

```rust
use doko::{Errors, Valid, Validate};

#[derive(Deserialize)]
struct NewUser { email: String, password: String }

impl Validate for NewUser {
    fn validate(&self, errors: &mut Errors) {
        errors.check("email", self.email.contains('@'), "must be an email address");
        errors.check("password", self.password.len() >= 8, "must be at least 8 characters");
    }
}

async fn signup(ctx: Ctx, Valid(user): Valid<NewUser>) -> Result<StatusCode> {
    // Rules that need the database:
    let mut errors = Errors::new();
    errors.check("email", !taken(&ctx, &user.email).await?, "is taken");
    errors.finish()?;
    // ...
}
```

`Valid<T>` reads a JSON body like axum's `Json<T>`, then runs `validate`. Broken rules get a 422:

```json
{ "error": "validation failed", "fields": { "password": ["must be at least 8 characters"] } }
```

A body that isn't JSON for `T` is answered in the same `{"error": ..}` shape, where axum's own `Json<T>` answers in plain text. Rules are plain Rust: there is no derive.

### Middleware

On by default, and tuned under `[middleware]` (`[env.<name>.middleware]` is deep-merged over it):

```toml
[middleware]
secure_headers = false          # default: on
body_limit_bytes = 1048576      # default: 2 MiB

[middleware.cors]               # default: no CORS headers
origins = ["https://app.example.com"]   # exact origins, or ["*"] for a public API
credentials = true                      # cookies cross-origin; refused with "*"
# methods, headers, max_age_secs
```

- **Secure headers.** `x-content-type-options: nosniff`, `x-frame-options: SAMEORIGIN` and `referrer-policy: strict-origin-when-cross-origin` on every response that doesn't set its own.
- **Body limit.** Caps the bodies that `Valid`, `Json` and `Bytes` hold in memory. A handler that streams its `Body`, like an upload through `put_stream`, is not limited.
- **CORS.** Preflights are answered by doko and never reach a handler. Without `headers`, any header the browser asks for is allowed.

### Reporting errors

```rust
impl Hooks for App {
    async fn on_error(ctx: Ctx, error: Error) {
        // error.status(), error.message(): send them to an error tracker or a queue
    }
    // ...
}
```

`on_error` gets every request answered with a 5xx, with the message the client didn't see. It runs after the response is sent, so it can't slow down or break the error response. A request cut off by `request_timeout_secs`, and failed jobs and tasks, are logged but don't reach it.

## Generators

```bash
doko generate controller posts
```

This writes `src/controllers/posts.rs` and registers it in `src/controllers/mod.rs`. Add `mod controllers;` once, and `.merge(controllers::routes())` in `Hooks::routes`.

```bash
doko generate job send_welcome
```

This writes `src/jobs/send_welcome.rs` (a `SendWelcome` job) and registers it in `src/jobs/mod.rs`. Add `mod jobs;` once, and return `jobs::jobs()` from `Hooks::jobs`.

```bash
doko generate mailer welcome
```

This writes `src/mailers/welcome.rs`, a function that builds a `Message`, and declares it in `src/mailers/mod.rs`. Add `mod mailers;` once. See [Email](#email).

```bash
doko generate auth
```

This writes a working account system and wires it into the app:

| File | What |
|---|---|
| `src/controllers/auth.rs` | `POST /auth/register`, `GET /auth/verify`, `POST /auth/login`, `POST /auth/logout`, `GET /auth/me`, `POST /auth/forgot-password`, `POST /auth/reset-password` |
| `src/mailers/auth.rs` | the verification and password-reset mail |
| `migrations/NNNN_create_users.sql` | the `users` table |
| `tests/auth.rs` | the flow as an integration test |

It also adds what those need: the `[d1.*]`, `[email.*]`, `[kv.SESSIONS]` and `RateLimiter` bindings it doesn't find in doko.toml, `APP_URL` under `[wrangler.vars]`, a `SESSION_SECRET` in `.dev.vars`, and the modules and `RateLimiter` export in `src/lib.rs`. Whatever it can't add safely it prints as a `todo:` line. Then `doko migrate && doko dev`.

- Register and forgot-password answer the same whether or not the address has an account, and a failed login takes as long as a successful one, so accounts can't be probed.
- Register, login and both password routes are limited to 10 requests a minute per client.
- A reset link works once and for an hour. It points at `APP_URL/reset-password?token=..`: that page is your frontend's, and it POSTs the token and the new password to `/auth/reset-password`.
- Before deploying, run `wrangler secret put SESSION_SECRET` and set `FROM` in the mailer to an address on your mail domain.
- The code is yours after that: the generator runs once and doesn't update it.

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
| `Bucket` | `serve`, `get_bytes`, `put(key, bytes, content_type)`, `put_stream(key, body, content_type)`, `delete` |
| `Db` | `all::<T>`, `first::<T>`, `run` (returns `changes`, `last_row_id`), `statement` + `batch` (one transaction) |

D1 params are a tuple bound to `?1`, `?2` and so on: `(id,)`, `(name, age)` or `()`. Each wrapper has `.raw()` for the full `worker` API.

**Uploads.** `put_stream` takes the handler's `body: Body` and stores it as it arrives, holding at most 8 MiB in memory. Bodies larger than that go to R2 as a multipart upload, a part at a time; if the body fails midway, nothing is stored.

**Environments.** Each wrangler env has its own namespaces, buckets and databases, so keys need no per-env prefix: give each env its own `id` / `bucket_name` / `database_id`.

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
- A response whose own `Vary` header names something the policy's `.vary()` doesn't is never cached, so a variant can't be served to the wrong request.
- `.vary(&["accept-language"])` on the policy keeps one copy per value of those request headers and names them in the response's `Vary`. Pick headers with few distinct values.
- Responses carry `x-doko-cache: HIT | STALE | MISS | BYPASS`.

**How tag purges work.** Each tag stores a version number in the KV namespace you pass, and that version is part of the cache key. A purge bumps the version, so every data center misses on its next lookup. This is global, but takes up to about 60s, which is how long KV writes take to propagate.

**The Cache API does nothing on `*.workers.dev`.** Production needs a custom domain.

**A KV tier for values.** `ctx.cache().tier(CACHE).get_or_set(..)` also writes the value to KV and answers a local miss from it, so a value computed in one data center is reused in all of them, and value caching works on `*.workers.dev`. KV keeps a value for at least 60 seconds, and each local miss costs a KV read.

## Background jobs and cron tasks

```toml
[queues.JOBS]                       # this worker sends to and consumes this queue
                                    # (consume = false: another worker consumes it)
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
- **Batches.** The jobs of one batch run concurrently, and each is acked or retried on its own, so jobs must not depend on each other's order. `max_batch_size` caps how many run at once.
- **Job logs.** Every attempt is logged as JSON, with the job name and attempt number.
- **Cron triggers.** doko writes each env's cron triggers from its `[tasks]` schedules. On every trigger, it runs each task whose schedule matches.
- **Testing a task locally:** `curl "localhost:8787/cdn-cgi/handler/scheduled?cron=0+3+*+*+*"`.

## Durable Objects and rate limits

```toml
[do.COUNTER]              # binding name
class_name = "Counter"    # the Rust type you export
```

```rust
use doko::{DurableObject, Result, axum::{body::Body, extract::Request, http::Response, response::IntoResponse}};
use worker::{Env, durable::State};

struct Counter(State);

impl DurableObject for Counter {
    fn new(state: State, _env: Env) -> Self { Self(state) }

    async fn fetch(&self, _req: Request) -> Result<Response<Body>> {
        let n = self.0.storage().get::<u64>("n").await?.unwrap_or(0) + 1;
        self.0.storage().put("n", n).await?;
        Ok(n.to_string().into_response())
    }
    // Optional: async fn alarm(&self) -> Result<()>
}

doko::app!(App, objects: [Counter]);    // exports the class

// In a handler. Each name is its own object; the URL's host is ignored.
let res = ctx.durable(COUNTER)?.get("page:/home")?.fetch(Request::get("https://do/").body(Body::empty())?).await?;
```

- **Migrations.** Wrangler needs a migration for every new class. doko appends one to `wrangler.toml` when a `[do.*]` class first appears, and keeps the earlier ones, so **commit `wrangler.toml`**: it is the migration history.
- **Removing or renaming a class.** Deleting a class deletes everything its objects stored, so doko stops when a class disappears from doko.toml until you say which it is, under `[app]`:

  ```toml
  [app]
  renamed_classes = [{ from = "Counter", to = "Tally" }]   # keeps the data
  deleted_classes = ["Room"]                               # destroys the data on the next deploy
  ```

  Each entry is applied once, so it can stay in doko.toml.
- **Calling an object with JSON.** `let n: u64 = stub.call("add", &5).await?` POSTs the argument to `/add` of the object's `fetch` and deserializes the reply; an error reply becomes an `Err` with its status.
- New classes use SQLite-backed storage.

### WebSockets

An object can hold WebSockets, which is how chat rooms, presence and live updates are built. The sockets hibernate: an idle object is evicted from memory while its clients stay connected.

```rust
use worker::{WebSocket, WebSocketIncomingMessage};

impl DurableObject for Room {
    fn new(state: State, _env: Env) -> Self { Self(state) }

    async fn fetch(&self, req: Request) -> Result<Response<Body>> {
        doko::accept_websocket(&self.0, &req)             // 101 with the client's socket, or 426
    }

    async fn websocket_message(&self, _ws: WebSocket, message: WebSocketIncomingMessage) -> Result<()> {
        if let WebSocketIncomingMessage::String(text) = message {
            for peer in self.0.get_websockets() { peer.send_with_str(&text)?; }
        }
        Ok(())
    }
    // Optional: websocket_close, websocket_error
}

// The route passes the upgrade request to the room's object unchanged.
async fn join(ctx: Ctx, Path(room): Path<String>, req: Request) -> Result<Response<Body>> {
    ctx.durable(ROOMS)?.get(&room)?.fetch(req).await
}
```

The object may have slept since the last message, so keep per-socket state in storage or in `ws.serialize_attachment(..)`, not in fields. See [examples/hello/src/controllers/rooms.rs](https://github.com/wheregmis/doko/blob/master/examples/hello/src/controllers/rooms.rs).

### Rate limits

`RateLimiter` is a Durable Object that ships with doko. Bind and export it, then limit routes the same way you cache them:

```toml
[do.RATE_LIMITER]
class_name = "RateLimiter"
```

```rust
use doko::{RateLimit, RateLimiter, RouterExt};

Router::new()
    .route("/login", post(login))
    // 5 requests per 60s per client IP. "login" keeps this count apart from other limits.
    .rate_limit(RateLimit::new(RATE_LIMITER, "login", 5, 60))

doko::app!(App, objects: [RateLimiter]);
```

- `.rate_limit()` applies to the routes added before it, and counts them together.
- Over the limit, requests get a 429 `{"error": "too many requests"}` with `retry-after`.
- The window is fixed and starts at a client's first request. Each client's counter lives in its own object and is deleted when the window ends.
- The client is the `cf-connecting-ip` header. Local dev has none, so all local requests share one counter.
- `.key_by(|req| ..)` counts per API key, tenant or user instead: `RateLimit::new(..).key_by(|req| req.headers().get("x-api-key")?.to_str().ok().map(str::to_owned))`. Requests it returns `None` for are counted by IP. Key only on values a client can't change at will.
- If the limiter can't be reached, requests get a 500 instead of passing unlimited.

## Auth

`doko::auth` is wired by name, so there is nothing to configure in code:

| Feature | Needs |
|---|---|
| Sessions | secret `SESSION_SECRET`, and `[kv.SESSIONS]` in doko.toml |
| JWTs (HS256, issued by you) | secret `JWT_SECRET` |
| JWTs (RS256, issued elsewhere) | secret `JWT_PUBLIC_KEY` (the issuer's public key as PEM), or `jwks_url` below |

Set secrets with `wrangler secret put`, or in `.dev.vars` locally. The optional `[auth]` table tunes the rest, and `[env.<name>.auth]` overrides it per key:

```toml
[auth]
session_ttl_secs = 86400            # default: 7 days
session_cookie = "sid"              # default: doko_session
jwt_issuer = "https://id.example"   # required `iss`; auth::sign adds it
jwt_audience = "my-api"             # required `aud`; auth::sign adds it
jwks_url = "https://id.example/.well-known/jwks.json"   # RS256 keys, picked by the token's `kid`
```

```rust
use doko::{RouterExt, auth::{self, Claims, Session, SetCookie}};

Router::new()
    .route("/me", get(me))
    .require_auth()                       // 401 without a session or a valid bearer JWT
    .route("/login", post(login))
    .route("/logout", post(logout))
    .route("/api/me", get(api_me))

async fn login(ctx: Ctx, session: Session, Json(c): Json<Credentials>) -> Result<(SetCookie, Json<Value>)> {
    let user = find_user(&ctx, &c.username).await?.ok_or_else(Error::unauthorized)?;
    if !auth::verify_password(&c.password, &user.password_hash).await? {
        return Err(Error::unauthorized());
    }
    let token = auth::sign(&ctx, &user, 3600).await?;                 // HS256 JWT, valid for an hour
    Ok((session.set(&user).await?, Json(json!({ "token": token }))))  // return the cookie
}

async fn logout(session: Session) -> Result<SetCookie> { session.clear().await }

async fn me(session: Session) -> Result<Json<User>> {
    session.get()?.map(Json).ok_or_else(Error::unauthorized)
}

async fn api_me(Claims(user): Claims<User>) -> Json<User> { Json(user) }   // 401 unless the token is valid
```

- **Sessions.** The cookie (`HttpOnly; Secure; SameSite=Lax`) holds a random id signed with `SESSION_SECRET`; the data lives in KV for 7 days. `session.set` and `session.clear` return a `SetCookie` that must go out with the response. `set` always issues a new id.
- **Strongly consistent sessions.** KV can serve a session for up to a minute after logout in other regions. If that matters, declare `[do.SESSIONS] class_name = "SessionStore"` instead of `[kv.SESSIONS]` and export it: `doko::app!(App, objects: [SessionStore])`.
- **JWTs.** `Claims<T>` reads `Authorization: Bearer`, checks the signature, `exp`, `nbf` and the configured `iss` / `aud`, and deserializes the claims into `T`. `auth::sign` adds `iat` and `exp`. Each algorithm has its own key, so a token can't choose HS256 to be checked against the RS256 public key.
- **JWKS.** With `jwks_url`, RS256 tokens are checked against the key their `kid` names. The key set is kept in memory for an hour; an unknown `kid` refetches it, at most once a minute, so rotated keys are picked up without a deploy. Set `jwt_issuer` too, or any tenant of a shared identity provider can mint tokens you accept.
- **Roles.** `.require_role("admin")` is `.require_auth()` plus a 403 unless the session value or the JWT claims carry the role, as `"role": "admin"` or `"roles": ["admin", ..]`. What roles exist and who has them is up to the app.
- **Signed tokens.** `auth::token(&ctx, "reset-password", &user.id, 3600).await?` makes a tamper-proof token for a link in a mail, and `auth::read_token::<i64>(&ctx, "reset-password", &token).await?` gives the data back, or `None` if the token is forged, expired or was made for another purpose. Tokens are signed with `SESSION_SECRET` and can't pass as a session cookie or a JWT. Their data is readable, so carry ids, not secrets.
- **Passwords.** `auth::hash_password` and `verify_password` use PBKDF2-SHA256 with a random salt at 100,000 iterations (the Workers maximum), in the runtime's native crypto.
- **`.require_auth()`** applies to the routes added before it. Put `.rate_limit(..)` on login and signup routes: see [examples/hello/src/controllers/auth.rs](https://github.com/wheregmis/doko/blob/master/examples/hello/src/controllers/auth.rs).

## Email

```toml
[email.EMAIL]             # optional: destination_address, allowed_destination_addresses,
                          # allowed_sender_addresses, remote (same as wrangler's send_email)
```

```rust
use doko::{InboundEmail, Message};

// Sending. Returns the message id.
ctx.mail(EMAIL)?.send(&Message {
    from: "hello@example.com".into(),
    to: vec![user.email],
    subject: "Welcome".into(),
    text: Some("Thanks for signing up.".into()),
    html: Some(format!("<p>Thanks for signing up, {}.</p>", doko::escape_html(&user.name))),
    ..Default::default()
}).await?;

// Receiving.
impl Hooks for App {
    async fn on_email(ctx: Ctx, email: InboundEmail) -> Result<()> {
        let body = email.read().await?;                  // subject, text, html: decoded
        save(&ctx, &email.from(), body.subject, body.text).await
        // also: email.to(), email.header("message-id"), email.reject("why"), email.forward("me@example.com")
    }
    // ...
}
```

- **Sending** needs a domain onboarded to Cloudflare Email Sending (`wrangler email sending enable example.com`), and `from` must be on it. It is for transactional mail, not newsletters.
- **Receiving** needs an Email Routing rule that delivers an address to this worker. Without an `on_email`, mail is rejected. An `Err` fails the delivery, so the sending server retries or bounces. doko logs the sender and recipient of every inbound mail.
- **More fields.** `cc`, `bcc`, `reply_to`, `headers: vec![("X-Entity-Ref-ID".into(), id)]` and `attachments: vec![Attachment { filename, content_type, content }]`. `from` and `reply_to` may carry a display name: `"Ada Lovelace <ada@example.com>"`. Inbound attachments are in `email.read().await?.attachments`.
- **Templates.** `doko::render_html(include_str!("welcome.html"), &json!({ "name": user.name }))?` fills `{{ name }}` placeholders (dots reach nested values) and HTML-escapes every value. `render_text` does the same without escaping, for `text`. A placeholder with no value is an error. There are no loops or conditions.
- **Escape user input** with `doko::escape_html` (or a template) before putting it in `html`. `send` refuses line breaks in addresses, the subject, headers and file names.
- **Locally,** `doko dev` logs sent mail instead of sending it (set `remote = true` on the binding to send for real). To deliver a test mail, POST a raw message, with a `Message-ID` header, to wrangler's local endpoint:

```bash
curl 'localhost:8787/cdn-cgi/local/email?from=ada@example.com&to=notes@example.com' --data-binary @mail.eml
```

`Mailer::raw()` and `InboundEmail::raw()` give the `worker` types, for inline images, raw MIME messages and replies.

## Postgres

Postgres goes through [Hyperdrive](https://developers.cloudflare.com/hyperdrive/), which pools connections close to the database. It is behind a feature, so apps that only use D1 don't carry the driver:

```toml
# Cargo.toml
doko = { package = "doko-rs", version = "0.1", features = ["pg"] }
```

```toml
# doko.toml
[hyperdrive.PG]
local_connection_string = "postgres://app:app@127.0.0.1:5432/app"   # used by `doko dev` and `doko migrate --pg`

[env.prod.hyperdrive.PG]
id = "<id from `wrangler hyperdrive create`>"
```

```rust
use crate::bindings::PG;

let pg = ctx.pg(PG).await?;
let todos: Vec<Todo> = pg.all("SELECT * FROM todos WHERE done = $1 ORDER BY id", (false,)).await?;
let todo: Option<Todo> = pg.first("INSERT INTO todos (title) VALUES ($1) RETURNING *", (&title,)).await?;
let changed: u64 = pg.run("UPDATE todos SET done = true WHERE id = $1", (id,)).await?;
pg.batch(vec![pg.statement("..", (a,))?, pg.statement("..", (b,))?]).await?;   // one transaction
```

- **Same shape as D1:** `all`, `first`, `run`, `statement` + `batch`, with params as a tuple and rows read by column name. The differences are the placeholders (`$1`, not `?1`), `ctx.pg(..)` being async because it connects, and `run` returning the number of changed rows.
- **Types.** Text, integers, floats, booleans, `json`/`jsonb`, UUIDs, dates and timestamps are read (the last three as strings). Cast anything else in SQL: `price::text`. Params bind as the type Postgres infers for the placeholder; for a type doko doesn't encode, such as a timestamp, write `$1::text::timestamptz`.
- **Local credentials only** go in `local_connection_string`: doko.toml is committed. The real database's credentials live in the Hyperdrive config.
- A binding without an `id` gets a placeholder so `doko dev` works; deploying that env fails until you set the real one.

### Postgres migrations

```bash
doko generate migration create_todos --pg   # migrations/pg/0001_create_todos.sql
doko migrate --pg                           # apply to the local database
DATABASE_URL=postgres://.. doko migrate --pg    # apply to any other database
```

Wrangler has no Postgres tooling, so this runs `psql`, which must be installed. Each file runs in one transaction and is recorded in a `_doko_migrations` table, so a rerun applies only what is new.

## Workers AI

```toml
[ai]      # binds Workers AI for ctx.ai()
```

```rust
#[derive(Deserialize)]
struct Answer { response: String }

let input = json!({ "messages": [{ "role": "user", "content": text }] });
let answer: Answer = ctx.ai()?.run("@cf/meta/llama-3.3-70b-instruct-fp8-fast", &input).await?;

// Or stream the answer as it is written: return this from the handler.
async fn stream(ctx: Ctx, Json(q): Json<Question>) -> Result<Response<Body>> {
    ctx.ai()?.stream(MODEL, &json!({ "messages": [{ "role": "user", "content": q.text }] })).await
}
```

- **Input and output are the model's own shapes**, as serde types or `serde_json::Value`. `wrangler ai models list` shows the current models and `wrangler ai models schema <model>` their shapes; models are retired over time.
- **`stream`** adds `"stream": true` and answers with server-sent events, one `data: {..}` line per piece and a final `data: [DONE]`.
- **There is no local AI.** Inference runs on Cloudflare even under `doko dev`, so it needs `wrangler login` and is billed to your account.
- `ctx.ai()?.raw()` gives the `worker` binding, for image and audio models.

## Integration tests

`doko::test::App` runs your worker under `wrangler dev` (on a free port, with empty local state) and gives you an HTTP client. Enable it for tests only:

```toml
[dev-dependencies]
doko = { ..., features = ["test"] }
```

```rust
#[test]
#[ignore = "starts wrangler dev"]
fn creates_notes() {
    let app = doko::test::App::start(&["DB"]);   // applies DB's migrations first
    let res = app.post_json("/notes", &json!({ "body": "hi" }));
    assert_eq!(res.status, 201);
    let note: Note = res.json();
}
```

`app.send(method, path, headers, body)` sends any request, and `res.header("set-cookie")` reads a response header.

Run with `cargo test -- --ignored`. This needs Node, and startup includes a release build, so share one `App` across the asserts in a test. Queues, D1, KV, Durable Objects, email and the Cache API all run locally. Postgres needs a database at the `local_connection_string`, and an app with `[ai]` needs `wrangler login` to start. See [examples/hello/tests/app.rs](https://github.com/wheregmis/doko/blob/master/examples/hello/tests/app.rs).
