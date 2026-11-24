//! Edge caching on the Workers Cache API.
//!
//! - [`Ctx::cache`] caches JSON values by key: `ctx.cache().get_or_set("k", 60, async { .. })`.
//! - [`RouterExt::cache`] caches whole GET responses, with stale-while-revalidate and tags.
//!
//! The Cache API is per data center, and is a no-op on `*.workers.dev`, so production needs a custom domain.
//! [`Cache::tier`] adds KV behind the value cache, which is global and works everywhere.
//! Tags make purges global: each tag has a version in KV, and that version is part of the cache key,
//! so [`Cache::purge_tag`] reaches every data center once KV propagates (up to ~60s).

use std::cell::Cell;

use axum::{
    Router,
    body::{Body, HttpBody, to_bytes},
    extract::Request,
    http::{
        HeaderMap, HeaderValue, Method, Response, StatusCode, Uri,
        header::{AUTHORIZATION, CACHE_CONTROL, SET_COOKIE, VARY},
    },
    middleware::{self, Next},
    response::IntoResponse,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
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
// Kept off the stored copy for the same reason as the policy's `Vary`: see `respond`.
const ORIGINAL_VARY: &str = "x-doko-vary";

/// Key-value cache on the Cache API, from [`Ctx::cache`]. Values are JSON.
pub struct Cache {
    ctx: Ctx,
    kv: Option<KvBinding>,
}

impl Cache {
    pub(crate) fn new(ctx: Ctx) -> Self {
        Self { ctx, kv: None }
    }

    /// Adds `kv` as a second tier: `ctx.cache().tier(CACHE)`. Values are written to both, and a
    /// miss in this data center's cache is answered from KV, so a value computed anywhere is
    /// reused everywhere. KV keeps a value for at least 60 seconds, and reads cost a KV operation.
    pub fn tier(mut self, kv: KvBinding) -> Self {
        self.kv = Some(kv);
        self
    }

    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let url = format!("{DATA_ORIGIN}{}", encode(key));
        let local: Result<Option<T>> = SendFuture::new({
            let url = url.clone();
            async move {
                match worker::Cache::default().get(url.as_str(), true).await? {
                    Some(mut res) => Ok(Some(res.json::<T>().await?)),
                    None => Ok(None),
                }
            }
        })
        .await;
        let (None, Some(kv)) = (local.as_ref().ok().and_then(Option::as_ref), self.kv) else {
            return local;
        };
        // Stored with its expiry, so the copy made here doesn't outlive the original.
        let Some((expires_ms, value)) = self.ctx.kv(kv)?.get::<(u64, Value)>(&kv_key(key)).await? else {
            return Ok(None);
        };
        let ttl_secs = expires_ms.saturating_sub(Date::now().as_millis()) / 1000;
        if ttl_secs == 0 {
            return Ok(None);
        }
        let body = value.to_string();
        self.ctx.wait_until(async move {
            // Best effort: without it, the next read asks KV again.
            let _ = put_json_entry(url, body, ttl_secs).await;
        });
        Ok(Some(serde_json::from_value(value)?))
    }

    pub async fn put<T: Serialize>(&self, key: &str, value: &T, ttl_secs: u64) -> Result<()> {
        let url = format!("{DATA_ORIGIN}{}", encode(key));
        let value = serde_json::to_value(value)?;
        SendFuture::new(put_json_entry(url, value.to_string(), ttl_secs)).await?;
        match self.kv {
            Some(kv) => put_kv(&self.ctx, kv, key, &value, ttl_secs).await,
            None => Ok(()),
        }
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
            Err(e) => console_error!(
                "{}",
                json!({ "level": "error", "component": "cache", "op": "get", "key": key, "error": e.message() })
            ),
        }
        let value = compute.await?;
        if ttl_secs == 0 {
            return Ok(value);
        }
        let json = match serde_json::to_value(&value) {
            Ok(v) => v,
            Err(e) => {
                console_error!(
                    "{}",
                    json!({ "level": "error", "component": "cache", "op": "serialize", "key": key, "error": e.to_string() })
                );
                return Ok(value);
            }
        };
        let url = format!("{DATA_ORIGIN}{}", encode(key));
        let (ctx, kv, key) = (self.ctx.clone(), self.kv, key.to_owned());
        self.ctx.wait_until(async move {
            let local = put_json_entry(url, json.to_string(), ttl_secs).await.map_err(crate::Error::from);
            let global = match kv {
                Some(kv) => put_kv(&ctx, kv, &key, &json, ttl_secs).await,
                None => Ok(()),
            };
            for e in [local, global].into_iter().filter_map(Result::err) {
                console_error!(
                    "{}",
                    json!({ "level": "error", "component": "cache", "op": "put", "error": e.message() })
                );
            }
        });
        Ok(value)
    }

    /// Removes `key` from this data center's cache, and from the KV [`tier`](Self::tier) if there
    /// is one. Other data centers keep their copies until the TTL, so for values that must
    /// disappear everywhere at once, use a short TTL or tags.
    pub async fn delete(&self, key: &str) -> Result<()> {
        let url = format!("{DATA_ORIGIN}{}", encode(key));
        SendFuture::new(async move { worker::Cache::default().delete(url.as_str(), true).await }).await?;
        match self.kv {
            Some(kv) => self.ctx.kv(kv)?.delete(&kv_key(key)).await,
            None => Ok(()),
        }
    }

    /// Invalidates every response cached under `tag`, in all data centers.
    pub async fn purge_tag(&self, kv: KvBinding, tag: &str) -> Result<()> {
        self.ctx.kv(kv)?.put(&tag_key(tag), &next_tag_version()).await
    }
}

