mod bindings;
mod controllers;

use doko::{
    Ctx, Hooks, Result,
    axum::{Json, Router, routing::get},
    worker::{ScheduledEvent, console_log},
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
            .merge(controllers::routes())
    }

    async fn on_scheduled(_ctx: Ctx, event: ScheduledEvent) {
        console_log!("cron fired: {}", event.cron());
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

doko::app!(App);
