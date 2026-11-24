//! Durable Objects, and the rate limiter built on them.
//!
//! Declare `[do.COUNTER] class_name = "Counter"` in doko.toml, implement [`DurableObject`] for
//! `Counter`, export it with `doko::app!(App, objects: [Counter])`, and call it from a handler:
//! `ctx.durable(bindings::COUNTER)?.get("some-name")?.fetch(req).await?`.

use std::time::Duration;

use axum::{
    body::Body,
    extract::{Query, Request},
    http::{
        Response, StatusCode,
        header::{RETRY_AFTER, UPGRADE},
    },
    middleware::Next,
    response::IntoResponse,
};
use serde::Deserialize;
use worker::{
    Date, Env, WebSocket, WebSocketIncomingMessage, WebSocketPair,
    durable::{ObjectNamespace, State},
    send::{SendFuture, SendWrapper},
};

use crate::{Ctx, DoBinding, Error, Result};

/// A Durable Object class: one single-threaded instance per name, with its own storage.
///
/// ```ignore
/// struct Counter(State);
///
/// impl DurableObject for Counter {
///     fn new(state: State, _env: Env) -> Self { Self(state) }
///
///     async fn fetch(&self, _req: Request) -> Result<Response<Body>> {
///         let n = self.0.storage().get::<u64>("n").await?.unwrap_or(0) + 1;
///         self.0.storage().put("n", n).await?;
///         Ok(n.to_string().into_response())
///     }
/// }
/// ```
///
/// These futures run inside the object and don't need to be `Send`, so `worker` types work as they are.
pub trait DurableObject: 'static {
    fn new(state: State, env: Env) -> Self;

    /// Handles a request sent with [`Stub::fetch`]. An `Err` becomes its JSON error response.
    fn fetch(&self, req: Request) -> impl Future<Output = Result<Response<Body>>>;

    /// Runs when an alarm set with `state.storage().set_alarm(..)` fires. An `Err` makes the runtime retry it.
    fn alarm(&self) -> impl Future<Output = Result<()>> {
        async { Ok(()) }
    }

    /// Runs for each message on a socket opened with [`accept_websocket`]. The object may have
    /// been asleep since the last one, so keep per-socket state in storage or in the socket's
    /// attachment (`ws.serialize_attachment(..)`), not in fields.
    fn websocket_message(&self, _ws: WebSocket, _message: WebSocketIncomingMessage) -> impl Future<Output = Result<()>> {
        async { Ok(()) }
    }

    /// Runs when a client closes its socket.
    fn websocket_close(&self, _ws: WebSocket, _code: usize, _reason: String, _was_clean: bool) -> impl Future<Output = Result<()>> {
        async { Ok(()) }
    }

    /// Runs when a socket fails.
    fn websocket_error(&self, _ws: WebSocket, _error: worker::Error) -> impl Future<Output = Result<()>> {
        async { Ok(()) }
    }
}

/// Opens a WebSocket in a [`DurableObject::fetch`]: the returned response hands the client its end,
/// and the object's `websocket_*` methods get the events. A request that isn't a WebSocket upgrade gets a 426. Sockets hibernate, so an idle object
/// costs nothing while its clients stay connected. `state.get_websockets()` lists the open ones.
///
/// ```ignore
/// async fn fetch(&self, req: Request) -> Result<Response<Body>> {
///     doko::accept_websocket(&self.0, &req)
/// }
///
/// async fn websocket_message(&self, _ws: WebSocket, message: WebSocketIncomingMessage) -> Result<()> {
///     if let WebSocketIncomingMessage::String(text) = message {
///         for peer in self.0.get_websockets() {
///             peer.send_with_str(&text)?;
///         }
///     }
///     Ok(())
/// }
/// ```
///
/// Route the client's upgrade request to the object unchanged:
/// `ctx.durable(ROOMS)?.get(&room)?.fetch(req).await`.
pub fn accept_websocket(state: &State, req: &Request) -> Result<Response<Body>> {
    if !req.headers().get(UPGRADE).is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket")) {
        return Err(Error::new(StatusCode::UPGRADE_REQUIRED, "expected a WebSocket upgrade"));
    }
    let pair = WebSocketPair::new()?;
    state.accept_web_socket(&pair.server);
    // worker turns this extension into the response's `webSocket`.
    Ok(Response::builder().status(StatusCode::SWITCHING_PROTOCOLS).extension(pair.client).body(Body::empty())?)
}

