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

mod ai;
pub mod auth;
mod cache;
mod durable;
mod error;
mod jobs;
mod log;
mod mail;
#[cfg(feature = "pg")]
mod pg;
mod request;
mod storage;
#[cfg(feature = "test")]
pub mod test;

pub use ai::Ai;
pub use axum;
pub use cache::{Cache, Policy, RouterExt};
#[doc(hidden)]
pub use durable::{object_alarm, object_result, serve_object};
pub use durable::{Durable, DurableObject, RateLimit, RateLimiter, Stub, accept_websocket};
pub use error::{Error, Result};
pub use jobs::{Job, Jobs, Tasks};
pub use mail::{Attachment, EmailBody, InboundEmail, Mailer, Message, escape_html, render_html, render_text};
#[cfg(feature = "pg")]
pub use pg::{Pg, PgStatement};
pub use request::{Errors, Valid, Validate};
pub use storage::{
    Bucket, D1Binding, Db, DoBinding, EmailBinding, HyperdriveBinding, Kv, KvBinding, QueueBinding, R2Binding, Statement,
};
pub use worker;

use std::{convert::Infallible, pin::pin, sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, FromRequestParts},
    http::{Response, StatusCode, request::Parts},
    middleware,
    response::IntoResponse,
    routing::get,
};
use futures_util::future::{Either, select};
use serde::de::DeserializeOwned;
use tower_service::Service;
use worker::{Context, Env, HttpRequest, ScheduleContext, ScheduledEvent, send::SendWrapper};

/// Var holding `[config]` from doko.toml, merged for the active environment.
const CONFIG_VAR: &str = "DOKO_CONFIG";
const ENV_VAR: &str = "DOKO_ENV";
/// Var holding `[tasks]` (name → cron schedule) for the active environment.
const TASKS_VAR: &str = "DOKO_TASKS";
/// Var holding `[app] request_timeout_secs`, when set.
const TIMEOUT_VAR: &str = "DOKO_REQUEST_TIMEOUT_SECS";

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

    /// A Durable Object namespace: `ctx.durable(bindings::COUNTER)?.get("name")?`.
    pub fn durable(&self, binding: DoBinding) -> Result<Durable> {
        Ok(Durable(SendWrapper::new(self.env.durable_object(binding.0).map_err(storage::missing(binding.0))?)))
    }

    /// An email sending binding: `ctx.mail(bindings::EMAIL)?.send(&message).await?`.
    pub fn mail(&self, binding: EmailBinding) -> Result<Mailer> {
        Ok(Mailer(SendWrapper::new(self.env.send_email(binding.0).map_err(storage::missing(binding.0))?)))
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

    /// Handles an email that Email Routing delivered to this worker. An `Err` fails the delivery,
    /// so the sending server retries or bounces it. Without an override, mail is rejected.
    fn on_email(_ctx: Ctx, email: InboundEmail) -> impl Future<Output = Result<()>> {
        email.reject("this address does not accept mail");
        async { Ok(()) }
    }

    /// Called after a request was answered with a 5xx, with the error whose message the client
    /// didn't see: send it to an error tracker or a queue. Runs after the response is sent.
    fn on_error(_ctx: Ctx, _error: Error) -> impl Future<Output = ()> + 'static {
        async {}
    }
}

