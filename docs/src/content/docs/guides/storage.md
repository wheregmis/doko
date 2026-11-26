---
title: "Storage: KV, R2, D1"
description: "Typed KV, R2 and D1 bindings, plus D1 migrations."
---

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

D1 stores a `bool` as `1` or `0` and hands back a number. Mark boolean fields of a row struct with `#[serde(deserialize_with = "doko::bool_from_sql")]` so they read back as `bool`.

**Uploads.** `put_stream` takes the handler's `body: Body` and stores it as it arrives, holding at most 8 MiB in memory. Bodies larger than that go to R2 as a multipart upload, a part at a time; if the body fails midway, nothing is stored.

**Environments.** Each wrangler env has its own namespaces, buckets and databases, so keys need no per-env prefix: give each env its own `id` / `bucket_name` / `database_id`.

## Migrations

```bash
doko generate migration create_notes    # migrations/0001_create_notes.sql
doko migrate                            # apply locally
doko migrate --remote --env prod        # apply to the prod database
```
