//! doko: a batteries-included framework for Cloudflare Workers, built on `worker` + axum.
//!
//! ```ignore
//! use doko::{Ctx, Hooks, axum::{Router, routing::get}};
//!
//! struct App;
//! impl Hooks for App {
//!     fn routes() -> Router<Ctx> {
//!         Router::new().route("/", get(|| async { "hello" }))
//!     }
//! }
//! doko::app!(App);
//! ```
//!
//! The app crate must also depend on `worker` directly: worker's `#[event]` macro expands to `::worker::` paths.

mod cache;
mod error;
mod jobs;
mod log;
mod storage;
#[cfg(feature = "test")]
pub mod test;

pub use axum;
pub use cache::{Cache, Policy, RouterExt};
pub use error::{Error, Result};
pub use jobs::{Job, Jobs, Tasks};
pub use storage::{Bucket, D1Binding, Db, Kv, KvBinding, QueueBinding, R2Binding, Statement};
pub use worker;

use std::{convert::Infallible, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::FromRequestParts,
    http::{Response, request::Parts},
    middleware,
};
use serde::de::DeserializeOwned;
use tower_service::Service;
use worker::{Context, Env, HttpRequest, ScheduleContext, ScheduledEvent, send::SendWrapper};

/// Var holding `[config]` from doko.toml, merged for the active environment.
const CONFIG_VAR: &str = "DOKO_CONFIG";
const ENV_VAR: &str = "DOKO_ENV";
/// Var holding `[tasks]` (name → cron schedule) for the active environment.
const TASKS_VAR: &str = "DOKO_TASKS";

#[derive(Clone)]
enum Exec {
    Fetch(Arc<Context>),
    Scheduled(SendWrapper<ScheduleContext>),
}

/// Request context shared with every handler. Extract it directly: `async fn h(ctx: Ctx)`.
#[derive(Clone)]
pub struct Ctx {
    // Env wraps JS values, which are !Send; Workers run single-threaded, so this is sound.
    env: SendWrapper<Env>,
    exec: Exec,
}

impl Ctx {
    /// The raw worker environment, for bindings doko doesn't wrap yet.
    pub fn env(&self) -> &Env {
        &self.env
    }

    /// The active doko environment: `dev` by default, or the `--env` name.
    pub fn env_name(&self) -> String {
        self.var(ENV_VAR).unwrap_or_else(|| "dev".into())
    }

    /// A plain-text var, if set.
    pub fn var(&self, name: &str) -> Option<String> {
        self.env.var(name).ok().map(|v| v.to_string())
    }

    /// A secret set with `wrangler secret put` (or `.dev.vars` locally). Missing secrets are a 500.
    pub fn secret(&self, name: &str) -> Result<String> {
        self.env.secret(name).map(|s| s.to_string()).map_err(|_| Error::internal(format!("secret `{name}` is not set")))
    }

    /// The app's `[config]` from doko.toml, deserialized into your type.
    pub fn config<T: DeserializeOwned>(&self) -> Result<T> {
        self.env.object_var(CONFIG_VAR).map_err(|e| Error::internal(format!("invalid [config]: {e}")))
    }

    /// A KV namespace: `ctx.kv(bindings::CACHE)?`.
    pub fn kv(&self, binding: KvBinding) -> Result<Kv> {
        Ok(Kv(SendWrapper::new(self.env.kv(binding.0).map_err(storage::missing(binding.0))?)))
    }

    /// An R2 bucket: `ctx.bucket(bindings::FILES)?`.
    pub fn bucket(&self, binding: R2Binding) -> Result<Bucket> {
        Ok(Bucket(SendWrapper::new(self.env.bucket(binding.0).map_err(storage::missing(binding.0))?)))
    }

    /// A D1 database: `ctx.db(bindings::DB)?`.
    pub fn db(&self, binding: D1Binding) -> Result<Db> {
        Ok(Db(SendWrapper::new(self.env.d1(binding.0).map_err(storage::missing(binding.0))?)))
    }

    /// The edge cache (Cache API) for JSON values. For whole responses, see [`RouterExt::cache`].
    pub fn cache(&self) -> Cache {
        Cache::new(self.clone())
    }

    /// Runs `fut` after the response is sent, keeping the worker alive until it finishes.
    pub fn wait_until(&self, fut: impl Future<Output = ()> + 'static) {
        match &self.exec {
            Exec::Fetch(c) => c.wait_until(fut),
            Exec::Scheduled(c) => c.wait_until(fut),
        }
    }
}

impl FromRequestParts<Ctx> for Ctx {
    type Rejection = Infallible;

    async fn from_request_parts(_: &mut Parts, ctx: &Ctx) -> Result<Self, Infallible> {
        Ok(ctx.clone())
    }
}

