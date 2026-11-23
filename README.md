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
crons = ["*/30 * * * *"]   # optional, calls Hooks::on_scheduled
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
