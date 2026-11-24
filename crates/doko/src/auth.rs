//! Sessions, JWTs and password hashing, on the runtime's WebCrypto.
//!
//! Everything is wired by name, so there is nothing to configure in code:
//!
//! | Feature | Needs |
//! |---|---|
//! | [`Session`] | secret `SESSION_SECRET`, and `[kv.SESSIONS]` or `[do.SESSIONS] class_name = "SessionStore"` |
//! | [`Claims`] and [`sign`] (HS256) | secret `JWT_SECRET` |
//! | [`Claims`] for RS256 tokens | secret `JWT_PUBLIC_KEY`, the issuer's public key as PEM |
//!
//! [`RouterExt::require_auth`](crate::RouterExt::require_auth) turns away requests with neither.

use std::time::Duration;

use axum::{
    body::{Body, to_bytes},
    extract::{FromRequestParts, Query, Request},
    http::{
        HeaderMap, HeaderValue, Method, Response, StatusCode,
        header::{AUTHORIZATION, COOKIE, SET_COOKIE},
        request::Parts,
    },
    middleware::Next,
    response::{IntoResponse, IntoResponseParts, ResponseParts},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use worker::{
    Date, Env,
    durable::State,
    js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array},
    send::SendFuture,
    wasm_bindgen::{JsCast, JsValue},
    wasm_bindgen_futures::JsFuture,
};

use crate::{Ctx, DoBinding, Durable, DurableObject, Error, Kv, KvBinding, Result};

const SESSION_SECRET: &str = "SESSION_SECRET";
const JWT_SECRET: &str = "JWT_SECRET";
const JWT_PUBLIC_KEY: &str = "JWT_PUBLIC_KEY";
/// The `[kv.SESSIONS]` or `[do.SESSIONS]` binding holding session data.
const SESSIONS: &str = "SESSIONS";
const SESSION_COOKIE: &str = "doko_session";
const SESSION_TTL_SECS: u64 = 7 * 24 * 60 * 60;
/// The most PBKDF2 iterations the Workers runtime accepts.
const PBKDF2_ITERATIONS: u32 = 100_000;
const PBKDF2: &str = "pbkdf2-sha256";

// --- WebCrypto. JS values are !Send, so every public fn wraps these in a SendFuture. ---

type Js<T = JsValue> = std::result::Result<T, JsValue>;

fn js_err(e: JsValue) -> Error {
    Error::internal(format!("crypto: {e:?}"))
}

/// `crypto.subtle.<method>(...args)`, awaited.
async fn subtle(method: &str, args: &[JsValue]) -> Js {
    let crypto = Reflect::get(&worker::js_sys::global(), &"crypto".into())?;
    let subtle = Reflect::get(&crypto, &"subtle".into())?;
    let f: Function = Reflect::get(&subtle, &method.into())?.dyn_into()?;
    let promise: Promise = f.apply(&subtle, &args.iter().collect::<Array>())?.dyn_into()?;
    JsFuture::from(promise).await
}

fn obj(fields: &[(&str, JsValue)]) -> JsValue {
    let o = Object::new();
    for (k, v) in fields {
        let _ = Reflect::set(&o, &(*k).into(), v);
    }
    o.into()
}

fn bytes(b: &[u8]) -> JsValue {
    Uint8Array::from(b).into()
}

fn random_bytes<const N: usize>() -> Js<[u8; N]> {
    let crypto = Reflect::get(&worker::js_sys::global(), &"crypto".into())?;
    let f: Function = Reflect::get(&crypto, &"getRandomValues".into())?.dyn_into()?;
    let mut out = [0; N];
    Uint8Array::new(&f.call1(&crypto, &Uint8Array::new_with_length(N as u32))?).copy_to(&mut out);
    Ok(out)
}

async fn hmac_key(secret: &[u8], usage: &str) -> Js {
    let alg = obj(&[("name", "HMAC".into()), ("hash", "SHA-256".into())]);
    subtle("importKey", &["raw".into(), bytes(secret), alg, false.into(), Array::of1(&usage.into()).into()]).await
}

