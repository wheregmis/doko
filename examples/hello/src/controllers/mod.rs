//! Controllers. `doko generate controller <name>` adds to this file; keep the doko: markers.

use doko::{Ctx, axum::Router};

pub mod posts;
// doko:modules

pub fn routes() -> Router<Ctx> {
    Router::new()
    .merge(posts::routes())
    // doko:routes
}
