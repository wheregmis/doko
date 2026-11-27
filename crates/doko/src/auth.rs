//! Sessions, JWTs and password hashing, on the runtime's WebCrypto.
//!
//! Everything is wired by name, so there is nothing to configure in code:
//!
//! | Feature | Needs |
//! |---|---|
//! | [`Session`] | secret `SESSION_SECRET`, and `[kv.SESSIONS]` or `[do.SESSIONS] class_name = "SessionStore"` |
//! | [`Claims`] and [`sign`] (HS256) | secret `JWT_SECRET` |
//!
//! | [`Claims`] for RS256 tokens | secret `JWT_PUBLIC_KEY` (the issuer's public key as PEM), or `jwks_url` below |
//!
//! [`RouterExt::require_auth`](crate::RouterExt::require_auth) turns away requests with neither.
//!
//! The optional `[auth]` table in doko.toml (and `[env.<name>.auth]`) tunes it:
//!
//! ```toml
//! [auth]
//! session_ttl_secs = 86400            # default: 7 days
//! session_cookie = "sid"              # default: doko_session
//! jwt_issuer = "https://id.example"   # tokens must carry this `iss`; `sign` adds it
//! jwt_audience = "my-api"             # tokens must carry this `aud`; `sign` adds it
//! jwks_url = "https://id.example/.well-known/jwks.json"   # RS256 keys, picked by `kid`
//! ```

use std::{cell::RefCell, time::Duration};

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
    js_sys::{Array, Function, JSON, Object, Promise, Reflect, Uint8Array},
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
/// Var holding `[auth]` from doko.toml, merged for the active environment.
const AUTH_VAR: &str = "DOKO_AUTH";
/// How long a JWKS is trusted before it is fetched again.
const JWKS_MAX_AGE_MS: u64 = 60 * 60 * 1000;
/// The soonest an unknown `kid` may trigger another fetch, so bad tokens can't hammer the issuer.
const JWKS_MIN_REFETCH_MS: u64 = 60 * 1000;

/// `[auth]` from doko.toml. `doko config` has already validated it.
#[derive(Deserialize, Default)]
struct Config {
    session_ttl_secs: Option<u64>,
    session_cookie: Option<String>,
    jwt_issuer: Option<String>,
    jwt_audience: Option<String>,
    jwks_url: Option<String>,
}

impl Config {
    fn load(ctx: &Ctx) -> Self {
        // A wrangler.toml generated before `[auth]` existed has no such var: defaults apply.
        ctx.env().object_var(AUTH_VAR).unwrap_or_default()
    }

    fn session_ttl(&self) -> u64 {
        self.session_ttl_secs.unwrap_or(7 * 24 * 60 * 60)
    }

    fn session_cookie(&self) -> &str {
        self.session_cookie.as_deref().unwrap_or("doko_session")
    }
}
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

/// `format` is WebCrypto's: `"spki"` with DER bytes, or `"jwk"` with a JWK object.
async fn rsa_verify(format: &str, public_key: JsValue, data: &[u8], signature: &[u8]) -> Js<bool> {
    const NAME: &str = "RSASSA-PKCS1-v1_5";
    let alg = obj(&[("name", NAME.into()), ("hash", "SHA-256".into())]);
    let usages = Array::of1(&"verify".into()).into();
    let key = subtle("importKey", &[format.into(), public_key, alg, false.into(), usages]).await?;
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
/// `claims` must serialize to a JSON object; `iat` and `exp` are added to it, and so are `iss` and
/// `aud` when `[auth]` sets `jwt_issuer` / `jwt_audience` and the claims don't.
pub async fn sign<T: Serialize>(ctx: &Ctx, claims: &T, ttl_secs: u64) -> Result<String> {
    let Value::Object(mut claims) = serde_json::to_value(claims)? else {
        return Err(Error::internal("JWT claims must serialize to a JSON object"));
    };
    let now = Date::now().as_millis() / 1000;
    claims.insert("iat".into(), now.into());
    claims.insert("exp".into(), now.saturating_add(ttl_secs).into());
    let config = Config::load(ctx);
    for (name, value) in [("iss", config.jwt_issuer), ("aud", config.jwt_audience)] {
        if let Some(value) = value {
            claims.entry(name).or_insert(value.into());
        }
    }
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
/// HS256 tokens are checked against the `JWT_SECRET` secret. RS256 tokens are checked against the
/// key their `kid` names at `[auth] jwks_url`, or against `JWT_PUBLIC_KEY` when no URL is set.
/// Expired tokens (`exp`), tokens not yet valid (`nbf`), and tokens whose `iss` / `aud` don't
/// match `[auth] jwt_issuer` / `jwt_audience` are rejected.
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

/// A token split into (signed part, header, claims, signature), if it is well formed.
fn parse_jwt(token: &str) -> Option<(&str, Value, Value, Vec<u8>)> {
    let (input, signature) = token.rsplit_once('.')?;
    let (header, claims) = input.split_once('.')?;
    let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header).ok()?).ok()?;
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).ok()?).ok()?;
    Some((input, header, claims, URL_SAFE_NO_PAD.decode(signature).ok()?))
}

