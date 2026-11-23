mod bindings;
mod controllers;
mod jobs;
mod tasks;

use doko::{
    Ctx, Hooks, Jobs, Result, Tasks,
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
            .merge(controllers::routes())
    }

    fn jobs() -> Jobs {
        Jobs::new().add::<jobs::CountWords>()
    }

    fn tasks() -> Tasks {
        Tasks::new().add("prune", tasks::prune)
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
