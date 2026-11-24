mod bindings;
mod controllers;
mod jobs;
mod mailers;
mod tasks;

use doko::{
    Ctx, Hooks, InboundEmail, Jobs, RateLimit, RateLimiter, Result, RouterExt, Tasks,
    axum::{Json, Router, routing::get},
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
            .route("/token-length", get(token_length))
            // 3 requests a minute per client IP, counted in a Durable Object; then 429.
            .merge(
                Router::new()
                    .route("/limited", get(|| async { "ok" }))
                    .rate_limit(RateLimit::new(bindings::RATE_LIMITER, "limited", 3, 60)),
            )
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
}

async fn index(ctx: Ctx) -> Result<String> {
    Ok(ctx.config::<Config>()?.greeting)
}

async fn health(ctx: Ctx) -> Json<Value> {
    Json(json!({ "status": "ok", "env": ctx.env_name() }))
}

/// 500s until you set it: `wrangler secret put API_TOKEN`, or `API_TOKEN=...` in `.dev.vars` locally.
async fn token_length(ctx: Ctx) -> Result<String> {
    Ok(ctx.secret("API_TOKEN")?.len().to_string())
}

doko::app!(App, objects: [RateLimiter]);