/// Whether the claims' `exp` and `nbf` allow the token at `now_secs`. A token without `exp` never expires.
fn in_validity_window(claims: &Value, now_secs: u64) -> bool {
    let time = |name: &str| claims.get(name).map(|v| v.as_f64());
    // A present but non-numeric time is invalid, not ignored.
    !matches!(time("exp"), Some(exp) if exp.is_none_or(|exp| now_secs as f64 >= exp))
        && !matches!(time("nbf"), Some(nbf) if nbf.is_none_or(|nbf| (now_secs as f64) < nbf))
}

/// Whether the claims carry the configured issuer and audience. `aud` may be one string or a list.
fn issued_for(claims: &Value, config: &Config) -> bool {
    let iss = config.jwt_issuer.as_deref().is_none_or(|iss| claims["iss"] == iss);
    let aud = config.jwt_audience.as_deref().is_none_or(|aud| match &claims["aud"] {
        Value::Array(list) => list.iter().any(|a| a == aud),
        one => one == aud,
    });
    iss && aud
}

/// The RSA key called `kid` in a JWKS, as JSON. A token without a `kid` gets the first RSA key.
fn find_jwk(keys: &[Value], kid: Option<&str>) -> Option<String> {
    keys.iter().find(|k| k["kty"] == "RSA" && kid.is_none_or(|kid| k["kid"] == kid)).map(Value::to_string)
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Value>,
}

/// The issuer's key for `kid`, from the JWKS at `url`. The key set is kept in the isolate for an
/// hour; a `kid` it doesn't have refetches it (at most once a minute), which picks up rotated keys.
async fn jwk(url: &str, kid: Option<&str>) -> Result<Option<String>> {
    thread_local! {
        static JWKS: RefCell<Option<(u64, Vec<Value>)>> = const { RefCell::new(None) };
    }
    let now = Date::now().as_millis();
    let cached = JWKS.with_borrow(|c| c.as_ref().map(|(at, keys)| (now.saturating_sub(*at), find_jwk(keys, kid))));
    if let Some((age, found)) = cached
        && age < JWKS_MAX_AGE_MS
        && (found.is_some() || age < JWKS_MIN_REFETCH_MS)
    {
        return Ok(found);
    }
    let url = url.parse().map_err(|e| Error::internal(format!("[auth] jwks_url: {e}")))?;
    let fetched: worker::Result<Jwks> =
        SendFuture::new(async { worker::Fetch::Url(url).send().await?.json().await }).await;
    let keys = fetched.map_err(|e| Error::internal(format!("fetching [auth] jwks_url: {e}")))?.keys;
    let found = find_jwk(&keys, kid);
    JWKS.set(Some((now, keys)));
    Ok(found)
}

fn pem_to_der(pem: &str) -> Option<Vec<u8>> {
    let body: String = pem.split("-----").nth(2)?.split_whitespace().collect();
    STANDARD.decode(body).ok()
}

/// What checks a token's signature.
#[derive(Debug, PartialEq)]
enum Key<'a> {
    Hmac(String),
    Jwks(&'a str),
    Pem(String),
    /// The app takes no tokens of this algorithm: a 401, not a missing-secret 500 anyone could cause.
    None,
}