async fn hmac_sign(secret: &[u8], data: &[u8]) -> Js<Vec<u8>> {
    let key = hmac_key(secret, "sign").await?;
    Ok(Uint8Array::new(&subtle("sign", &["HMAC".into(), key, bytes(data)]).await?).to_vec())
}

/// Compares in constant time, inside the runtime.
async fn hmac_verify(secret: &[u8], data: &[u8], signature: &[u8]) -> Js<bool> {
    let key = hmac_key(secret, "verify").await?;
    Ok(subtle("verify", &["HMAC".into(), key, bytes(signature), bytes(data)]).await?.is_truthy())
}

async fn rsa_verify(public_key_der: &[u8], data: &[u8], signature: &[u8]) -> Js<bool> {
    const NAME: &str = "RSASSA-PKCS1-v1_5";
    let alg = obj(&[("name", NAME.into()), ("hash", "SHA-256".into())]);
    let usages = Array::of1(&"verify".into()).into();
    let key = subtle("importKey", &["spki".into(), bytes(public_key_der), alg, false.into(), usages]).await?;
    Ok(subtle("verify", &[NAME.into(), key, bytes(signature), bytes(data)]).await?.is_truthy())
}

async fn pbkdf2(password: &[u8], salt: &[u8], iterations: u32) -> Js<Vec<u8>> {
    let usages = Array::of1(&"deriveBits".into()).into();
    let key = subtle("importKey", &["raw".into(), bytes(password), "PBKDF2".into(), false.into(), usages]).await?;
    let alg = obj(&[
        ("name", "PBKDF2".into()),
        ("hash", "SHA-256".into()),
        ("salt", bytes(salt)),
        ("iterations", iterations.into()),
    ]);
    Ok(Uint8Array::new(&subtle("deriveBits", &[alg, key, 256.into()]).await?).to_vec())
}

// --- Passwords ---

/// Hashes a password for storage, with PBKDF2-SHA256 and a random salt. The work runs in the
/// runtime's native crypto, so it fits Workers' CPU limits.
pub async fn hash_password(password: &str) -> Result<String> {
    SendFuture::new(async {
        let salt = random_bytes::<16>()?;
        let hash = pbkdf2(password.as_bytes(), &salt, PBKDF2_ITERATIONS).await?;
        Ok(format!("{PBKDF2}${PBKDF2_ITERATIONS}${}${}", STANDARD.encode(salt), STANDARD.encode(hash)))
    })
    .await
    .map_err(js_err)
}

/// Whether `password` matches a hash from [`hash_password`]. A malformed hash is `false`.
pub async fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let Some((iterations, salt, expected)) = parse_password_hash(hash) else {
        return Ok(false);
    };
    let actual = SendFuture::new(pbkdf2(password.as_bytes(), &salt, iterations)).await.map_err(js_err)?;
    // No early exit, so timing doesn't reveal how many leading bytes matched.
    Ok(actual.len() == expected.len() && actual.iter().zip(&expected).fold(0, |diff, (a, b)| diff | (a ^ b)) == 0)
}

fn parse_password_hash(hash: &str) -> Option<(u32, Vec<u8>, Vec<u8>)> {
    let mut parts = hash.split('$');
    let (scheme, iterations, salt, key) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let iterations: u32 = iterations.parse().ok()?;
    // The runtime rejects more iterations than this; fewer than 1 is not a hash.
    if scheme != PBKDF2 || parts.next().is_some() || !(1..=PBKDF2_ITERATIONS).contains(&iterations) {
        return None;
    }
    Some((iterations, STANDARD.decode(salt).ok()?, STANDARD.decode(key).ok()?))
}

// --- JWT ---

