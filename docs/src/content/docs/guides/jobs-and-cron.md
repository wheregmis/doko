---
title: "Jobs and cron tasks"
description: "Queues-backed jobs with retries and a dead-letter queue, and named cron tasks."
---

```toml
[queues.JOBS]                       # this worker sends to and consumes this queue
                                    # (consume = false: another worker consumes it)
queue = "my-app-jobs"
max_retries = 3                     # wrangler consumer settings: max_batch_size,
dead_letter_queue = "my-app-dlq"    # max_batch_timeout, retry_delay, max_concurrency

[tasks]                             # name = cron schedule
prune = "0 3 * * *"

[env.prod.tasks]
prune = "0 4 * * *"
```

```rust
#[derive(Serialize, Deserialize)]
struct CountWords { id: i64 }

impl Job for CountWords {
    const NAME: &'static str = "count_words";   // stored in each message: don't rename casually
    async fn perform(self, ctx: Ctx) -> Result<()> { /* ... */ Ok(()) }
}

impl Hooks for App {
    fn jobs() -> Jobs { Jobs::new().add::<CountWords>() }
    fn tasks() -> Tasks { Tasks::new().add("prune", prune) }   // async fn prune(ctx: Ctx) -> Result<()>
    // ...
}

ctx.enqueue(JOBS, &CountWords { id }).await?;          // or enqueue_in(JOBS, &job, delay_secs)
```

- **Failed jobs.** A job that returns `Err` is retried, and after `max_retries` it moves to the `dead_letter_queue`. Make `perform` safe to run more than once.
- **Batches.** The jobs of one batch run concurrently, and each is acked or retried on its own, so jobs must not depend on each other's order. `max_batch_size` caps how many run at once.
- **Job logs.** Every attempt is logged as JSON, with the job name and attempt number.
- **Cron triggers.** doko writes each env's cron triggers from its `[tasks]` schedules. On every trigger, it runs each task whose schedule matches.
- **Testing a task locally:** `curl "localhost:8787/cdn-cgi/handler/scheduled?cron=0+3+*+*+*"`.
