---
title: "Workers AI"
description: "Run and stream Workers AI models with serde types."
---

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
