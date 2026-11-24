mod bindings;
mod controllers;
mod jobs;
mod mailers;
mod tasks;

use controllers::rooms::Room;
use doko::{
    Ctx, Error, Hooks, InboundEmail, Jobs, Policy, RateLimit, RateLimiter, Result, RouterExt, Tasks,
    axum::{
        Json, Router,
        http::{HeaderMap, header::ACCEPT_LANGUAGE},
        routing::get,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Config {
    greeting: String,
}

struct App;

impl Hooks for App {
    fn routes() -> Router<Ctx> {
        Router::new()
            .route("/", get(index))
            .route("/health", get(health))
            .route("/motto", get(motto))
            // A handler that fails, and what `on_error` below recorded about it.
            .route("/boom", get(|| async { Err::<(), _>(Error::internal("the basket broke")) }))
            .route("/last-error", get(last_error))
            .route("/token-length", get(token_length))
            // 3 requests a minute per API key (per client IP without one), counted in a Durable Object; then 429.
            .merge(
                Router::new().route("/limited", get(|| async { "ok" })).rate_limit(
                    RateLimit::new(bindings::RATE_LIMITER, "limited", 3, 60)
                        .key_by(|req| req.headers().get("x-api-key")?.to_str().ok().map(str::to_owned)),
                ),
            )
            // Cached per language: each `accept-language` value gets its own copy.
            .merge(Router::new().route("/language", get(language)).cache(Policy::ttl(60).vary(&["accept-language"])))
            // Slower than `request_timeout_secs` in doko.toml, so it answers 504.
            .route("/slow", get(slow))
            .merge(controllers::routes())
    }

    fn jobs() -> Jobs {
        Jobs::new().add::<jobs::CountWords>()
    }

    fn tasks() -> Tasks {
        Tasks::new().add("prune", tasks::prune)
    }

    /// Mail routed to this worker becomes a note. doko logs the sender and recipient of each one.
    async fn on_email(ctx: Ctx, email: InboundEmail) -> Result<()> {
        let body = email.read().await?;
        let note = format!("mail from {}: {}", email.from(), body.subject.unwrap_or_default());
        ctx.db(bindings::DB)?.run("INSERT INTO notes (body) VALUES (?1)", (&note,)).await?;
        ctx.cache().purge_tag(bindings::CACHE, controllers::notes::TAG).await
    }

    /// Every 5xx lands here after its response is sent. A real app would forward it to an error tracker.
    async fn on_error(ctx: Ctx, error: Error) {
        if let Ok(kv) = ctx.kv(bindings::CACHE) {
            let _ = kv.put("last_error", &error.message()).await;
        }
    }
}

async fn index(ctx: Ctx) -> Result<String> {
    Ok(ctx.config::<Config>()?.greeting)
}

async fn last_error(ctx: Ctx) -> Result<String> {
    ctx.kv(bindings::CACHE)?.get("last_error").await?.ok_or_else(Error::not_found)
}

/// A plain var from doko.toml's `[wrangler.vars]`.
async fn motto(ctx: Ctx) -> Result<String> {
    ctx.var("MOTTO").ok_or_else(doko::Error::not_found)
}

async fn language(headers: HeaderMap) -> String {
    headers.get(ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok()).unwrap_or("none").to_owned()
}

async fn slow() -> &'static str {
    // worker's timer is !Send; SendFuture lets an axum handler await it.
    worker::send::SendFuture::new(worker::Delay::from(std::time::Duration::from_secs(60))).await;
    "finally"
}

async fn health(ctx: Ctx) -> Json<Value> {
    Json(json!({ "status": "ok", "env": ctx.env_name() }))
}

/// 500s until you set it: `wrangler secret put API_TOKEN`, or `API_TOKEN=...` in `.dev.vars` locally.
async fn token_length(ctx: Ctx) -> Result<String> {
    Ok(ctx.secret("API_TOKEN")?.len().to_string())
}

doko::app!(App, objects: [RateLimiter, Room]);
