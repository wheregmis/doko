---
title: "Cache"
description: "Response caching with stale-while-revalidate and global tag purges."
---

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
