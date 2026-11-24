//! Background jobs, run from the JOBS queue.

use doko::{Ctx, Error, Job, Result};
use serde::{Deserialize, Serialize};

use crate::bindings::{CACHE, DB};
use crate::controllers::notes::TAG;

/// Counts a note's words after it is created.
#[derive(Serialize, Deserialize)]
pub struct CountWords {
    pub id: i64,
}

impl Job for CountWords {
    const NAME: &'static str = "count_words";

    async fn perform(self, ctx: Ctx) -> Result<()> {
        let db = ctx.db(DB)?;
        let body: Option<String> =
            db.first("SELECT body FROM notes WHERE id = ?1", (self.id,)).await?.map(|r: Row| r.body);
        // Err means retry, then the dead-letter queue: a note deleted before its job ran ends up there.
        let body = body.ok_or_else(Error::not_found)?;
        db.run("UPDATE notes SET words = ?1 WHERE id = ?2", (body.split_whitespace().count(), self.id)).await?;
        ctx.cache().purge_tag(CACHE, TAG).await
    }
}

#[derive(Deserialize)]
struct Row {
    body: String,
}
