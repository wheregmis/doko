//! Todos in Postgres, through Hyperdrive. The handlers read like the D1 ones in notes.rs;
//! only the placeholders differ (`$1` instead of `?1`).

use doko::{
    Ctx, Error, Result,
    axum::{
        Json, Router,
        extract::Path,
        http::StatusCode,
        routing::{get, post},
    },
};
use serde::{Deserialize, Serialize};

use crate::bindings::PG;

#[derive(Serialize, Deserialize)]
struct Todo {
    id: i64,
    title: String,
    done: bool,
    /// A jsonb column.
    tags: Vec<String>,
    /// A timestamptz column, read as an RFC 3339 string.
    created_at: String,
}

#[derive(Deserialize)]
struct NewTodo {
    title: String,
    #[serde(default)]
    tags: Vec<String>,
}

pub fn routes() -> Router<Ctx> {
    Router::new().route("/todos", get(index).post(create)).route("/todos/{id}/done", post(finish))
}

async fn index(ctx: Ctx) -> Result<Json<Vec<Todo>>> {
    Ok(Json(ctx.pg(PG).await?.all("SELECT * FROM todos ORDER BY id", ()).await?))
}

async fn create(ctx: Ctx, Json(new): Json<NewTodo>) -> Result<(StatusCode, Json<Todo>)> {
    if new.title.trim().is_empty() {
        return Err(Error::bad_request("title must not be empty"));
    }
    let todo = ctx
        .pg(PG)
        .await?
        .first("INSERT INTO todos (title, tags) VALUES ($1, $2) RETURNING *", (&new.title, &new.tags))
        .await?
        .ok_or_else(|| Error::internal("insert returned no row"))?;
    Ok((StatusCode::CREATED, Json(todo)))
}

async fn finish(ctx: Ctx, Path(id): Path<i64>) -> Result<StatusCode> {
    match ctx.pg(PG).await?.run("UPDATE todos SET done = true WHERE id = $1", (id,)).await? {
        0 => Err(Error::not_found()),
        _ => Ok(StatusCode::NO_CONTENT),
    }
}