/// Issues an HS256 JWT signed with the `JWT_SECRET` secret, expiring in `ttl_secs`.
/// `claims` must serialize to a JSON object; `iat` and `exp` are added to it.
pub async fn sign<T: Serialize>(ctx: &Ctx, claims: &T, ttl_secs: u64) -> Result<String> {
    let Value::Object(mut claims) = serde_json::to_value(claims)? else {
        return Err(Error::internal("JWT claims must serialize to a JSON object"));
    };
    let now = Date::now().as_millis() / 1000;
    claims.insert("iat".into(), now.into());
    claims.insert("exp".into(), now.saturating_add(ttl_secs).into());
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?)
    );
    let secret = ctx.secret(JWT_SECRET)?;
    let signature = SendFuture::new(hmac_sign(secret.as_bytes(), input.as_bytes())).await.map_err(js_err)?;
    Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature)))
}

/// The claims of a valid `Authorization: Bearer <jwt>` token, or a 401.
///
/// ```ignore
/// #[derive(Deserialize)]
/// struct User { sub: String }
///
/// async fn me(Claims(user): Claims<User>) -> String { user.sub }
/// ```
///
/// HS256 tokens are checked against the `JWT_SECRET` secret, RS256 tokens against `JWT_PUBLIC_KEY`.
/// Expired tokens (`exp`) and tokens not yet valid (`nbf`) are rejected.
pub struct Claims<T>(pub T);

impl<T: DeserializeOwned> FromRequestParts<Ctx> for Claims<T> {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, ctx: &Ctx) -> Result<Self> {
        let token = bearer(&parts.headers).ok_or_else(Error::unauthorized)?.to_owned();
        let claims = verify_jwt(ctx, &token).await?;
        Ok(Claims(serde_json::from_value(claims).map_err(|_| Error::unauthorized())?))
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers.get(AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ")
}

/// A token split into (signed part, algorithm, claims, signature), if it is well formed.
fn parse_jwt(token: &str) -> Option<(&str, String, Value, Vec<u8>)> {
    let (input, signature) = token.rsplit_once('.')?;
    let (header, claims) = input.split_once('.')?;
    let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header).ok()?).ok()?;
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).ok()?).ok()?;
    Some((input, header["alg"].as_str()?.to_owned(), claims, URL_SAFE_NO_PAD.decode(signature).ok()?))
}

/// Whether the claims' `exp` and `nbf` allow the token at `now_secs`. A token without `exp` never expires.
fn in_validity_window(claims: &Value, now_secs: u64) -> bool {
    let time = |name: &str| claims.get(name).map(|v| v.as_f64());
    // A present but non-numeric time is invalid, not ignored.
    !matches!(time("exp"), Some(exp) if exp.is_none_or(|exp| now_secs as f64 >= exp))
        && !matches!(time("nbf"), Some(nbf) if nbf.is_none_or(|nbf| (now_secs as f64) < nbf))
}

fn pem_to_der(pem: &str) -> Option<Vec<u8>> {
    let body: String = pem.split("-----").nth(2)?.split_whitespace().collect();
    STANDARD.decode(body).ok()
}

async fn verify_jwt(ctx: &Ctx, token: &str) -> Result<Value> {
    let (input, alg, claims, signature) = parse_jwt(token).ok_or_else(Error::unauthorized)?;
    // Each algorithm has its own key, so a token can't pick HS256 to be checked against the public key.
    let valid = match alg.as_str() {
        "HS256" => {
            let secret = ctx.secret(JWT_SECRET)?;
            SendFuture::new(hmac_verify(secret.as_bytes(), input.as_bytes(), &signature)).await.map_err(js_err)?
        }
        "RS256" => {
            let der = pem_to_der(&ctx.secret(JWT_PUBLIC_KEY)?)
                .ok_or_else(|| Error::internal(format!("`{JWT_PUBLIC_KEY}` is not a PEM public key")))?;
            SendFuture::new(rsa_verify(&der, input.as_bytes(), &signature)).await.map_err(js_err)?
        }
        _ => false,
    };
    if valid && in_validity_window(&claims, Date::now().as_millis() / 1000) { Ok(claims) } else { Err(Error::unauthorized()) }
}

// --- Sessions ---

