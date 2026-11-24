//! Background jobs on Queues, and named cron tasks.

use std::{collections::HashMap, pin::Pin};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use worker::{MessageBuilder, MessageExt, ScheduledEvent, console_error, console_log, send::SendFuture};

use crate::{Ctx, Error, QueueBinding, Result};

type BoxFuture = Pin<Box<dyn Future<Output = Result<()>>>>;
#[doc(hidden)]
pub type TaskFuture = BoxFuture;

const MAX_DELAY_SECS: u32 = 12 * 60 * 60;

/// A background job. Enqueue with [`Ctx::enqueue`], register in [`Hooks::jobs`](crate::Hooks::jobs).
///
/// ```ignore
/// #[derive(Serialize, Deserialize)]
/// struct SendWelcome { email: String }
///
/// impl Job for SendWelcome {
///     const NAME: &'static str = "send_welcome";
///     async fn perform(self, ctx: Ctx) -> Result<()> { /* ... */ Ok(()) }
/// }
/// ```
///
/// A job that returns `Err` is retried per the queue's `max_retries` / `retry_delay`, then sent to
/// its `dead_letter_queue` if one is set, so make `perform` safe to run more than once.
/// The jobs of one batch run concurrently, so they must not depend on each other's order.
pub trait Job: Serialize + DeserializeOwned + 'static {
    /// Stored in every message. Renaming it strands jobs already queued under the old name.
    const NAME: &'static str;

    fn perform(self, ctx: Ctx) -> impl Future<Output = Result<()>>;
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    job: String,
    args: Value,
}

/// The jobs this worker can run, from [`Hooks::jobs`](crate::Hooks::jobs).
#[derive(Default)]
pub struct Jobs(HashMap<&'static str, fn(Value, Ctx) -> BoxFuture>);

impl Jobs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `J`. Panics if `J::NAME` is empty or another job already uses `J::NAME`.
    pub fn add<J: Job>(mut self) -> Self {
        assert!(!J::NAME.is_empty(), "job name must not be empty");
        let run: fn(Value, Ctx) -> BoxFuture =
            |args, ctx| Box::pin(async move { J::perform(serde_json::from_value(args)?, ctx).await });
        assert!(self.0.insert(J::NAME, run).is_none(), "job `{}` is registered twice", J::NAME);
        self
    }
}