fn kv_key(key: &str) -> String {
    format!("doko:cache:{key}")
}

async fn put_kv(ctx: &Ctx, kv: KvBinding, key: &str, value: &Value, ttl_secs: u64) -> Result<()> {
    let expires_ms = Date::now().as_millis().saturating_add(ttl_secs.saturating_mul(1000));
    // KV's shortest TTL is 60s; the stored expiry still ends the value on time.
    ctx.kv(kv)?.put_ttl(&kv_key(key), &(expires_ms, value), ttl_secs.max(60)).await
}

async fn put_json_entry(url: String, body: String, ttl_secs: u64) -> worker::Result<()> {
    let mut res = worker::Response::ok(body)?;
    let headers = res.headers_mut();
    headers.set("content-type", "application/json")?;
    headers.set("cache-control", &format!("public, max-age={ttl_secs}"))?;
    worker::Cache::default().put(url.as_str(), res).await
}

fn next_tag_version() -> u64 {
    thread_local! {
        static LAST_TAG_VERSION: Cell<u64> = const { Cell::new(0) };
    }
    LAST_TAG_VERSION.with(|last| {
        let next = Date::now().as_millis().max(last.get().saturating_add(1));
        last.set(next);
        next
    })
}

/// How [`RouterExt::cache`] caches responses.
#[derive(Clone, Debug)]
pub struct Policy {
    ttl: u64,
    stale: u64,
    tags: Option<(KvBinding, Vec<&'static str>)>,
    vary: Vec<&'static str>,
}

impl Policy {
    /// Serve from cache for `secs` after a response is stored.
    pub fn ttl(secs: u64) -> Self {
        Self { ttl: secs, stale: 0, tags: None, vary: Vec::new() }
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

    /// Keep a separate copy per value of these request headers, e.g. `&["accept-language"]`, and
    /// name them in the response's `Vary`. Every distinct value is its own cache entry, so pick
    /// headers with few values: not `cookie` or `user-agent`.
    pub fn vary(mut self, headers: &[&'static str]) -> Self {
        self.vary = headers.to_vec();
        self
    }
}

/// Adds `.cache(policy)`, `.rate_limit(limit)` and `.require_auth()` to routers.
pub trait RouterExt {
    /// Caches successful GET responses of the routes added so far. Requests with `Authorization`,
    /// and responses that are not 200, set cookies, are `private`/`no-store`, stream, or carry a
    /// `Vary` header naming something the policy's [`vary`](Policy::vary) doesn't, are never cached.
    /// Responses carry `x-doko-cache: HIT | STALE | MISS | BYPASS`.
    fn cache(self, policy: Policy) -> Self;

    /// Limits requests per client IP (or [`RateLimit::key_by`](crate::RateLimit::key_by)) to the routes added so far, counted together. Over the limit,
    /// they get a 429 with `retry-after`. If the limiter itself fails, they get a 500.
    fn rate_limit(self, limit: crate::RateLimit) -> Self;

    /// Answers 401 on the routes added so far unless the request has a session (see
    /// [`auth::Session`](crate::auth::Session)) or a valid `Authorization: Bearer` JWT.
    fn require_auth(self) -> Self;

    /// Like [`require_auth`](Self::require_auth), and answers 403 unless the session value or the
    /// JWT claims name `role`, as `"role": "admin"` or `"roles": ["admin", ..]`.
    fn require_role(self, role: &'static str) -> Self;
}

impl RouterExt for Router<Ctx> {
    fn cache(self, policy: Policy) -> Self {
        self.route_layer(middleware::from_fn(move |req: Request, next: Next| respond(policy.clone(), req, next)))
    }

    fn rate_limit(self, limit: crate::RateLimit) -> Self {
        self.route_layer(middleware::from_fn(move |req: Request, next: Next| crate::durable::limit(limit, req, next)))
    }

    fn require_auth(self) -> Self {
        self.route_layer(middleware::from_fn(|req: Request, next: Next| crate::auth::require(None, req, next)))
    }

    fn require_role(self, role: &'static str) -> Self {
        self.route_layer(middleware::from_fn(move |req: Request, next: Next| crate::auth::require(Some(role), req, next)))
    }
}

async fn respond(policy: Policy, req: Request, next: Next) -> Response<Body> {
    // Not stored with the response: the Cache API would then match on those request headers itself.
    let vary = HeaderValue::from_str(&policy.vary.join(", ")).ok().filter(|v| !v.is_empty());
    let mut res = lookup(policy, req, next).await;
    // A handler that already names all of them in its own `Vary` needs nothing added.
    if let Some(vary) = vary
        && !vary_covered(vary.to_str().unwrap_or("*"), &vary_names(res.headers()))
    {
        res.headers_mut().append(VARY, vary);
    }
    res
}

async fn lookup(policy: Policy, req: Request, next: Next) -> Response<Body> {
    let ctx = req.extensions().get::<Ctx>().cloned();
    let (Some(ctx), true, false) = (ctx, req.method() == Method::GET, req.headers().contains_key(AUTHORIZATION)) else {
        return mark(next.run(req).await, "BYPASS");
    };
    let uri = req.uri().clone();
    let key = match cache_key(&ctx, &policy, &uri, req.headers()).await {
        Ok(k) => k,
        Err(e) => {
            console_error!(
                "{}",
                json!({ "level": "error", "component": "cache", "op": "key", "uri": uri.to_string(), "error": e.message() })
            );
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
            if let Some(stored_at) = header_u64(&res, STORED_AT) {
                let age_ms = Date::now().as_millis().saturating_sub(stored_at);
                let ttl_ms = policy.ttl.saturating_mul(1000);
                let total_ms = policy.ttl.saturating_add(policy.stale).saturating_mul(1000);
                if age_ms < ttl_ms {
                    restore_headers(res.headers_mut());
                    return mark(res, "HIT");
                }
                if policy.stale > 0 && age_ms < total_ms {
                    restore_headers(res.headers_mut());
                    let (parts, _) = req.into_parts();
                    let fresh_req = Request::from_parts(parts, Body::empty());
                    ctx.wait_until(async move {
                        let fresh = next.run(fresh_req).await;
                        if let Some(stored) = storable(fresh, &policy).await.and_then(|(_, s)| s) {
                            put(key, stored).await;
                        }
                    });
                    return mark(res, "STALE");
                }
            }
        }
        Ok(None) => {}
        Err(e) => console_error!(
            "{}",
            json!({ "level": "error", "component": "cache", "op": "response_get", "error": e.to_string() })
        ),
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

/// Every name in the response's `Vary` headers.
fn vary_names(headers: &HeaderMap) -> Vec<&str> {
    headers.get_all(VARY).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(',')).map(str::trim).collect()
}

/// Whether every header a `Vary` value names is one of `known`. `Vary: *` never is.
fn vary_covered(vary: &str, known: &[&str]) -> bool {
    let mut names = vary.split(',').map(str::trim).filter(|n| !n.is_empty());
    names.all(|name| known.iter().any(|k| k.eq_ignore_ascii_case(name)))
}

fn restore_headers(headers: &mut HeaderMap) {
    headers.remove(STORED_AT);
    if let Some(vary) = headers.remove(ORIGINAL_VARY) {
        headers.insert(VARY, vary);
    }
    headers.remove(CACHE_CONTROL);
    if let Some(cc) = headers.remove(ORIGINAL_CC) {
        headers.insert(CACHE_CONTROL, cc);
    }
}

fn cache_control_forbids_caching(cc: &str) -> bool {
    cc.split(',').any(|directive| {
        let name = directive.split('=').next().unwrap_or("").trim();
        name.eq_ignore_ascii_case("private")
            || name.eq_ignore_ascii_case("no-store")
            || name.eq_ignore_ascii_case("no-cache")
    })
}

/// Splits a cacheable response into (client copy, stored copy). The stored copy is `None` when it
/// must not be cached. `None` overall means the body failed to read.
async fn storable(res: Response<Body>, policy: &Policy) -> Option<(Response<Body>, Option<Response<Body>>)> {
    let max_age = policy.ttl.saturating_add(policy.stale);
    let cc = res.headers().get(CACHE_CONTROL).and_then(|v| v.to_str().ok()).unwrap_or("");
    let size = res.body().size_hint().exact();
    // A handler's own `Vary` says its response depends on those request headers. The cache key
    // only tells requests apart by the policy's `vary` headers, so anything else can't be cached.
    let vary = vary_names(res.headers()).join(", ");
    let cacheable = max_age > 0
        && vary_covered(&vary, &policy.vary)
        && res.status() == StatusCode::OK
        && !res.headers().contains_key(SET_COOKIE)
        && !cache_control_forbids_caching(cc)
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
    if h.remove(VARY).is_some() {
        h.insert(ORIGINAL_VARY, HeaderValue::from_str(&vary).ok()?);
    }
    let cache_control = HeaderValue::from_str(&format!("public, max-age={max_age}")).expect("digits are valid header bytes");
    h.insert(CACHE_CONTROL, cache_control);
    h.insert(STORED_AT, HeaderValue::from(Date::now().as_millis()));
    Some((Response::from_parts(parts, Body::from(bytes)), Some(stored)))
}

async fn put(key: String, stored: Response<Body>) {
    let result = async {
        let res = worker::Response::try_from(stored)?;
        worker::Cache::default().put(key.as_str(), res).await
    };
    if let Err(e) = result.await {
        console_error!(
            "{}",
            json!({ "level": "error", "component": "cache", "op": "response_put", "error": e.to_string() })
        );
    }
}

/// The request URL, plus the request's `vary` header values, plus the current version of each tag,
/// so bumping a tag changes every key under it.
async fn cache_key(ctx: &Ctx, policy: &Policy, uri: &Uri, headers: &HeaderMap) -> Result<String> {
    let mut key = match uri.scheme() {
        Some(_) => uri.to_string(),
        None => format!("https://doko.local{}", uri.path_and_query().map_or("/", |p| p.as_str())),
    };
    if !policy.vary.is_empty() {
        key.push(if key.contains('?') { '&' } else { '?' });
        key.push_str("__doko_vary=");
        key.push_str(&vary_key(&policy.vary, headers));
    }
    if let Some((kv, tags)) = &policy.tags
        && !tags.is_empty()
    {
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

/// The values of the `vary` headers, encoded so that no header value can imitate another's.
fn vary_key(vary: &[&str], headers: &HeaderMap) -> String {
    let value = |name: &&str| encode(headers.get(*name).and_then(|v| v.to_str().ok()).unwrap_or(""));
    vary.iter().map(value).collect::<Vec<_>>().join(",")
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
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(key.len());
    for &b in key.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_keys() {
        assert_eq!(encode(""), "");
        assert_eq!(encode("notes:all"), "notes%3Aall");
        assert_eq!(encode("a b/ü"), "a%20b%2F%C3%BC");
        assert_eq!(encode("ok-_.~9"), "ok-_.~9");
    }

    #[test]
    fn keys_vary_headers_apart() {
        let mut h = HeaderMap::new();
        assert_eq!(vary_key(&["accept-language", "x-plan"], &h), ",");
        h.insert("accept-language", HeaderValue::from_static("de,en"));
        assert_eq!(vary_key(&["accept-language", "x-plan"], &h), "de%2Cen,", "a comma in a value can't split the key");
        h.insert("x-plan", HeaderValue::from_static("pro"));
        assert_eq!(vary_key(&["accept-language", "x-plan"], &h), "de%2Cen,pro");
    }

    #[test]
    fn only_caches_a_vary_the_policy_covers() {
        assert!(vary_covered("", &[]), "no Vary, nothing to cover");
        assert!(vary_covered("Accept-Language", &["accept-language", "x-plan"]));
        assert!(vary_covered("x-plan, accept-language", &["accept-language", "x-plan"]));
        assert!(!vary_covered("accept-language, cookie", &["accept-language"]));
        assert!(!vary_covered("accept-language", &[]));
        assert!(!vary_covered("*", &["accept-language"]));
        let mut h = HeaderMap::new();
        h.append(VARY, HeaderValue::from_static("a, b"));
        h.append(VARY, HeaderValue::from_static("c"));
        assert_eq!(vary_names(&h), ["a", "b", "c"]);
    }

    #[test]
    fn detects_cache_control_forbidding_directives() {
        assert!(cache_control_forbids_caching("private"));
        assert!(cache_control_forbids_caching("PRIVATE"));
        assert!(cache_control_forbids_caching("public, No-Store"));
        assert!(cache_control_forbids_caching("no-cache=\"Set-Cookie\""));
        assert!(cache_control_forbids_caching("max-age=0, no-cache = \"x\""));
        assert!(!cache_control_forbids_caching("public, max-age=60"));
        assert!(!cache_control_forbids_caching("x-private-ext=1"));
        assert!(!cache_control_forbids_caching(""));
    }
}