#[doc(hidden)]
pub async fn serve_object<O: DurableObject>(object: &O, req: worker::Request) -> worker::Result<worker::Response> {
    let req = worker::HttpRequest::try_from(req)?.map(Body::new);
    let res = object.fetch(req).await.unwrap_or_else(IntoResponse::into_response);
    worker::Response::try_from(res)
}

#[doc(hidden)]
pub async fn object_alarm<O: DurableObject>(object: &O) -> worker::Result<worker::Response> {
    object_result(object.alarm().await)?;
    worker::Response::empty()
}

#[doc(hidden)]
pub fn object_result(result: Result<()>) -> worker::Result<()> {
    result.map_err(|e| worker::Error::RustError(e.to_string()))
}

/// A `[do.NAME]` namespace, from [`Ctx::durable`].
pub struct Durable(pub(crate) SendWrapper<ObjectNamespace>);

impl Durable {
    /// The object called `name`. Every name is its own instance, created on first use.
    pub fn get(&self, name: &str) -> Result<Stub> {
        Ok(Stub(SendWrapper::new(self.0.get_by_name(name)?)))
    }

    /// The underlying namespace, for unique ids, jurisdictions and location hints.
    pub fn raw(&self) -> &ObjectNamespace {
        &self.0
    }
}

/// A handle to one Durable Object.
pub struct Stub(SendWrapper<worker::Stub>);

impl Stub {
    /// Sends `req` to the object's [`DurableObject::fetch`]. The URL must be absolute; only its
    /// path and query matter: `Request::get("https://do/incr")`.
    pub async fn fetch(&self, req: Request) -> Result<Response<Body>> {
        SendFuture::new(async {
            let res = self.0.fetch_with_request(worker::Request::try_from(req)?).await?;
            Ok(res.into())
        })
        .await
    }

    /// The underlying stub, for RPC and TCP sockets. Its futures are not `Send`.
    pub fn raw(&self) -> &worker::Stub {
        &self.0
    }
}

/// A fixed-window rate limit for [`RouterExt::rate_limit`](crate::RouterExt::rate_limit).
#[derive(Clone, Copy, Debug)]
pub struct RateLimit {
    binding: DoBinding,
    name: &'static str,
    limit: u64,
    window_secs: u64,
    key: Option<fn(&Request) -> Option<String>>,
}

impl RateLimit {
    /// At most `limit` requests per `window_secs` for each client IP. `binding` is a `[do.NAME]`
    /// whose class is [`RateLimiter`]. `name` keeps this count apart from other limits on the binding.
    pub fn new(binding: DoBinding, name: &'static str, limit: u64, window_secs: u64) -> Self {
        Self { binding, name, limit, window_secs, key: None }
    }

    /// Counts per `key(request)` instead of per client IP: an API key, a tenant, a user id.
    /// Requests for which it returns `None` are counted by IP.
    ///
    /// ```ignore
    /// RateLimit::new(RATE_LIMITER, "api", 100, 60)
    ///     .key_by(|req| req.headers().get("x-api-key")?.to_str().ok().map(str::to_owned))
    /// ```
    ///
    /// Only key on values the client can't choose freely, or it can dodge the limit by changing them.
    pub fn key_by(mut self, key: fn(&Request) -> Option<String>) -> Self {
        self.key = Some(key);
        self
    }
}

pub(crate) async fn limit(rl: RateLimit, req: Request, next: Next) -> Response<Body> {
    let ctx = req.extensions().get::<Ctx>().cloned();
    // Cloudflare sets this header itself, so clients can't spoof it. It's absent in local dev.
    let ip = || req.headers().get("cf-connecting-ip").and_then(|v| v.to_str().ok()).unwrap_or("unknown").to_owned();
    // The prefix keeps a custom key from ever landing on another client's IP counter.
    let client = rl.key.and_then(|key| key(&req)).map(|k| format!("key:{k}")).unwrap_or_else(ip);
    match check(rl, ctx, client).await {
        Ok(None) => next.run(req).await,
        Ok(Some(retry_after)) => {
            let mut res = Error::new(StatusCode::TOO_MANY_REQUESTS, "too many requests").into_response();
            res.headers_mut().extend(retry_after.map(|v| (RETRY_AFTER, v)));
            res
        }
        // Fails closed: a limiter that can't count must not let everything through.
        Err(e) => e.into_response(),
    }
}

