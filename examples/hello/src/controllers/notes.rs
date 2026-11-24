//! Notes in D1. GET responses are edge-cached and purged by tag on writes.

use doko::{
    Ctx, Error, Policy, Result, RouterExt,
    axum::{
        Json, Router,
        extract::Path,
        http::StatusCode,
        routing::get,
    },
};
use serde::{Deserialize, Serialize};

use crate::bindings::{CACHE, DB, JOBS};
use crate::jobs::CountWords;

pub const TAG: &str = "notes";

#[derive(Serialize, Deserialize)]
struct Note {
    id: i64,
    body: String,
    created_at: String,
    /// Set in the background by the CountWords job.
    words: Option<i64>,
}

#[derive(Deserialize)]
struct NewNote {
    body: String,
}

pub fn routes() -> Router<Ctx> {
    Router::new()
        .route("/notes", get(index).post(create))
        .route("/notes/{id}", get(show).delete(destroy))
        // Only GETs are cached; POST/DELETE pass through and purge the tag.
        .cache(Policy::ttl(60).stale_while_revalidate(300).tags(CACHE, &[TAG]))
        // Added after .cache(), so it uses value caching instead.
        .route("/notes/count", get(count))
}

async fn index(ctx: Ctx) -> Result<Json<Vec<Note>>> {
    Ok(Json(ctx.db(DB)?.all("SELECT * FROM notes ORDER BY id", ()).await?))
}

/// Value caching: the count is computed at most once per 30s. The KV tier shares it between
/// data centers, and makes it work on `*.workers.dev`, where the Cache API does nothing.
async fn count(ctx: Ctx) -> Result<Json<i64>> {
    let db = ctx.db(DB)?;
    let n = ctx
        .cache()
        .tier(CACHE)
        .get_or_set("notes:count", 30, async {
            let row: Option<serde_json::Value> = db.first("SELECT COUNT(*) AS n FROM notes", ()).await?;
            Ok(row.and_then(|r| r["n"].as_i64()).unwrap_or(0))
        })
        .await?;
    Ok(Json(n))
}

async fn show(ctx: Ctx, Path(id): Path<i64>) -> Result<Json<Note>> {
    let note = ctx.db(DB)?.first("SELECT * FROM notes WHERE id = ?1", (id,)).await?;
    note.map(Json).ok_or_else(Error::not_found)
}

async fn create(ctx: Ctx, Json(new): Json<NewNote>) -> Result<(StatusCode, Json<Note>)> {
    if new.body.trim().is_empty() {
        return Err(Error::bad_request("body must not be empty"));
    }
    let note: Note = ctx
        .db(DB)?
        .first("INSERT INTO notes (body) VALUES (?1) RETURNING *", (&new.body,))
        .await?
        .ok_or_else(|| Error::internal("insert returned no row"))?;
    ctx.enqueue(JOBS, &CountWords { id: note.id }).await?;
    ctx.cache().purge_tag(CACHE, TAG).await?;
    Ok((StatusCode::CREATED, Json(note)))
}

async fn destroy(ctx: Ctx, Path(id): Path<i64>) -> Result<StatusCode> {
    let meta = ctx.db(DB)?.run("DELETE FROM notes WHERE id = ?1", (id,)).await?;
    if meta.changes == Some(0) {
        return Err(Error::not_found());
    }
    ctx.cache().purge_tag(CACHE, TAG).await?;
    Ok(StatusCode::NO_CONTENT)
}
