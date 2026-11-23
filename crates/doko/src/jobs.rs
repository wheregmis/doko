//! Background jobs on Queues, and named cron tasks.

use std::{collections::HashMap, pin::Pin};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use worker::{MessageBuilder, MessageExt, ScheduledEvent, console_error, console_log, send::SendFuture};

use crate::{Ctx, Error, QueueBinding, Result};

type BoxFuture = Pin<Box<dyn Future<Output = Result<()>>>>;

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

    /// Registers `J`. Panics if another job already uses `J::NAME`.
    pub fn add<J: Job>(mut self) -> Self {
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

    /// Registers `run` under `name`, the key used in doko.toml's `[tasks]`. Panics on a duplicate name.
    pub fn add<F, Fut>(mut self, name: &'static str, run: F) -> Self
    where
        F: Fn(Ctx) -> Fut + 'static,
        Fut: Future<Output = Result<()>> + 'static,
    {
        let run: Box<dyn Fn(Ctx) -> BoxFuture> = Box::new(move |ctx| Box::pin(run(ctx)));
        assert!(self.0.insert(name, run).is_none(), "task `{name}` is registered twice");
        self
    }
}

impl Ctx {
    /// Queues `job` on `queue` to run in the background.
    pub async fn enqueue<J: Job>(&self, queue: QueueBinding, job: &J) -> Result<()> {
        self.send_job(queue, job, None).await
    }

    /// Like [`enqueue`](Self::enqueue), but the job runs no sooner than `delay_secs` from now (max 12h).
    pub async fn enqueue_in<J: Job>(&self, queue: QueueBinding, job: &J, delay_secs: u32) -> Result<()> {
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

pub(crate) async fn run_batch(ctx: Ctx, jobs: Jobs, batch: QueueBatch) -> worker::Result<()> {
    // shortcut: messages run one at a time; run them concurrently if batches get slow.
    for msg in batch.messages()? {
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
    Ok(())
}

pub(crate) async fn run_tasks(ctx: Ctx, tasks: Tasks, event: ScheduledEvent) {
    let cron = event.cron();
    let schedule: HashMap<String, String> = match ctx.env().object_var(crate::TASKS_VAR) {
        Ok(s) => s,
        Err(e) => return console_error!("reading [tasks]: {e}"),
    };
    let mut due: Vec<&String> = schedule.iter().filter(|(_, s)| **s == cron).map(|(n, _)| n).collect();
    due.sort();
    for name in due {
        let result = match tasks.0.get(name.as_str()) {
            Some(run) => run(ctx.clone()).await,
            None => Err(Error::internal("not registered in Hooks::tasks")),
        };
        match result {
            Ok(()) => console_log!("{}", json!({ "level": "info", "task": name, "cron": cron })),
            Err(e) => console_error!("{}", json!({ "level": "error", "task": name, "cron": cron, "error": e.message() })),
        }
    }
}