/// The request's server-side session. Extract it in any handler; it is empty until [`set`](Self::set).
///
/// ```ignore
/// async fn login(session: Session, Json(c): Json<Credentials>) -> Result<SetCookie> {
///     let user = check(c).await?;
///     session.set(&user).await          // return the cookie with the response
/// }
///
/// async fn me(session: Session) -> Result<Json<User>> {
///     session.get()?.map(Json).ok_or_else(Error::unauthorized)
/// }
/// ```
///
/// The cookie holds only a random id, signed with the `SESSION_SECRET` secret. The data lives in
/// `[kv.SESSIONS]`, or in `[do.SESSIONS] class_name = "SessionStore"` when you declare that instead:
/// KV can serve a session for up to a minute after logout in other regions, the object never does.
/// Sessions last 7 days.
#[derive(Clone)]
pub struct Session {
    ctx: Ctx,
    id: Option<String>,
    data: Option<Value>,
}

/// A `Set-Cookie` header from [`Session::set`] or [`Session::clear`]. Return it from the handler,
/// alone or in a tuple: `Ok((cookie, Json(body)))`.
#[must_use = "the session only reaches the browser if this is returned with the response"]
pub struct SetCookie(HeaderValue);

impl IntoResponseParts for SetCookie {
    type Error = std::convert::Infallible;

    fn into_response_parts(self, mut res: ResponseParts) -> std::result::Result<ResponseParts, Self::Error> {
        res.headers_mut().append(SET_COOKIE, self.0);
        Ok(res)
    }
}

impl IntoResponse for SetCookie {
    fn into_response(self) -> Response<Body> {
        (self, ()).into_response()
    }
}

fn set_cookie(value: &str, max_age: u64) -> Result<SetCookie> {
    // Lax keeps the cookie off cross-site POSTs, which is the CSRF defence for cookie sessions.
    let cookie = format!("{SESSION_COOKIE}={value}; Path=/; Max-Age={max_age}; HttpOnly; Secure; SameSite=Lax");
    Ok(SetCookie(HeaderValue::from_str(&cookie)?))
}

fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(SESSION_COOKIE)?.strip_prefix('='))
}

enum Store {
    Kv(Kv),
    Durable(Durable),
}

impl Store {
    fn open(ctx: &Ctx) -> Result<Self> {
        // The env check is by type: a KV binding called SESSIONS is not a Durable Object namespace.
        if ctx.env().durable_object(SESSIONS).is_ok() {
            return Ok(Self::Durable(ctx.durable(DoBinding::new(SESSIONS))?));
        }
        ctx.kv(KvBinding::new(SESSIONS)).map(Self::Kv).map_err(|_| {
            Error::internal(format!("sessions need a [kv.{SESSIONS}] or [do.{SESSIONS}] binding in doko.toml"))
        })
    }

    async fn object(objects: &Durable, id: &str, method: Method, body: Body) -> Result<Response<Body>> {
        let uri = format!("https://doko.do/?ttl_secs={SESSION_TTL_SECS}");
        objects.get(id)?.fetch(Request::builder().method(method).uri(uri).body(body)?).await
    }

    async fn get(&self, id: &str) -> Result<Option<Value>> {
        match self {
            Self::Kv(kv) => kv.get(&kv_key(id)).await,
            Self::Durable(objects) => {
                let res = Self::object(objects, id, Method::GET, Body::empty()).await?;
                match res.status() {
                    StatusCode::OK => Ok(Some(serde_json::from_slice(&to_bytes(res.into_body(), usize::MAX).await?)?)),
                    StatusCode::NOT_FOUND => Ok(None),
                    status => Err(Error::internal(format!("session store answered {status}"))),
                }
            }
        }
    }

    async fn put(&self, id: &str, data: &Value) -> Result<()> {
        match self {
            Self::Kv(kv) => kv.put_ttl(&kv_key(id), data, SESSION_TTL_SECS).await,
            Self::Durable(objects) => {
                Self::ok(Self::object(objects, id, Method::PUT, Body::from(serde_json::to_vec(data)?)).await?)
            }
        }
    }