/// The app entry point. Implement it once and register it with [`app!`].
pub trait Hooks: 'static {
    /// Your routes. doko adds request logging and an `x-request-id` header around them.
    fn routes() -> Router<Ctx>;

    /// Background jobs this worker consumes from its `[queues.*]`: `Jobs::new().add::<SendWelcome>()`.
    fn jobs() -> Jobs {
        Jobs::new()
    }

    /// Cron tasks by name; schedules come from doko.toml's `[tasks]`: `Tasks::new().add("cleanup", cleanup)`.
    fn tasks() -> Tasks {
        Tasks::new()
    }
}

#[doc(hidden)]
pub fn build_router<H: Hooks>() -> Router<Ctx> {
    H::routes().layer(middleware::from_fn(log::requests))
}

#[doc(hidden)]
pub async fn dispatch_fetch(
    router: Router<Ctx>,
    req: HttpRequest,
    env: Env,
    exec: Context,
) -> worker::Result<Response<Body>> {
    let ctx = Ctx { env: SendWrapper::new(env), exec: Exec::Fetch(Arc::new(exec)) };
    // Middleware (e.g. the response cache) reads Ctx from here; handlers get it as state.
    let mut req = req;
    req.extensions_mut().insert(ctx.clone());
    Ok(router.with_state(ctx).call(req).await?)
}

#[doc(hidden)]
pub async fn dispatch_scheduled(
    lookup: impl Fn(&str, Ctx) -> Option<jobs::TaskFuture>,
    event: ScheduledEvent,
    env: Env,
    exec: ScheduleContext,
) {
    let ctx = Ctx { env: SendWrapper::new(env), exec: Exec::Scheduled(SendWrapper::new(exec)) };
    jobs::run_tasks(ctx, lookup, event).await
}

#[doc(hidden)]
pub async fn dispatch_queue(
    jobs: &Jobs,
    batch: jobs::QueueBatch,
    env: Env,
    exec: Context,
) -> worker::Result<()> {
    let ctx = Ctx { env: SendWrapper::new(env), exec: Exec::Fetch(Arc::new(exec)) };
    jobs::run_batch(ctx, jobs, batch).await
}

#[doc(hidden)]
pub async fn fetch<H: Hooks>(req: HttpRequest, env: Env, exec: Context) -> worker::Result<Response<Body>> {
    dispatch_fetch(build_router::<H>(), req, env, exec).await
}

#[doc(hidden)]
pub async fn scheduled<H: Hooks>(event: ScheduledEvent, env: Env, exec: ScheduleContext) {
    let tasks = H::tasks();
    dispatch_scheduled(|name, ctx| tasks.spawn(name, ctx), event, env, exec).await
}

#[doc(hidden)]
pub async fn queue<H: Hooks>(batch: jobs::QueueBatch, env: Env, exec: Context) -> worker::Result<()> {
    dispatch_queue(&H::jobs(), batch, env, exec).await
}

#[doc(hidden)]
pub use jobs::QueueBatch;

/// Wires a [`Hooks`] type to the Workers fetch, scheduled and queue events.
#[macro_export]
macro_rules! app {
    ($app:ty) => {
        #[::worker::event(fetch)]
        async fn __doko_fetch(
            req: ::worker::HttpRequest,
            env: ::worker::Env,
            ctx: ::worker::Context,
        ) -> ::worker::Result<$crate::axum::http::Response<$crate::axum::body::Body>> {
            ::std::thread_local! {
                static ROUTER: ::std::cell::OnceCell<$crate::axum::Router<$crate::Ctx>> =
                    const { ::std::cell::OnceCell::new() };
            }
            let router = ROUTER.with(|c| c.get_or_init($crate::build_router::<$app>).clone());
            $crate::dispatch_fetch(router, req, env, ctx).await
        }

        #[::worker::event(scheduled)]
        async fn __doko_scheduled(
            event: ::worker::ScheduledEvent,
            env: ::worker::Env,
            ctx: ::worker::ScheduleContext,
        ) {
            ::std::thread_local! {
                static TASKS: ::std::cell::OnceCell<$crate::Tasks> =
                    const { ::std::cell::OnceCell::new() };
            }
            $crate::dispatch_scheduled(
                |name, ctx| TASKS.with(|c| c.get_or_init(<$app as $crate::Hooks>::tasks).spawn(name, ctx)),
                event,
                env,
                ctx,
            )
            .await
        }

        #[::worker::event(queue)]
        async fn __doko_queue(
            batch: $crate::QueueBatch,
            env: ::worker::Env,
            ctx: ::worker::Context,
        ) -> ::worker::Result<()> {
            static JOBS: ::std::sync::OnceLock<$crate::Jobs> = ::std::sync::OnceLock::new();
            let jobs = JOBS.get_or_init(<$app as $crate::Hooks>::jobs);
            $crate::dispatch_queue(jobs, batch, env, ctx).await
        }
    };
}