#[doc(hidden)]
pub fn build_router<H: Hooks>() -> Router<Ctx> {
    // Added outside the app's routes, so its `.require_auth()` and `.rate_limit()` don't cover it.
    H::routes()
        .route("/_health", get(request::health))
        .layer(middleware::from_fn(request::guard::<H>))
        .layer(middleware::from_fn(log::requests))
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
    let timeout = ctx.var(TIMEOUT_VAR).and_then(|secs| secs.parse::<u64>().ok());
    // Caps the bodies extractors buffer (`Valid`, `Json`, `Bytes`); axum's default is 2 MiB.
    // A handler that streams `Body` (uploads) is not limited.
    let router = match request::Config::load(&ctx).body_limit_bytes {
        Some(max) => router.layer(DefaultBodyLimit::max(max)),
        None => router,
    };
    let response = router.with_state(ctx).call(req);
    let Some(secs) = timeout else {
        return Ok(response.await?);
    };
    // The platform limits CPU time, not waiting: without this, a hung upstream holds the request open.
    // Losing the race drops the handler's future, which cancels whatever it was awaiting.
    match select(pin!(response), pin!(worker::Delay::from(Duration::from_secs(secs)))).await {
        Either::Left((response, _)) => Ok(response?),
        Either::Right(_) => {
            Ok(Error::new(StatusCode::GATEWAY_TIMEOUT, format!("no response within {secs}s")).into_response())
        }
    }
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
pub async fn dispatch_email<H: Hooks>(
    message: worker::ForwardableEmailMessage,
    env: Env,
    exec: Context,
) -> worker::Result<()> {
    let ctx = Ctx { env: SendWrapper::new(env), exec: Exec::Fetch(Arc::new(exec)) };
    let (from, to) = (message.from(), message.to());
    let result = H::on_email(ctx, InboundEmail(SendWrapper::new(message))).await;
    let error = result.as_ref().err().map(|e| e.message().to_owned());
    worker::console_log!(
        "{}",
        serde_json::json!({ "level": if error.is_some() { "error" } else { "info" }, "email": { "from": from, "to": to }, "error": error })
    );
    result.map_err(|e| worker::Error::RustError(e.to_string()))
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

/// Wires a [`Hooks`] type to the Workers fetch, scheduled, queue and email events.
///
/// `doko::app!(App, objects: [Counter, RateLimiter])` also exports those [`DurableObject`] types
/// as classes. Each must be in scope by that name, which is its `class_name` in doko.toml.
#[macro_export]
macro_rules! app {
    ($app:ty, objects: [$($object:ident),* $(,)?]) => {
        $crate::app!($app);
        $(
            const _: () = {
                // wasm-bindgen 0.2.130's struct derive ignores worker's `wasm_bindgen = ::worker::wasm_bindgen`
                // and emits bare `wasm_bindgen::` paths, so the name must be in scope here.
                #[allow(unused_imports)]
                use ::worker::wasm_bindgen;

                // The exported class. It shadows the app's type in this block; `self::` still names the app's.
                // No argument: exports every handler, so alarms and WebSockets both reach the object.
                #[::worker::durable_object]
                pub struct $object(self::$object);

                impl ::worker::DurableObject for $object {
                    fn new(state: ::worker::durable::State, env: ::worker::Env) -> Self {
                        Self(<self::$object as $crate::DurableObject>::new(state, env))
                    }

                    async fn fetch(&self, req: ::worker::Request) -> ::worker::Result<::worker::Response> {
                        $crate::serve_object(&self.0, req).await
                    }

                    async fn alarm(&self) -> ::worker::Result<::worker::Response> {
                        $crate::object_alarm(&self.0).await
                    }

                    async fn websocket_message(
                        &self,
                        ws: ::worker::WebSocket,
                        message: ::worker::WebSocketIncomingMessage,
                    ) -> ::worker::Result<()> {
                        $crate::object_result(<self::$object as $crate::DurableObject>::websocket_message(&self.0, ws, message).await)
                    }

                    async fn websocket_close(
                        &self,
                        ws: ::worker::WebSocket,
                        code: usize,
                        reason: ::std::string::String,
                        was_clean: bool,
                    ) -> ::worker::Result<()> {
                        $crate::object_result(
                            <self::$object as $crate::DurableObject>::websocket_close(&self.0, ws, code, reason, was_clean).await,
                        )
                    }

                    async fn websocket_error(&self, ws: ::worker::WebSocket, error: ::worker::Error) -> ::worker::Result<()> {
                        $crate::object_result(<self::$object as $crate::DurableObject>::websocket_error(&self.0, ws, error).await)
                    }
                }
            };
        )*
    };
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

        #[::worker::event(email)]
        async fn __doko_email(
            message: ::worker::ForwardableEmailMessage,
            env: ::worker::Env,
            ctx: ::worker::Context,
        ) -> ::worker::Result<()> {
            $crate::dispatch_email::<$app>(message, env, ctx).await
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