    async fn delete(&self, id: &str) -> Result<()> {
        match self {
            Self::Kv(kv) => kv.delete(&kv_key(id)).await,
            Self::Durable(objects) => Self::ok(Self::object(objects, id, Method::DELETE, Body::empty()).await?),
        }
    }

    fn ok(res: Response<Body>) -> Result<()> {
        if res.status().is_success() { Ok(()) } else { Err(Error::internal(format!("session store answered {}", res.status()))) }
    }
}

fn kv_key(id: &str) -> String {
    format!("doko:session:{id}")
}

impl Session {
    /// The session named by a correctly signed cookie, or an empty one.
    async fn load(ctx: &Ctx, cookie: Option<String>) -> Result<Self> {
        let mut session = Self { ctx: ctx.clone(), id: None, data: None };
        let Some((id, signature)) = cookie.as_deref().and_then(|c| c.split_once('.')) else {
            return Ok(session);
        };
        let Ok(signature) = URL_SAFE_NO_PAD.decode(signature) else {
            return Ok(session);
        };
        let secret = ctx.secret(SESSION_SECRET)?;
        if SendFuture::new(hmac_verify(secret.as_bytes(), id.as_bytes(), &signature)).await.map_err(js_err)? {
            session.data = Store::open(ctx)?.get(id).await?;
            session.id = session.data.is_some().then(|| id.to_owned());
        }
        Ok(session)
    }

    /// The value stored by [`set`](Self::set), or `None` when there is no session.
    pub fn get<T: DeserializeOwned>(&self) -> Result<Option<T>> {
        Ok(self.data.clone().map(serde_json::from_value).transpose()?)
    }

    /// Starts a session holding `value`, replacing the current one. The session id is always new,
    /// so an id planted before login is worthless after it.
    pub async fn set<T: Serialize>(&self, value: &T) -> Result<SetCookie> {
        let store = Store::open(&self.ctx)?;
        let secret = self.ctx.secret(SESSION_SECRET)?;
        let id = URL_SAFE_NO_PAD.encode(random_bytes::<16>().map_err(js_err)?);
        let signature = SendFuture::new(hmac_sign(secret.as_bytes(), id.as_bytes())).await.map_err(js_err)?;
        store.put(&id, &serde_json::to_value(value)?).await?;
        if let Some(old) = &self.id {
            store.delete(old).await?;
        }
        set_cookie(&format!("{id}.{}", URL_SAFE_NO_PAD.encode(signature)), SESSION_TTL_SECS)
    }

    /// Ends the session: deletes its data and expires the cookie.
    pub async fn clear(&self) -> Result<SetCookie> {
        if let Some(id) = &self.id {
            Store::open(&self.ctx)?.delete(id).await?;
        }
        set_cookie("", 0)
    }
}

impl FromRequestParts<Ctx> for Session {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, ctx: &Ctx) -> Result<Self> {
        // `require_auth` has already loaded it for the routes it guards.
        if let Some(session) = parts.extensions.remove::<Session>() {
            return Ok(session);
        }
        let cookie = session_cookie(&parts.headers).map(str::to_owned);
        Session::load(ctx, cookie).await
    }
}

/// The middleware behind `RouterExt::require_auth`.
pub(crate) async fn require(mut req: Request, next: Next) -> Response<Body> {
    let token = bearer(req.headers()).map(str::to_owned);
    let cookie = session_cookie(req.headers()).map(str::to_owned);
    let ctx = req.extensions().get::<Ctx>().cloned();
    let checked = async {
        let ctx = ctx.ok_or_else(|| Error::internal("require_auth: request has no Ctx"))?;
        if let Some(token) = token {
            return verify_jwt(&ctx, &token).await.map(|_| None);
        }
        let session = Session::load(&ctx, cookie).await?;
        if session.data.is_some() { Ok(Some(session)) } else { Err(Error::unauthorized()) }
    };
    match checked.await {
        Ok(session) => {
            if let Some(session) = session {
                // Handed to the `Session` extractor, so the store is read once per request.
                req.extensions_mut().insert(session);
            }
            next.run(req).await
        }
        Err(e) => e.into_response(),
    }
}

