//! Notes in D1, with the list cached in KV.

use doko::{
    Ctx, Error, Result,
    axum::{
        Json, Router,
        extract::Path,
        http::StatusCode,
        routing::get,
    },
};
use serde::{Deserialize, Serialize};

use crate::bindings::{CACHE, DB};

const LIST_KEY: &str = "notes:all";

#[derive(Serialize, Deserialize)]
struct Note {
    id: i64,
    body: String,
    created_at: String,
}

#[derive(Deserialize)]
struct NewNote {
    body: String,
}

pub fn routes() -> Router<Ctx> {
    Router::new()
        .route("/notes", get(index).post(create))
        .route("/notes/{id}", get(show).delete(destroy))
}

async fn index(ctx: Ctx) -> Result<Json<Vec<Note>>> {
    let cache = ctx.kv(CACHE)?;
    if let Some(notes) = cache.get(LIST_KEY).await? {
        return Ok(Json(notes));
    }
    let notes: Vec<Note> = ctx.db(DB)?.all("SELECT * FROM notes ORDER BY id", ()).await?;
    cache.put_ttl(LIST_KEY, &notes, 60).await?;
    Ok(Json(notes))
}

async fn show(ctx: Ctx, Path(id): Path<i64>) -> Result<Json<Note>> {
    let note = ctx.db(DB)?.first("SELECT * FROM notes WHERE id = ?1", (id,)).await?;
    note.map(Json).ok_or_else(Error::not_found)
}

async fn create(ctx: Ctx, Json(new): Json<NewNote>) -> Result<(StatusCode, Json<Note>)> {
    if new.body.trim().is_empty() {
        return Err(Error::bad_request("body must not be empty"));
    }
    let note = ctx
        .db(DB)?
        .first("INSERT INTO notes (body) VALUES (?1) RETURNING *", (&new.body,))
        .await?
        .ok_or_else(|| Error::internal("insert returned no row"))?;
    ctx.kv(CACHE)?.delete(LIST_KEY).await?;
    Ok((StatusCode::CREATED, Json(note)))
}

async fn destroy(ctx: Ctx, Path(id): Path<i64>) -> Result<StatusCode> {
    let meta = ctx.db(DB)?.run("DELETE FROM notes WHERE id = ?1", (id,)).await?;
    if meta.changes == Some(0) {
        return Err(Error::not_found());
    }
    ctx.kv(CACHE)?.delete(LIST_KEY).await?;
    Ok(StatusCode::NO_CONTENT)
}