/// Each algorithm has its own key, so a token can't pick HS256 to be checked against the public key.
fn key_for<'a>(alg: Option<&str>, jwks_url: Option<&'a str>, secret: impl Fn(&str) -> Option<String>) -> Key<'a> {
    match (alg, jwks_url) {
        (Some("HS256"), _) => secret(JWT_SECRET).map_or(Key::None, Key::Hmac),
        (Some("RS256"), Some(url)) => Key::Jwks(url),
        (Some("RS256"), None) => secret(JWT_PUBLIC_KEY).map_or(Key::None, Key::Pem),
        _ => Key::None,
    }
}

async fn verify_jwt(ctx: &Ctx, token: &str) -> Result<Value> {
    let (input, header, claims, signature) = parse_jwt(token).ok_or_else(Error::unauthorized)?;
    let config = Config::load(ctx);
    let secret = |name: &str| ctx.env().secret(name).ok().map(|s| s.to_string());
    let valid = match key_for(header["alg"].as_str(), config.jwks_url.as_deref(), secret) {
        Key::Hmac(secret) => {
            SendFuture::new(hmac_verify(secret.as_bytes(), input.as_bytes(), &signature)).await.map_err(js_err)?
        }
        Key::Jwks(url) => match jwk(url, header["kid"].as_str()).await? {
            Some(key) => {
                SendFuture::new(async { rsa_verify("jwk", JSON::parse(&key)?, input.as_bytes(), &signature).await })
                    .await
                    .map_err(js_err)?
            }
            None => false,
        },
        Key::Pem(pem) => {
            let der = pem_to_der(&pem)
                .ok_or_else(|| Error::internal(format!("`{JWT_PUBLIC_KEY}` is not a PEM public key")))?;
            SendFuture::new(async { rsa_verify("spki", bytes(&der), input.as_bytes(), &signature).await })
                .await
                .map_err(js_err)?
        }
        Key::None => false,
    };
    let now = Date::now().as_millis() / 1000;
    if valid && in_validity_window(&claims, now) && issued_for(&claims, &config) {
        Ok(claims)
    } else {
        Err(Error::unauthorized())
    }
}

// --- Signed tokens ---

/// Kept out of the signed text of session cookies and JWTs, so neither can pass as a token.
const TOKEN_CONTEXT: &str = "doko-token:";

#[derive(Serialize, Deserialize)]
struct TokenBody {
    purpose: String,
    exp: u64,
    data: Value,
}

/// A tamper-proof token carrying `data`, valid for `ttl_secs` and only for `purpose`: for the
/// links in "verify your email" and "reset your password" mail. Signed with `SESSION_SECRET`.
///
/// ```ignore
/// let token = auth::token(&ctx, "reset-password", &user.id, 3600).await?;
/// let id: Option<i64> = auth::read_token(&ctx, "reset-password", &token).await?;
/// ```
///
/// `data` is readable by whoever holds the token, so put ids in it, not secrets. A token works
/// until it expires: to make one single-use, include something the action changes (see `generate auth`).
pub async fn token<T: Serialize>(ctx: &Ctx, purpose: &str, data: &T, ttl_secs: u64) -> Result<String> {
    let exp = (Date::now().as_millis() / 1000).saturating_add(ttl_secs);
    let body = TokenBody { purpose: purpose.to_owned(), exp, data: serde_json::to_value(data)? };
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&body)?);
    let secret = ctx.secret(SESSION_SECRET)?;
    let signed = format!("{TOKEN_CONTEXT}{body}");
    let signature = SendFuture::new(hmac_sign(secret.as_bytes(), signed.as_bytes())).await.map_err(js_err)?;
    Ok(format!("{body}.{}", URL_SAFE_NO_PAD.encode(signature)))
}

