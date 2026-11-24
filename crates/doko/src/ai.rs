//! Workers AI, through the `[ai]` binding: `ctx.ai()?.run(model, &input).await?`.
//!
//! Inference always runs on Cloudflare, also under `doko dev`: it needs `wrangler login`, and it
//! is billed to the account like production use.

use axum::{
    body::Body,
    http::{Response, header::CONTENT_TYPE},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use worker::send::SendFuture;

use crate::{Ctx, Error, Result, storage};

/// The name `[ai]` in doko.toml gives the binding.
const AI: &str = "AI";

/// The `[ai]` binding, from [`Ctx::ai`].
pub struct Ai(worker::Ai);

impl Ctx {
    /// Workers AI: `ctx.ai()?`. Needs an `[ai]` table in doko.toml.
    pub fn ai(&self) -> Result<Ai> {
        Ok(Ai(self.env().ai(AI).map_err(storage::missing(AI))?))
    }
}

impl Ai {
    /// Runs `model` on `input` and deserializes its answer. Both shapes are the model's own, from
    /// the Workers AI catalog.
    ///
    /// ```ignore
    /// #[derive(Deserialize)]
    /// struct Answer { response: String }
    ///
    /// let input = json!({ "messages": [{ "role": "user", "content": "Say hi" }] });
    /// let answer: Answer = ctx.ai()?.run("@cf/meta/llama-3.3-70b-instruct-fp8-fast", &input).await?;
    /// ```
    pub async fn run<I: Serialize, O: DeserializeOwned>(&self, model: &str, input: &I) -> Result<O> {
        Ok(SendFuture::new(self.0.run(model, input)).await?)
    }

    /// Runs a text model and streams its answer as it is written, as server-sent events (one
    /// `data: {..}` line per piece, in the model's own shape, then `data: [DONE]`). Return it from the handler.
    /// `input` must serialize to a JSON object; `"stream": true` is added to it.
    pub async fn stream<I: Serialize>(&self, model: &str, input: &I) -> Result<Response<Body>> {
        let Value::Object(mut input) = serde_json::to_value(input)? else {
            return Err(Error::internal("AI input must serialize to a JSON object"));
        };
        input.insert("stream".into(), true.into());
        let events = SendFuture::new(self.0.run_bytes(model, &input)).await?;
        let body = Body::new(worker::Body::from_stream(events)?);
        Ok(Response::builder().header(CONTENT_TYPE, "text/event-stream").body(body)?)
    }

    /// The underlying binding, for image and audio models (`run_bytes`).
    pub fn raw(&self) -> &worker::Ai {
        &self.0
    }
}
