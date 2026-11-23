//! Files in R2: `PUT /files/{key}` uploads the body, `GET` streams it back.

use doko::{
    Ctx, Result,
    axum::{
        Router,
        body::{Body, Bytes},
        extract::Path,
        http::{HeaderMap, Response, StatusCode, header::CONTENT_TYPE},
        routing::get,
    },
};

use crate::bindings::FILES;

pub fn routes() -> Router<Ctx> {
    Router::new().route("/files/{key}", get(show).put(upload).delete(destroy))
}

async fn show(ctx: Ctx, Path(key): Path<String>) -> Result<Response<Body>> {
    ctx.bucket(FILES)?.serve(&key).await
}

async fn upload(ctx: Ctx, Path(key): Path<String>, headers: HeaderMap, body: Bytes) -> Result<StatusCode> {
    let content_type = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("application/octet-stream");
    ctx.bucket(FILES)?.put(&key, body.to_vec(), content_type).await?;
    Ok(StatusCode::CREATED)
}

async fn destroy(ctx: Ctx, Path(key): Path<String>) -> Result<StatusCode> {
    ctx.bucket(FILES)?.delete(&key).await?;
    Ok(StatusCode::NO_CONTENT)
}
