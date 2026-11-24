//! What doko puts around every request: validated JSON bodies, `[middleware]` from doko.toml
//! (secure headers, CORS, the body limit), `/_health`, and `Hooks::on_error`.

use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{FromRequest, FromRequestParts, Query, Request},
    http::{
        HeaderName, HeaderValue, Method, StatusCode,
        header::{
            ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
            ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_MAX_AGE, ACCESS_CONTROL_REQUEST_HEADERS,
            ACCESS_CONTROL_REQUEST_METHOD, ORIGIN, REFERRER_POLICY, VARY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
        },
        request::Parts,
    },
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::{Ctx, Error, Hooks, Result};

/// Var holding `[middleware]` from doko.toml, merged for the active environment.
const MIDDLEWARE_VAR: &str = "DOKO_MIDDLEWARE";
const SECURE_HEADERS: [(HeaderName, &str); 3] = [
    (X_CONTENT_TYPE_OPTIONS, "nosniff"),
    (X_FRAME_OPTIONS, "SAMEORIGIN"),
    (REFERRER_POLICY, "strict-origin-when-cross-origin"),
];
const CORS_METHODS: &str = "GET, POST, PUT, PATCH, DELETE, OPTIONS";

/// The rules a request body must meet, checked by [`Valid`].
///
/// ```ignore
/// impl Validate for NewUser {
///     fn validate(&self, errors: &mut Errors) {
///         errors.check("email", self.email.contains('@'), "must be an email address");
///         errors.check("password", self.password.len() >= 8, "must be at least 8 characters");
///     }
/// }
/// ```
pub trait Validate {
    fn validate(&self, errors: &mut Errors);
}

/// Validation messages by field. An empty one means the value is valid.
#[derive(Default, Debug)]
pub struct Errors(BTreeMap<String, Vec<String>>);

impl Errors {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `message` for `field`.
    pub fn add(&mut self, field: &str, message: impl Into<String>) {
        self.0.entry(field.to_owned()).or_default().push(message.into());
    }