/// The cron tasks this worker can run, from [`Hooks::tasks`](crate::Hooks::tasks).
/// Schedules live in doko.toml's `[tasks]`, so they can differ per env without code changes.
#[derive(Default)]
pub struct Tasks(HashMap<&'static str, Box<dyn Fn(Ctx) -> BoxFuture>>);

impl Tasks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `run` under `name`, the key used in doko.toml's `[tasks]`. Panics on an empty or duplicate name.
    pub fn add<F, Fut>(mut self, name: &'static str, run: F) -> Self
    where
        F: Fn(Ctx) -> Fut + 'static,
        Fut: Future<Output = Result<()>> + 'static,
    {
        assert!(!name.is_empty(), "task name must not be empty");
        let run: Box<dyn Fn(Ctx) -> BoxFuture> = Box::new(move |ctx| Box::pin(run(ctx)));
        assert!(self.0.insert(name, run).is_none(), "task `{name}` is registered twice");
        self
    }

    #[doc(hidden)]
    pub fn spawn(&self, name: &str, ctx: Ctx) -> Option<TaskFuture> {
        self.0.get(name).map(|run| run(ctx))
    }
}

impl Ctx {
    /// Queues `job` on `queue` to run in the background.
    pub async fn enqueue<J: Job>(&self, queue: QueueBinding, job: &J) -> Result<()> {
        self.send_job(queue, job, None).await
    }

    /// Like [`enqueue`](Self::enqueue), but the job runs no sooner than `delay_secs` from now (max 12h).
    pub async fn enqueue_in<J: Job>(&self, queue: QueueBinding, job: &J, delay_secs: u32) -> Result<()> {
        if delay_secs > MAX_DELAY_SECS {
            return Err(Error::internal(format!(
                "queue delay_secs {delay_secs} exceeds maximum of {MAX_DELAY_SECS} seconds (12h)"
            )));
        }
        self.send_job(queue, job, Some(delay_secs)).await
    }

    async fn send_job<J: Job>(&self, queue: QueueBinding, job: &J, delay: Option<u32>) -> Result<()> {
        let q = self.env().queue(queue.0).map_err(crate::storage::missing(queue.0))?;
        let mut msg = MessageBuilder::new(Envelope { job: J::NAME.to_owned(), args: serde_json::to_value(job)? });
        if let Some(d) = delay {
            msg = msg.delay_seconds(d);
        }
        let msg = msg.build();
        SendFuture::new(async move { q.send(msg).await }).await?;
        Ok(())
    }
}

#[doc(hidden)]
pub type QueueBatch = worker::MessageBatch<Value>;

pub(crate) async fn run_batch(ctx: Ctx, jobs: &Jobs, batch: QueueBatch) -> worker::Result<()> {
    // Concurrently: a batch of slow jobs takes as long as its slowest one, not their sum.
    // Each message is acked or retried on its own, so one failure doesn't redeliver the rest.
    let messages = batch.messages()?;
    futures_util::future::join_all(messages.iter().map(|msg| run_message(&ctx, jobs, msg))).await;
    Ok(())
}

async fn run_message(ctx: &Ctx, jobs: &Jobs, msg: &worker::Message<Value>) {
    let attempt = msg.attempts();
    let (job, result) = match serde_json::from_value::<Envelope>(msg.body().clone()) {
        Ok(Envelope { job, args }) => {
            let result = match jobs.0.get(job.as_str()) {
                Some(run) => run(args, ctx.clone()).await,
                None => Err(Error::internal(format!("no job named `{job}` in Hooks::jobs"))),
            };
            (job, result)
        }
        Err(e) => ("?".into(), Err(Error::internal(format!("not a doko job message: {e}")))),
    };
    match result {
        Ok(()) => {
            console_log!("{}", json!({ "level": "info", "job": job, "attempt": attempt, "status": "done" }));
            msg.ack();
        }
        Err(e) => {
            console_error!("{}", json!({ "level": "error", "job": job, "attempt": attempt, "error": e.message() }));
            msg.retry();
        }
    }
}

pub(crate) async fn run_tasks(ctx: Ctx, lookup: impl Fn(&str, Ctx) -> Option<TaskFuture>, event: ScheduledEvent) {
    let cron = event.cron();
    let schedule: HashMap<String, String> = match ctx.env().object_var(crate::TASKS_VAR) {
        Ok(s) => s,
        Err(e) => {
            console_error!(
                "{}",
                json!({ "level": "error", "component": "tasks", "cron": cron, "error": format!("reading [tasks]: {e}") })
            );
            return;
        }
    };
    let mut due: Vec<&String> = schedule.iter().filter(|(_, s)| **s == cron).map(|(n, _)| n).collect();
    due.sort();
    if due.is_empty() {
        console_log!(
            "{}",
            json!({ "level": "warn", "component": "tasks", "cron": cron, "warning": "no matching tasks for cron trigger" })
        );
        return;
    }
    for name in due {
        let result = match lookup(name.as_str(), ctx.clone()) {
            Some(fut) => fut.await,
            None => Err(Error::internal("not registered in Hooks::tasks")),
        };
        match result {
            Ok(()) => console_log!("{}", json!({ "level": "info", "task": name, "cron": cron })),
            Err(e) => console_error!("{}", json!({ "level": "error", "task": name, "cron": cron, "error": e.message() })),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Serialize, Deserialize)]
    struct DummyJob;

    impl Job for DummyJob {
        const NAME: &'static str = "dummy";
        async fn perform(self, _ctx: Ctx) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Serialize, Deserialize)]
    struct EmptyJob;

    impl Job for EmptyJob {
        const NAME: &'static str = "";
        async fn perform(self, _ctx: Ctx) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn rejects_duplicate_jobs() {
        Jobs::new().add::<DummyJob>().add::<DummyJob>();
    }

    #[test]
    #[should_panic(expected = "must not be empty")]
    fn rejects_empty_job_name() {
        Jobs::new().add::<EmptyJob>();
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn rejects_duplicate_tasks() {
        Tasks::new().add("cleanup", |_| async { Ok(()) }).add("cleanup", |_| async { Ok(()) });
    }

    #[test]
    #[should_panic(expected = "must not be empty")]
    fn rejects_empty_task_name() {
        Tasks::new().add("", |_| async { Ok(()) });
    }
}
