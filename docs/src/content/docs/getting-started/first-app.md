---
title: "Your first app"
description: "The smallest doko app and its doko.toml."
---

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
