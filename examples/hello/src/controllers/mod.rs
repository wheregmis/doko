//! Controllers. `doko generate controller <name>` adds to this file; keep the doko: markers.

use doko::{Ctx, axum::Router};

pub mod auth;
pub mod contact;
pub mod files;
pub mod notes;
pub mod posts;
pub mod rooms;
pub mod summaries;
pub mod todos;
// doko:modules

pub fn routes() -> Router<Ctx> {
    Router::new()
        .merge(posts::routes())
        .merge(notes::routes())
        .merge(files::routes())
        .merge(auth::routes())
        .merge(contact::routes())
        .merge(rooms::routes())
        .merge(todos::routes())
        .merge(summaries::routes())
    // doko:routes
}