/// `Some(retry-after)` when the client is over its limit.
async fn check(rl: RateLimit, ctx: Option<Ctx>, client: String) -> Result<Option<Option<axum::http::HeaderValue>>> {
    let ctx = ctx.ok_or_else(|| Error::internal("rate limit: request has no Ctx"))?;
    let stub = ctx.durable(rl.binding)?.get(&format!("{}:{client}", rl.name))?;
    let uri = format!("https://doko.do/?limit={}&window_secs={}", rl.limit, rl.window_secs);
    let res = stub.fetch(Request::get(uri).body(Body::empty())?).await?;
    match res.status() {
        StatusCode::OK => Ok(None),
        StatusCode::TOO_MANY_REQUESTS => Ok(Some(res.headers().get(RETRY_AFTER).cloned())),
        status => Err(Error::internal(format!("rate limiter `{}` answered {status}", rl.binding.0))),
    }
}

/// The Durable Object class behind [`RateLimit`]: one instance per limit name and client IP.
/// Export it with `doko::app!(App, objects: [RateLimiter])` and bind it with
/// `[do.RATE_LIMITER] class_name = "RateLimiter"`.
pub struct RateLimiter(State);

#[derive(Deserialize)]
struct Params {
    limit: u64,
    window_secs: u64,
}

const WINDOW_KEY: &str = "window";

impl DurableObject for RateLimiter {
    fn new(state: State, _: Env) -> Self {
        Self(state)
    }

    async fn fetch(&self, req: Request) -> Result<Response<Body>> {
        let Query(p) = Query::<Params>::try_from_uri(req.uri())?;
        let window_ms = p.window_secs.saturating_mul(1000);
        let storage = self.0.storage();
        // Read-then-write is atomic here: an object takes no other request while it awaits its storage.
        let stored = storage.get(WINDOW_KEY).await?;
        match admit(stored, Date::now().as_millis(), window_ms, p.limit) {
            Ok(window) => {
                if window.1 == 1 {
                    // The alarm empties this object once its window is over, so idle clients store nothing.
                    storage.set_alarm(Duration::from_millis(window_ms)).await?;
                }
                storage.put(WINDOW_KEY, window).await?;
                Ok(StatusCode::OK.into_response())
            }
            Err(retry_secs) => Ok((StatusCode::TOO_MANY_REQUESTS, [(RETRY_AFTER, retry_secs.to_string())]).into_response()),
        }
    }

    async fn alarm(&self) -> Result<()> {
        Ok(self.0.storage().delete_all().await?)
    }
}

/// Counts one request at `now` into the stored `(window start ms, count)`. Returns the window to
/// store, or `Err(seconds until it ends)` when it is full. A window starts at its first request.
fn admit(stored: Option<(u64, u64)>, now: u64, window_ms: u64, limit: u64) -> Result<(u64, u64), u64> {
    let (start, count) = stored.filter(|(start, _)| now.saturating_sub(*start) < window_ms).unwrap_or((now, 0));
    if count >= limit {
        return Err(start.saturating_add(window_ms).saturating_sub(now).div_ceil(1000));
    }
    Ok((start, count + 1))
}

#[cfg(test)]
mod tests {
    use super::admit;

    #[test]
    fn counts_requests_in_fixed_windows() {
        assert_eq!(admit(None, 5_000, 60_000, 2), Ok((5_000, 1)));
        assert_eq!(admit(Some((5_000, 1)), 6_000, 60_000, 2), Ok((5_000, 2)));
        assert_eq!(admit(Some((5_000, 2)), 6_500, 60_000, 2), Err(59), "58.5s left, rounded up");
        assert_eq!(admit(Some((5_000, 2)), 64_999, 60_000, 2), Err(1));
        assert_eq!(admit(Some((5_000, 2)), 65_000, 60_000, 2), Ok((65_000, 1)), "a new window");
        assert_eq!(admit(None, 0, 60_000, 0), Err(60), "limit 0 blocks everything");
    }
}
