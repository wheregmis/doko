use doko::{
    Ctx, Error, Result,
    axum::{Json, Router, extract::Path, routing::get},
};

pub fn routes() -> Router<Ctx> {
    Router::new().route("/posts", get(index)).route("/posts/{id}", get(show))
}

async fn index(_ctx: Ctx) -> Result<Json<Vec<String>>> {
    Ok(Json(vec![]))
}

async fn show(_ctx: Ctx, Path(_id): Path<String>) -> Result<Json<String>> {
    Err(Error::not_found())
}