/// The data of a [`token`] made for `purpose`, or `None` if it is forged, expired, or for another purpose.
pub async fn read_token<T: DeserializeOwned>(ctx: &Ctx, purpose: &str, token: &str) -> Result<Option<T>> {
    let Some((body, signature)) = token.split_once('.') else { return Ok(None) };
    let Ok(signature) = URL_SAFE_NO_PAD.decode(signature) else { return Ok(None) };
    let secret = ctx.secret(SESSION_SECRET)?;
    let signed = format!("{TOKEN_CONTEXT}{body}");
    if !SendFuture::new(hmac_verify(secret.as_bytes(), signed.as_bytes(), &signature)).await.map_err(js_err)? {
        return Ok(None);
    }
    let body = URL_SAFE_NO_PAD.decode(body).ok().and_then(|b| serde_json::from_slice::<TokenBody>(&b).ok());
    Ok(body
        .filter(|b| token_is_live(b, purpose, Date::now().as_millis() / 1000))
        .and_then(|b| serde_json::from_value(b.data).ok()))
}

fn token_is_live(body: &TokenBody, purpose: &str, now_secs: u64) -> bool {
    body.purpose == purpose && now_secs < body.exp
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
/// Sessions last 7 days, or `[auth] session_ttl_secs`; the cookie is `doko_session`, or `[auth] session_cookie`.
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

fn set_cookie(name: &str, value: &str, max_age: u64) -> Result<SetCookie> {
    // Lax keeps the cookie off cross-site POSTs, which is the CSRF defence for cookie sessions.
    let cookie = format!("{name}={value}; Path=/; Max-Age={max_age}; HttpOnly; Secure; SameSite=Lax");
    Ok(SetCookie(HeaderValue::from_str(&cookie)?))
}

fn session_cookie<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(name)?.strip_prefix('='))
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

    async fn object(objects: &Durable, id: &str, method: Method, ttl_secs: u64, body: Body) -> Result<Response<Body>> {
        let uri = format!("https://doko.do/?ttl_secs={ttl_secs}");
        objects.get(id)?.fetch(Request::builder().method(method).uri(uri).body(body)?).await
    }

    async fn get(&self, id: &str) -> Result<Option<Value>> {
        match self {
            Self::Kv(kv) => kv.get(&kv_key(id)).await,
            Self::Durable(objects) => {
                let res = Self::object(objects, id, Method::GET, 0, Body::empty()).await?;
                match res.status() {
                    StatusCode::OK => Ok(Some(serde_json::from_slice(&to_bytes(res.into_body(), usize::MAX).await?)?)),
                    StatusCode::NOT_FOUND => Ok(None),
                    status => Err(Error::internal(format!("session store answered {status}"))),
                }
            }
        }
    }

    async fn put(&self, id: &str, data: &Value, ttl_secs: u64) -> Result<()> {
        match self {
            Self::Kv(kv) => kv.put_ttl(&kv_key(id), data, ttl_secs).await,
            Self::Durable(objects) => {
                Self::ok(Self::object(objects, id, Method::PUT, ttl_secs, Body::from(serde_json::to_vec(data)?)).await?)
            }
        }
    }

    async fn delete(&self, id: &str) -> Result<()> {
        match self {
            Self::Kv(kv) => kv.delete(&kv_key(id)).await,
            Self::Durable(objects) => Self::ok(Self::object(objects, id, Method::DELETE, 0, Body::empty()).await?),
        }
    }

    fn ok(res: Response<Body>) -> Result<()> {
        if res.status().is_success() {
            Ok(())
        } else {
            Err(Error::internal(format!("session store answered {}", res.status())))
        }
    }
}

fn kv_key(id: &str) -> String {
    format!("doko:session:{id}")
}

impl Session {
    /// The session named by a correctly signed cookie in `headers`, or an empty one.
    async fn load(ctx: &Ctx, headers: &HeaderMap) -> Result<Self> {
        let cookie = session_cookie(headers, Config::load(ctx).session_cookie()).map(str::to_owned);
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
        let config = Config::load(&self.ctx);
        store.put(&id, &serde_json::to_value(value)?, config.session_ttl()).await?;
        if let Some(old) = &self.id {
            store.delete(old).await?;
        }
        set_cookie(
            config.session_cookie(),
            &format!("{id}.{}", URL_SAFE_NO_PAD.encode(signature)),
            config.session_ttl(),
        )
    }

