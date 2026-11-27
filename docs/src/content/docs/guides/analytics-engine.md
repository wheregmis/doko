---
title: "Analytics Engine"
description: "Write data points to Workers Analytics Engine and read them back with SQL."
---

```toml
[analytics.USAGE]
dataset = "usage"          # created by the first write; letters, digits and _

[env.prod.analytics.USAGE]
dataset = "usage_prod"
```

```rust
use doko::{DataPoint, analytics};

// Writing. Returns at once: the runtime sends points after the response.
ctx.analytics(USAGE)?.write(&DataPoint::new(&version).blob(&version).blob(&country).double(1.0))?;

// Reading, with SQL through Cloudflare's API.
#[derive(Deserialize)]
struct Day { day: String, checks: String }

let token = ctx.secret("ANALYTICS_API_TOKEN")?;
let days: Vec<Day> = analytics::query(&account_id, &token, "SELECT toDate(timestamp) AS day, \
    SUM(_sample_interval) AS checks FROM usage GROUP BY day ORDER BY day").await?;
```

- **Columns.** The index is `index1`, blobs are `blob1`..`blob20` and doubles `double1`..`double20`, in the order they were added. A point carries one index of up to 96 bytes, which is also the sampling key: rows that share it are sampled together. `write` refuses a point over Analytics Engine's limits (20 blobs, 20 doubles, 16 KB of blobs) instead of letting it be dropped.
- **Sampling.** Analytics Engine may sample busy datasets, so count with `SUM(_sample_interval)`, not `COUNT()`.
- **Reading** goes through the SQL API, not the binding: it needs the account id and an API token with Account Analytics Read. The API has no parameters, so never put request input into the SQL. Integer columns arrive as JSON strings: read them as `String`, or cast in SQL (`toFloat64(..)`).
- **Locally,** `doko dev` accepts writes and drops them, and `query` reads the real account.
- `ctx.analytics(USAGE)?.raw()` gives the `worker` binding, for byte blobs.
