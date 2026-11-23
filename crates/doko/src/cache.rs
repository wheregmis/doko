//! Edge caching on the Workers Cache API.
//!
//! - [`Ctx::cache`] caches JSON values by key: `ctx.cache().get_or_set("k", 60, async { .. })`.
//! - [`RouterExt::cache`] caches whole GET responses, with stale-while-revalidate and tags.
//!
//! The Cache API is per data center, and is a no-op on `*.workers.dev`, so production needs a custom domain.
//! Tags make purges global: each tag has a version in KV, and that version is part of the cache key,
//! so [`Cache::purge_tag`] reaches every data center once KV propagates (up to ~60s).

use axum::{
    Router,
    body::{Body, HttpBody, to_bytes},
    extract::Request,
    http::{
        HeaderValue, Method, Response, StatusCode, Uri,
        header::{AUTHORIZATION, CACHE_CONTROL, SET_COOKIE},
    },
    middleware::{self, Next},
    response::IntoResponse,
};
use serde::{Serialize, de::DeserializeOwned};
// worker::Cache is !Send, so each call creates it inside a SendFuture.
use worker::{Date, console_error, send::SendFuture};

use crate::{Ctx, KvBinding, Result};

/// Responses bigger than this, or with no known size (streams), are passed through uncached.
const MAX_CACHED_BYTES: u64 = 10 * 1024 * 1024;
const DATA_ORIGIN: &str = "https://doko.cache/data/";
const STATUS: &str = "x-doko-cache";
const STORED_AT: &str = "x-doko-stored-at";
// The stored copy's cache-control drives Cache API expiry; the handler's own value is kept here.
const ORIGINAL_CC: &str = "x-doko-cache-control";

/// Key-value cache on the Cache API, from [`Ctx::cache`]. Values are JSON.
pub struct Cache {
    ctx: Ctx,
}

impl Cache {
    pub(crate) fn new(ctx: Ctx) -> Self {
        Self { ctx }
    }

    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let url = format!("{DATA_ORIGIN}{}", encode(key));
        SendFuture::new(async move {
            match worker::Cache::default().get(url.as_str(), true).await? {
                Some(mut res) => Ok(Some(res.json::<T>().await?)),
                None => Ok(None),
            }
        })
        .await
    }

    pub async fn put<T: Serialize>(&self, key: &str, value: &T, ttl_secs: u64) -> Result<()> {
        let url = format!("{DATA_ORIGIN}{}", encode(key));
        let body = serde_json::to_string(value)?;
        SendFuture::new(async move {
            let mut res = worker::Response::ok(body)?;
            res.headers_mut().set("cache-control", &format!("public, max-age={ttl_secs}"))?;
            worker::Cache::default().put(url.as_str(), res).await
        })
        .await?;
        Ok(())
    }

    /// The cached value, or `compute`'s result, cached in the background for `ttl_secs`.
    /// A failing cache counts as a miss, so it never fails the request.
    pub async fn get_or_set<T, F>(&self, key: &str, ttl_secs: u64, compute: F) -> Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: Future<Output = Result<T>>,
    {
        match self.get(key).await {
            Ok(Some(v)) => return Ok(v),
            Ok(None) => {}
            Err(e) => console_error!("cache get `{key}`: {}", e.message()),
        }
        let value = compute.await?;
        let url = format!("{DATA_ORIGIN}{}", encode(key));
        let body = serde_json::to_string(&value)?;
        self.ctx.wait_until(async move {
            let put = async {
                let mut res = worker::Response::ok(body)?;
                res.headers_mut().set("cache-control", &format!("public, max-age={ttl_secs}"))?;
                worker::Cache::default().put(url.as_str(), res).await
            };
            if let Err(e) = put.await {
                console_error!("cache put: {e}");
            }
        });
        Ok(value)
    }

    /// Removes `key` in this data center only. Use a short TTL, or tags, for global invalidation.
    pub async fn delete(&self, key: &str) -> Result<()> {
        let url = format!("{DATA_ORIGIN}{}", encode(key));
        SendFuture::new(async move { worker::Cache::default().delete(url.as_str(), true).await }).await?;
        Ok(())
    }

    /// Invalidates every response cached under `tag`, in all data centers.
    pub async fn purge_tag(&self, kv: KvBinding, tag: &str) -> Result<()> {
        self.ctx.kv(kv)?.put(&tag_key(tag), &Date::now().as_millis()).await
    }
}