    /// Ends the session: deletes its data and expires the cookie.
    pub async fn clear(&self) -> Result<SetCookie> {
        if let Some(id) = &self.id {
            Store::open(&self.ctx)?.delete(id).await?;
        }
        set_cookie(Config::load(&self.ctx).session_cookie(), "", 0)
    }
}

impl FromRequestParts<Ctx> for Session {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, ctx: &Ctx) -> Result<Self> {
        // `require_auth` has already loaded it for the routes it guards.
        if let Some(session) = parts.extensions.remove::<Session>() {
            return Ok(session);
        }
        // Owned, so the future doesn't borrow `parts` across an await.
        let headers = parts.headers.clone();
        Session::load(ctx, &headers).await
    }
}

/// Whether the claims or session value names `role`: `"role": "admin"` or `"roles": ["admin", ..]`.
fn has_role(who: &Value, role: &str) -> bool {
    who["role"] == role || who["roles"].as_array().is_some_and(|roles| roles.iter().any(|r| r == role))
}

/// The middleware behind `RouterExt::require_auth` and, with a `role`, `require_role`.
pub(crate) async fn require(role: Option<&'static str>, mut req: Request, next: Next) -> Response<Body> {
    let token = bearer(req.headers()).map(str::to_owned);
    let headers = req.headers().clone();
    let ctx = req.extensions().get::<Ctx>().cloned();
    let checked = async {
        let ctx = ctx.ok_or_else(|| Error::internal("require_auth: request has no Ctx"))?;
        if let Some(token) = token {
            return verify_jwt(&ctx, &token).await.map(|claims| (claims, None));
        }
        let session = Session::load(&ctx, &headers).await?;
        match session.data.clone() {
            Some(data) => Ok((data, Some(session))),
            None => Err(Error::unauthorized()),
        }
    };
    match checked.await {
        Ok((who, _)) if role.is_some_and(|role| !has_role(&who, role)) => Error::forbidden().into_response(),
        Ok((_, session)) => {
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
            Method::GET => {
                storage.get::<String>(DATA_KEY).await?.map(IntoResponse::into_response).ok_or_else(Error::not_found)
            }
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
    fn a_token_for_a_key_the_app_lacks_is_refused_not_an_error() {
        let none = |_: &str| None;
        let both = |name: &str| Some(format!("{name}-value"));
        // An RS256-only app (jwks_url, no JWT_SECRET) gets a self-made HS256 token: a 401, not a 500.
        assert_eq!(key_for(Some("HS256"), Some("https://id.test/certs"), none), Key::None);
        assert_eq!(key_for(Some("RS256"), None, none), Key::None);
        assert_eq!(key_for(Some("HS256"), None, both), Key::Hmac("JWT_SECRET-value".into()));
        assert_eq!(key_for(Some("RS256"), None, both), Key::Pem("JWT_PUBLIC_KEY-value".into()));
        assert_eq!(key_for(Some("RS256"), Some("https://id.test/certs"), both), Key::Jwks("https://id.test/certs"));
        for alg in [None, Some("none"), Some("ES256"), Some("hs256")] {
            assert_eq!(key_for(alg, Some("https://id.test/certs"), both), Key::None, "{alg:?}");
        }
    }

    #[test]
    fn finds_the_session_cookie() {
        let mut h = HeaderMap::new();
        assert_eq!(session_cookie(&h, "doko_session"), None);
        h.append(COOKIE, HeaderValue::from_static("a=1; doko_session_x=no"));
        h.append(COOKIE, HeaderValue::from_static("b=2;  doko_session=id.sig ; c=3"));
        assert_eq!(session_cookie(&h, "doko_session"), Some("id.sig"));
        assert_eq!(session_cookie(&h, "b"), Some("2"), "the cookie name is configurable");
    }

    #[test]
    fn parses_password_hashes() {
        let (iterations, salt, key) = parse_password_hash("pbkdf2-sha256$1000$c2FsdA==$a2V5").unwrap();
        assert_eq!((iterations, salt.as_slice(), key.as_slice()), (1000, &b"salt"[..], &b"key"[..]));
        for bad in [
            "",
            "plaintext",
            "md5$1000$c2FsdA==$a2V5",
            "pbkdf2-sha256$0$c2FsdA==$a2V5",
            "pbkdf2-sha256$100001$c2FsdA==$a2V5",
            "pbkdf2-sha256$1000$c2FsdA==$a2V5$x",
            "pbkdf2-sha256$1000$!$a2V5",
        ] {
            assert!(parse_password_hash(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn parses_jwts_and_checks_their_times() {
        // {"alg":"HS256","typ":"JWT"} . {"sub":"1"} . "sig"
        let (input, header, claims, signature) =
            parse_jwt("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxIn0.c2ln").unwrap();
        assert_eq!(input, "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxIn0");
        assert_eq!(
            (header["alg"].as_str(), &claims, signature.as_slice()),
            (Some("HS256"), &json!({ "sub": "1" }), &b"sig"[..])
        );
        assert!(parse_jwt("a.b").is_none());
        assert!(parse_jwt("not.a.jwt").is_none());

        assert!(in_validity_window(&json!({}), 100));
        assert!(in_validity_window(&json!({ "exp": 101, "nbf": 100 }), 100));
        assert!(!in_validity_window(&json!({ "exp": 100 }), 100), "exp is exclusive");
        assert!(!in_validity_window(&json!({ "nbf": 101 }), 100));
        assert!(!in_validity_window(&json!({ "exp": "tomorrow" }), 100), "a malformed exp is not ignored");
    }

    #[test]
    fn finds_roles() {
        assert!(has_role(&json!({ "role": "admin" }), "admin"));
        assert!(has_role(&json!({ "roles": ["editor", "admin"] }), "admin"));
        assert!(!has_role(&json!({ "roles": ["editor"] }), "admin"));
        assert!(!has_role(&json!({ "role": "administrator", "roles": "admin" }), "admin"));
        assert!(!has_role(&json!({}), "admin"));
    }

    #[test]
    fn tokens_expire_and_keep_to_their_purpose() {
        let body = TokenBody { purpose: "reset".into(), exp: 100, data: Value::Null };
        assert!(token_is_live(&body, "reset", 99));
        assert!(!token_is_live(&body, "reset", 100), "expired");
        assert!(!token_is_live(&body, "verify", 99), "another purpose");
    }

    #[test]
    fn checks_issuer_and_audience() {
        let config = |iss: Option<&str>, aud: Option<&str>| Config {
            jwt_issuer: iss.map(Into::into),
            jwt_audience: aud.map(Into::into),
            ..Default::default()
        };
        assert!(issued_for(&json!({}), &config(None, None)), "nothing configured, nothing checked");
        let both = config(Some("https://id.example"), Some("api"));
        assert!(issued_for(&json!({ "iss": "https://id.example", "aud": "api" }), &both));
        assert!(issued_for(&json!({ "iss": "https://id.example", "aud": ["web", "api"] }), &both));
        assert!(!issued_for(&json!({ "iss": "https://evil.example", "aud": "api" }), &both));
        assert!(!issued_for(&json!({ "iss": "https://id.example", "aud": ["web"] }), &both));
        assert!(!issued_for(&json!({ "aud": "api" }), &both), "a missing claim does not pass");
    }

    #[test]
    fn picks_jwks_keys_by_kid() {
        let keys = [
            json!({ "kty": "EC", "kid": "a" }),
            json!({ "kty": "RSA", "kid": "b" }),
            json!({ "kty": "RSA", "kid": "c" }),
        ];
        assert!(find_jwk(&keys, Some("c")).unwrap().contains(r#""kid":"c""#));
        assert!(find_jwk(&keys, None).unwrap().contains(r#""kid":"b""#), "no kid: the first RSA key");
        assert!(find_jwk(&keys, Some("a")).is_none(), "not an RSA key");
        assert!(find_jwk(&keys, Some("z")).is_none());
    }

    #[test]
    fn reads_pem_keys() {
        assert_eq!(pem_to_der("-----BEGIN PUBLIC KEY-----\nc2Fs\ndA==\n-----END PUBLIC KEY-----\n").unwrap(), b"salt");
        assert_eq!(pem_to_der("-----BEGIN PUBLIC KEY----- c2FsdA== -----END PUBLIC KEY-----").unwrap(), b"salt");
        assert!(pem_to_der("c2FsdA==").is_none());
    }
}
