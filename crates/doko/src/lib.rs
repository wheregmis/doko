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

mod error;
mod log;

pub use axum;
pub use error::{Error, Result};
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

    /// Runs on every cron trigger listed in `doko.toml` (`app.crons`).
    fn on_scheduled(_ctx: Ctx, _event: ScheduledEvent) -> impl Future<Output = ()> {
        async {}
    }
}

#[doc(hidden)]
pub async fn fetch<H: Hooks>(req: HttpRequest, env: Env, exec: Context) -> worker::Result<Response<Body>> {
    let ctx = Ctx { env: SendWrapper::new(env), exec: Exec::Fetch(Arc::new(exec)) };
    Ok(H::routes().layer(middleware::from_fn(log::requests)).with_state(ctx).call(req).await?)
}

#[doc(hidden)]
pub async fn scheduled<H: Hooks>(event: ScheduledEvent, env: Env, exec: ScheduleContext) {
    let ctx = Ctx { env: SendWrapper::new(env), exec: Exec::Scheduled(SendWrapper::new(exec)) };
    H::on_scheduled(ctx, event).await
}

/// Wires a [`Hooks`] type to the Workers fetch and scheduled events.
#[macro_export]
macro_rules! app {
    ($app:ty) => {
        #[::worker::event(fetch)]
        async fn __doko_fetch(
            req: ::worker::HttpRequest,
            env: ::worker::Env,
            ctx: ::worker::Context,
        ) -> ::worker::Result<$crate::axum::http::Response<$crate::axum::body::Body>> {
            $crate::fetch::<$app>(req, env, ctx).await
        }

        #[::worker::event(scheduled)]
        async fn __doko_scheduled(
            event: ::worker::ScheduledEvent,
            env: ::worker::Env,
            ctx: ::worker::ScheduleContext,
        ) {
            $crate::scheduled::<$app>(event, env, ctx).await
        }
    };
}
