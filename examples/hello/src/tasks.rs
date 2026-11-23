//! Cron tasks. Schedules are in doko.toml's [tasks].

use doko::{Ctx, Result};

use crate::bindings::{CACHE, DB};
use crate::controllers::notes::TAG;

/// Deletes notes older than 30 days.
pub async fn prune(ctx: Ctx) -> Result<()> {
    let meta = ctx.db(DB)?.run("DELETE FROM notes WHERE created_at < datetime('now', '-30 days')", ()).await?;
    if meta.changes.unwrap_or(0) > 0 {
        ctx.cache().purge_tag(CACHE, TAG).await?;
    }
    Ok(())
}