/// The Durable Object class for strongly consistent sessions: one object per session.
/// Use it with `[do.SESSIONS] class_name = "SessionStore"` and `doko::app!(App, objects: [SessionStore])`.
pub struct SessionStore(State);

#[derive(Deserialize)]
struct Ttl {
    ttl_secs: u64,
}

const DATA_KEY: &str = "data";

impl DurableObject for SessionStore {
    fn new(state: State, _: Env) -> Self {
        Self(state)
    }

    async fn fetch(&self, req: Request) -> Result<Response<Body>> {
        let storage = self.0.storage();
        match *req.method() {
            Method::GET => storage.get::<String>(DATA_KEY).await?.map(IntoResponse::into_response).ok_or_else(Error::not_found),
            Method::PUT => {
                let Query(ttl) = Query::<Ttl>::try_from_uri(req.uri())?;
                let data = String::from_utf8(to_bytes(req.into_body(), usize::MAX).await?.to_vec())?;
                storage.put(DATA_KEY, data).await?;
                // The alarm deletes the session when it expires.
                storage.set_alarm(Duration::from_secs(ttl.ttl_secs)).await?;
                Ok(().into_response())
            }
            Method::DELETE => Ok(storage.delete_all().await?.into_response()),
            _ => Err(Error::new(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")),
        }
    }

    async fn alarm(&self) -> Result<()> {
        Ok(self.0.storage().delete_all().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_the_session_cookie() {
        let mut h = HeaderMap::new();
        assert_eq!(session_cookie(&h), None);
        h.append(COOKIE, HeaderValue::from_static("a=1; doko_session_x=no"));
        h.append(COOKIE, HeaderValue::from_static("b=2;  doko_session=id.sig ; c=3"));
        assert_eq!(session_cookie(&h), Some("id.sig"));
    }

    #[test]
    fn parses_password_hashes() {
        let (iterations, salt, key) = parse_password_hash("pbkdf2-sha256$1000$c2FsdA==$a2V5").unwrap();
        assert_eq!((iterations, salt.as_slice(), key.as_slice()), (1000, &b"salt"[..], &b"key"[..]));
        for bad in ["", "plaintext", "md5$1000$c2FsdA==$a2V5", "pbkdf2-sha256$0$c2FsdA==$a2V5", "pbkdf2-sha256$100001$c2FsdA==$a2V5", "pbkdf2-sha256$1000$c2FsdA==$a2V5$x", "pbkdf2-sha256$1000$!$a2V5"] {
            assert!(parse_password_hash(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn parses_jwts_and_checks_their_times() {
        // {"alg":"HS256","typ":"JWT"} . {"sub":"1"} . "sig"
        let (input, alg, claims, signature) = parse_jwt("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxIn0.c2ln").unwrap();
        assert_eq!(input, "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxIn0");
        assert_eq!((alg.as_str(), &claims, signature.as_slice()), ("HS256", &json!({ "sub": "1" }), &b"sig"[..]));
        assert!(parse_jwt("a.b").is_none());
        assert!(parse_jwt("not.a.jwt").is_none());

        assert!(in_validity_window(&json!({}), 100));
        assert!(in_validity_window(&json!({ "exp": 101, "nbf": 100 }), 100));
        assert!(!in_validity_window(&json!({ "exp": 100 }), 100), "exp is exclusive");
        assert!(!in_validity_window(&json!({ "nbf": 101 }), 100));
        assert!(!in_validity_window(&json!({ "exp": "tomorrow" }), 100), "a malformed exp is not ignored");
    }

    #[test]
    fn reads_pem_keys() {
        assert_eq!(pem_to_der("-----BEGIN PUBLIC KEY-----\nc2Fs\ndA==\n-----END PUBLIC KEY-----\n").unwrap(), b"salt");
        assert_eq!(pem_to_der("-----BEGIN PUBLIC KEY----- c2FsdA== -----END PUBLIC KEY-----").unwrap(), b"salt");
        assert!(pem_to_der("c2FsdA==").is_none());
    }
}