    /// Records `message` for `field` unless `ok`.
    pub fn check(&mut self, field: &str, ok: bool, message: impl Into<String>) {
        if !ok {
            self.add(field, message);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `Ok` when nothing was recorded, else a 422 listing the messages per field. For rules that
    /// need the database: `errors.add("email", "is taken"); errors.finish()?`.
    pub fn finish(self) -> Result<()> {
        if self.0.is_empty() { Ok(()) } else { Err(Error::invalid(self.0)) }
    }
}

/// A JSON body that parsed and passed [`Validate`]: `async fn create(Valid(user): Valid<NewUser>)`.
///
/// A body that isn't JSON for `T` is rejected with the status axum gives it (400, 413, 415 or 422)
/// and one that breaks a rule with a 422, both as `{"error": ..}`.
pub struct Valid<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned + Validate> FromRequest<S> for Valid<T> {
    type Rejection = Error;

    async fn from_request(req: Request, state: &S) -> Result<Self> {
        let Json(value) =
            Json::<T>::from_request(req, state).await.map_err(|e| Error::new(e.status(), e.body_text()))?;
        let mut errors = Errors::new();
        value.validate(&mut errors);
        errors.finish()?;
        Ok(Self(value))
    }
}

const PER_PAGE: i64 = 20;
const MAX_PER_PAGE: i64 = 100;

/// Which page of a list a request asks for: `?page=2&per_page=50`. Extract it in a handler.
///
/// ```ignore
/// async fn index(ctx: Ctx, page: Page) -> Result<Json<Paged<Post>>> {
///     let db = ctx.db(DB)?;
///     let posts = db.all("SELECT * FROM posts ORDER BY id DESC LIMIT ?1 OFFSET ?2", (page.limit(), page.offset())).await?;
///     Ok(Json(page.of(posts, total)))    // total: SELECT COUNT(*) FROM posts
/// }
/// ```
///
/// Pages start at 1. Without the parameters it is page 1 of 20; `per_page` is capped at 100,
/// so a client can't ask for the whole table. Parameters that aren't numbers get a 400.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(from = "PageQuery")]
pub struct Page {
    page: i64,
    per_page: i64,
}

#[derive(Deserialize)]
struct PageQuery {
    page: Option<i64>,
    per_page: Option<i64>,
}

impl From<PageQuery> for Page {
    fn from(q: PageQuery) -> Self {
        Self::new(q.page.unwrap_or(1), q.per_page.unwrap_or(PER_PAGE))
    }
}

impl Page {
    /// Out-of-range values are brought into range instead of refused.
    pub fn new(page: i64, per_page: i64) -> Self {
        Self { page: page.max(1), per_page: per_page.clamp(1, MAX_PER_PAGE) }
    }

    /// For SQL's `LIMIT`.
    pub fn limit(&self) -> i64 {
        self.per_page
    }

    /// For SQL's `OFFSET`.
    pub fn offset(&self) -> i64 {
        (self.page - 1).saturating_mul(self.per_page)
    }

    /// Wraps this page's `items` with what a client needs to show page links. `total` is the
    /// number of rows in the whole list.
    pub fn of<T>(&self, items: Vec<T>, total: i64) -> Paged<T> {
        let total_pages = (total.max(0) + self.per_page - 1) / self.per_page;
        Paged { items, page: self.page, per_page: self.per_page, total, total_pages }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Page {
    type Rejection = Error;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self> {
        let Query(page) =
            Query::<Page>::from_request_parts(parts, state).await.map_err(|e| Error::bad_request(e.body_text()))?;
        Ok(page)
    }
}

/// One page of a list, as JSON: `{"items": [..], "page": 1, "per_page": 20, "total": 41, "total_pages": 3}`.
#[derive(Serialize, Debug)]
pub struct Paged<T> {
    pub items: Vec<T>,
    pub page: i64,
    pub per_page: i64,
    pub total: i64,
    pub total_pages: i64,
}

/// `[middleware]` from doko.toml. `doko config` has already validated it.
#[derive(Deserialize, Default)]
pub(crate) struct Config {
    secure_headers: Option<bool>,
    pub(crate) body_limit_bytes: Option<usize>,
    cors: Option<Cors>,
}

impl Config {
    pub(crate) fn load(ctx: &Ctx) -> Self {
        // A wrangler.toml without `[middleware]` has no such var: defaults apply.
        ctx.env().object_var(MIDDLEWARE_VAR).unwrap_or_default()
    }
}

#[derive(Deserialize)]
struct Cors {
    origins: Vec<String>,
    methods: Option<Vec<String>>,
    headers: Option<Vec<String>>,
    #[serde(default)]
    credentials: bool,
    max_age_secs: Option<u64>,
}

impl Cors {
    fn any_origin(&self) -> bool {
        self.origins.iter().any(|o| o == "*")
    }

    /// The headers that let `origin` read the response; none if it isn't allowed. `preflight` is
    /// the request's `access-control-request-headers`, when it is a preflight.
    fn headers(&self, origin: &str, preflight: Option<Option<&str>>) -> Vec<(HeaderName, String)> {
        let any = self.any_origin();
        if !any && !self.origins.iter().any(|o| o == origin) {
            return Vec::new();
        }
        // `doko config` refuses `*` with credentials, so a credentialed answer always names one origin.
        let mut out = vec![(ACCESS_CONTROL_ALLOW_ORIGIN, if any { "*".to_owned() } else { origin.to_owned() })];
        if self.credentials {
            out.push((ACCESS_CONTROL_ALLOW_CREDENTIALS, "true".to_owned()));
        }
        if let Some(requested) = preflight {
            out.push((
                ACCESS_CONTROL_ALLOW_METHODS,
                self.methods.as_ref().map_or(CORS_METHODS.to_owned(), |m| m.join(", ")),
            ));
            // Without a list, any header the browser asks for is allowed.
            if let Some(headers) = self.headers.as_ref().map(|h| h.join(", ")).or(requested.map(str::to_owned)) {
                out.push((ACCESS_CONTROL_ALLOW_HEADERS, headers));
            }
            if let Some(age) = self.max_age_secs {
                out.push((ACCESS_CONTROL_MAX_AGE, age.to_string()));
            }
        }
        out
    }
}

pub(crate) async fn health() -> Json<Value> {
    Json(json!({ "ok": true }))
}

fn header(req: &Request, name: HeaderName) -> Option<String> {
    req.headers().get(name)?.to_str().ok().map(str::to_owned)
}

/// Applies `[middleware]` and reports server errors to `Hooks::on_error`.
pub(crate) async fn guard<H: Hooks>(req: Request, next: Next) -> Response {
    let ctx = req.extensions().get::<Ctx>().cloned();
    let cfg = ctx.as_ref().map(Config::load).unwrap_or_default();
    let origin = header(&req, ORIGIN);
    let preflight = (req.method() == Method::OPTIONS && req.headers().contains_key(ACCESS_CONTROL_REQUEST_METHOD))
        .then(|| header(&req, ACCESS_CONTROL_REQUEST_HEADERS));

    let mut res = match (&cfg.cors, &preflight) {
        // The browser is asking permission, not making the request: the handler never sees it.
        (Some(_), Some(_)) => StatusCode::NO_CONTENT.into_response(),
        _ => next.run(req).await,
    };

    if let Some(cors) = &cfg.cors {
        let allowed = origin.map(|o| cors.headers(&o, preflight.as_ref().map(|h| h.as_deref()))).unwrap_or_default();
        for (name, value) in allowed {
            if let Ok(value) = HeaderValue::from_str(&value) {
                res.headers_mut().insert(name, value);
            }
        }
        if !cors.any_origin() {
            // The answer depends on who asked, so caches must not share it between origins.
            res.headers_mut().append(VARY, HeaderValue::from_static("origin"));
        }
    }
    if cfg.secure_headers != Some(false) {
        for (name, value) in SECURE_HEADERS {
            if !res.headers().contains_key(&name) {
                res.headers_mut().insert(name, HeaderValue::from_static(value));
            }
        }
    }
    if res.status().is_server_error()
        && let Some(ctx) = ctx
    {
        let status = res.status();
        // A 5xx that isn't a `doko::Error` (a handler returning a bare status) is reported too.
        let error = res
            .extensions_mut()
            .remove::<Error>()
            .unwrap_or_else(|| Error::new(status, "the handler answered with this status"));
        // After the response: reporting must not slow down or break the error page.
        ctx.wait_until(H::on_error(ctx.clone(), error));
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cors(origins: &[&str], credentials: bool) -> Cors {
        Cors {
            origins: origins.iter().map(|o| o.to_string()).collect(),
            methods: None,
            headers: None,
            credentials,
            max_age_secs: Some(600),
        }
    }

    fn get<'a>(headers: &'a [(HeaderName, String)], name: &HeaderName) -> Option<&'a str> {
        headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    #[test]
    fn cors_allows_only_listed_origins() {
        let c = cors(&["https://app.example"], true);
        assert!(c.headers("https://evil.example", None).is_empty());
        let h = c.headers("https://app.example", None);
        assert_eq!(get(&h, &ACCESS_CONTROL_ALLOW_ORIGIN), Some("https://app.example"));
        assert_eq!(get(&h, &ACCESS_CONTROL_ALLOW_CREDENTIALS), Some("true"));
        assert_eq!(get(&h, &ACCESS_CONTROL_ALLOW_METHODS), None, "only on preflights");
        assert_eq!(
            get(&cors(&["*"], false).headers("https://x.example", None), &ACCESS_CONTROL_ALLOW_ORIGIN),
            Some("*")
        );
    }

    #[test]
    fn cors_answers_preflights() {
        let h = cors(&["*"], false).headers("https://x.example", Some(Some("x-api-key")));
        assert_eq!(get(&h, &ACCESS_CONTROL_ALLOW_METHODS), Some(CORS_METHODS));
        assert_eq!(get(&h, &ACCESS_CONTROL_ALLOW_HEADERS), Some("x-api-key"), "mirrors the request");
        assert_eq!(get(&h, &ACCESS_CONTROL_MAX_AGE), Some("600"));
        let mut listed = cors(&["*"], false);
        listed.headers = Some(vec!["content-type".into()]);
        assert_eq!(
            get(&listed.headers("https://x.example", Some(Some("x-api-key"))), &ACCESS_CONTROL_ALLOW_HEADERS),
            Some("content-type")
        );
    }

    #[test]
    fn pages_clamp_and_count() {
        let page = Page::new(3, 10);
        assert_eq!((page.limit(), page.offset()), (10, 20));
        let paged = page.of(vec![1, 2, 3], 23);
        assert_eq!((paged.total, paged.total_pages, paged.page), (23, 3, 3));
        assert_eq!(Page::new(1, 20).of(Vec::<i64>::new(), 0).total_pages, 0);
        assert_eq!(Page::new(1, 20).of(vec![0; 20], 20).total_pages, 1);
        assert_eq!(Page::new(-5, 100_000), Page::new(1, 100), "out of range is clamped");
        assert_eq!(Page::from(PageQuery { page: None, per_page: None }), Page::new(1, 20));
    }

    #[test]
    fn collects_validation_errors() {
        let mut e = Errors::new();
        e.check("name", true, "must not be empty");
        assert!(e.is_empty());
        e.check("email", false, "must be an email address");
        e.add("email", "is taken");
        let err = e.finish().unwrap_err();
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(Errors::new().finish().is_ok());
    }
}
