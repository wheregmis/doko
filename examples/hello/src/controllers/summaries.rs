//! Workers AI: `POST /summarize` answers with a summary, `POST /summarize/stream` streams one.
//! Inference runs on Cloudflare even under `doko dev`, and is billed to the account.

use doko::{
    Ctx, Error, Result,
    axum::{Json, Router, body::Body, http::Response, routing::post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// Models are retired over time: pick a current one with `wrangler ai models list`.
const MODEL: &str = "@cf/ibm-granite/granite-4.0-h-micro";

#[derive(Deserialize)]
struct Text {
    text: String,
}

#[derive(Serialize)]
struct Summary {
    summary: String,
}

pub fn routes() -> Router<Ctx> {
    Router::new().route("/summarize", post(summarize)).route("/summarize/stream", post(stream))
}

fn prompt(text: &str) -> Result<Value> {
    if text.trim().is_empty() {
        return Err(Error::bad_request("text must not be empty"));
    }
    Ok(json!({
        "messages": [
            { "role": "system", "content": "Summarize the user's text in one sentence." },
            { "role": "user", "content": text },
        ],
        "max_tokens": 120,
    }))
}

async fn summarize(ctx: Ctx, Json(t): Json<Text>) -> Result<Json<Summary>> {
    let answer: Value = ctx.ai()?.run(MODEL, &prompt(&t.text)?).await?;
    // Chat models answer in one of two shapes, depending on the model.
    let text = answer["response"].as_str().or_else(|| answer["choices"][0]["message"]["content"].as_str());
    let text = text.ok_or_else(|| Error::internal(format!("unexpected answer from {MODEL}: {answer}")))?;
    Ok(Json(Summary { summary: text.trim().to_owned() }))
}

/// Server-sent events, one per piece of the answer.
async fn stream(ctx: Ctx, Json(t): Json<Text>) -> Result<Response<Body>> {
    ctx.ai()?.stream(MODEL, &prompt(&t.text)?).await
}
