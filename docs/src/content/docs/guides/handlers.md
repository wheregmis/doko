---
title: "Handlers"
description: "axum handlers with Ctx, JSON errors, pagination, validation, middleware and error hooks."
---

- Take `ctx: Ctx` directly as an argument.
- Return `doko::Result<T>`. `?` on any error gives a JSON 500 whose message is logged but not sent to the client.
- Use `Error::not_found()`, `bad_request(..)`, `unauthorized()`, `forbidden()` or `Error::new(status, ..)` for `{"error": ".."}` responses.
- `ctx.wait_until(fut)` runs work after the response is sent.

Every request is logged as one JSON line, and responses carry `x-request-id` (the `cf-ray` header, or `-` locally). `GET /_health` answers `{"ok": true}` without touching your routes or their auth, for uptime checks. For other middleware, `.layer(...)` any tower layer onto your router.

## Pagination

```rust
use doko::{Page, Paged};

// GET /posts?page=2&per_page=50
async fn index(ctx: Ctx, page: Page) -> Result<Json<Paged<Post>>> {
    let db = ctx.db(DB)?;
    let posts = db.all("SELECT * FROM posts ORDER BY id DESC LIMIT ?1 OFFSET ?2", (page.limit(), page.offset())).await?;
    let total = count(&db).await?;                 // SELECT COUNT(*) FROM posts
    Ok(Json(page.of(posts, total)))
}
```

The response is `{"items": [..], "page": 2, "per_page": 50, "total": 120, "total_pages": 3}`. Without the parameters a request gets page 1 of 20. `per_page` is capped at 100, so a client can't ask for a whole table.

## Validation

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

## Middleware

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

## Reporting errors

```rust
impl Hooks for App {
    async fn on_error(ctx: Ctx, error: Error) {
        // error.status(), error.message(): send them to an error tracker or a queue
    }
    // ...
}
```

`on_error` gets every request answered with a 5xx, with the message the client didn't see. It runs after the response is sent, so it can't slow down or break the error response. A request cut off by `request_timeout_secs`, and failed jobs and tasks, are logged but don't reach it.
