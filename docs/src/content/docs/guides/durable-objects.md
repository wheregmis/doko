---
title: "Durable Objects and rate limits"
description: "A DurableObject trait, typed stubs, WebSockets and a per-route rate limiter."
---

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

## WebSockets

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

## Rate limits

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