/// How [`RouterExt::cache`] caches responses.
#[derive(Clone, Debug)]
pub struct Policy {
    ttl: u64,
    stale: u64,
    tags: Option<(KvBinding, Vec<&'static str>)>,
}

impl Policy {
    /// Serve from cache for `secs` after a response is stored.
    pub fn ttl(secs: u64) -> Self {
        Self { ttl: secs, stale: 0, tags: None }
    }

    /// After the TTL, keep serving the stale copy for `secs` more while refreshing it in the background.
    pub fn stale_while_revalidate(mut self, secs: u64) -> Self {
        self.stale = secs;
        self
    }

    /// Tag the cached responses so [`Cache::purge_tag`] can invalidate them. Tag versions live in `kv`.
    pub fn tags(mut self, kv: KvBinding, tags: &[&'static str]) -> Self {
        self.tags = Some((kv, tags.to_vec()));
        self
    }
}

/// Adds `.cache(policy)` to routers.
pub trait RouterExt {
    /// Caches successful GET responses of the routes added so far. Requests with `Authorization`,
    /// and responses that are not 200, set cookies, are `private`/`no-store`, or stream, are never cached.
    /// Responses carry `x-doko-cache: HIT | STALE | MISS | BYPASS`.
    fn cache(self, policy: Policy) -> Self;
}

impl RouterExt for Router<Ctx> {
    fn cache(self, policy: Policy) -> Self {
        self.route_layer(middleware::from_fn(move |req: Request, next: Next| respond(policy.clone(), req, next)))
    }
}

async fn respond(policy: Policy, req: Request, next: Next) -> Response<Body> {
    let ctx = req.extensions().get::<Ctx>().cloned();
    let (Some(ctx), true, false) = (ctx, req.method() == Method::GET, req.headers().contains_key(AUTHORIZATION)) else {
        return mark(next.run(req).await, "BYPASS");
    };
    let uri = req.uri().clone();
    let key = match cache_key(&ctx, &policy, &uri).await {
        Ok(k) => k,
        Err(e) => {
            console_error!("cache key: {}", e.message());
            return mark(next.run(req).await, "BYPASS");
        }
    };

    let cached = SendFuture::new({
        let key = key.clone();
        // Convert inside: worker::Response is !Send and must not live across later awaits.
        async move { worker::Cache::default().get(key.as_str(), false).await.map(|r| r.map(Response::<Body>::from)) }
    })
    .await;
    match cached {
        Ok(Some(mut res)) => {
            let age_ms = Date::now().as_millis().saturating_sub(header_u64(&res, STORED_AT).unwrap_or(0));
            let headers = res.headers_mut();
            headers.remove(STORED_AT);
            headers.remove(CACHE_CONTROL);
            if let Some(cc) = headers.remove(ORIGINAL_CC) {
                headers.insert(CACHE_CONTROL, cc);
            }
            if age_ms < policy.ttl * 1000 {
                return mark(res, "HIT");
            }
            let fresh_req = clone_get(&req);
            ctx.wait_until(async move {
                let fresh = next.run(fresh_req).await;
                if let Some(stored) = storable(fresh, &policy).await.and_then(|(_, s)| s) {
                    put(key, stored).await;
                }
            });
            return mark(res, "STALE");
        }
        Ok(None) => {}
        Err(e) => console_error!("cache get: {e}"),
    }

    match storable(next.run(req).await, &policy).await {
        Some((res, Some(stored))) => {
            ctx.wait_until(put(key, stored));
            mark(res, "MISS")
        }
        Some((res, None)) => mark(res, "BYPASS"),
        None => crate::Error::internal("reading response body for cache").into_response(),
    }
}

/// Splits a cacheable response into (client copy, stored copy). The stored copy is `None` when it
/// must not be cached. `None` overall means the body failed to read.
async fn storable(res: Response<Body>, policy: &Policy) -> Option<(Response<Body>, Option<Response<Body>>)> {
    let cc = res.headers().get(CACHE_CONTROL).and_then(|v| v.to_str().ok()).unwrap_or("");
    let size = res.body().size_hint().exact();
    let cacheable = res.status() == StatusCode::OK
        && !res.headers().contains_key(SET_COOKIE)
        && !["private", "no-store", "no-cache"].iter().any(|d| cc.contains(d))
        && size.is_some_and(|n| n <= MAX_CACHED_BYTES);
    if !cacheable {
        return Some((res, None));
    }
    let (parts, body) = res.into_parts();
    let bytes = to_bytes(body, MAX_CACHED_BYTES as usize).await.ok()?;
    let mut stored = Response::new(Body::from(bytes.clone()));
    *stored.status_mut() = parts.status;
    *stored.headers_mut() = parts.headers.clone();
    let h = stored.headers_mut();
    if let Some(original) = h.remove(CACHE_CONTROL) {
        h.insert(ORIGINAL_CC, original);
    }
    let max_age = policy.ttl + policy.stale;
    h.insert(CACHE_CONTROL, HeaderValue::from_str(&format!("public, max-age={max_age}")).ok()?);
    h.insert(STORED_AT, HeaderValue::from(Date::now().as_millis()));
    Some((Response::from_parts(parts, Body::from(bytes)), Some(stored)))
}

async fn put(key: String, stored: Response<Body>) {
    let result = async {
        let res = worker::Response::try_from(stored)?;
        worker::Cache::default().put(key.as_str(), res).await
    };
    if let Err(e) = result.await {
        console_error!("cache put: {e}");
    }
}

/// The request URL, plus the current version of each tag, so bumping a tag changes every key under it.
async fn cache_key(ctx: &Ctx, policy: &Policy, uri: &Uri) -> Result<String> {
    let mut key = match uri.scheme() {
        Some(_) => uri.to_string(),
        None => format!("https://doko.local{}", uri.path_and_query().map_or("/", |p| p.as_str())),
    };
    if let Some((kv, tags)) = &policy.tags {
        let kv = ctx.kv(*kv)?;
        let mut versions = Vec::with_capacity(tags.len());
        for tag in tags {
            versions.push(kv.get::<u64>(&tag_key(tag)).await?.unwrap_or(0).to_string());
        }
        key.push(if key.contains('?') { '&' } else { '?' });
        key.push_str("__doko_tags=");
        key.push_str(&versions.join("."));
    }
    Ok(key)
}

fn clone_get(req: &Request) -> Request {
    let mut r = Request::new(Body::empty());
    *r.method_mut() = Method::GET;
    *r.uri_mut() = req.uri().clone();
    *r.headers_mut() = req.headers().clone();
    *r.extensions_mut() = req.extensions().clone();
    r
}

fn mark(mut res: Response<Body>, status: &'static str) -> Response<Body> {
    res.headers_mut().insert(STATUS, HeaderValue::from_static(status));
    res
}

fn header_u64(res: &Response<Body>, name: &str) -> Option<u64> {
    res.headers().get(name)?.to_str().ok()?.parse().ok()
}

fn tag_key(tag: &str) -> String {
    format!("doko:cache-tag:{tag}")
}

/// Percent-encodes everything but unreserved URL characters.
fn encode(key: &str) -> String {
    key.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_keys() {
        assert_eq!(encode("notes:all"), "notes%3Aall");
        assert_eq!(encode("a b/ü"), "a%20b%2F%C3%BC");
        assert_eq!(encode("ok-_.~9"), "ok-_.~9");
    }
}

