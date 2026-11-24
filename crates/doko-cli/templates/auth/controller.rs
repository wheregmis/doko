//! Accounts: register, verify the email address, log in and out, reset a forgotten password.
//! Written by `doko generate auth`. It is your code now: change it freely.

use doko::{
    Ctx, Error, Errors, RateLimit, Result, RouterExt, Valid, Validate,
    auth::{self, Session, SetCookie},
    axum::{
        Json, Router,
        extract::Query,
        http::StatusCode,
        routing::{get, post},
    },
};
use serde::{Deserialize, Serialize};

use crate::bindings::{__DB__, __EMAIL__, __RATE_LIMITER__};
use crate::mailers;

/// Token purposes: a token made for one is refused for the other.
const VERIFY: &str = "verify-email";
const RESET: &str = "reset-password";
const VERIFY_TTL_SECS: u64 = 3 * 24 * 60 * 60;
const RESET_TTL_SECS: u64 = 60 * 60;

/// What the session carries and what `/auth/me` returns. Other controllers read it with
/// `session.get::<User>()` behind `.require_auth()`.
#[derive(Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub email_verified: bool,
}

#[derive(Deserialize)]
struct UserRow {
    id: i64,
    email: String,
    password_hash: String,
    password_version: i64,
    email_verified_at: Option<String>,
}

impl From<UserRow> for User {
    fn from(row: UserRow) -> Self {
        Self { id: row.id, email: row.email, email_verified: row.email_verified_at.is_some() }
    }
}

#[derive(Deserialize)]
struct Credentials {
    email: String,
    password: String,
}

impl Validate for Credentials {
    fn validate(&self, errors: &mut Errors) {
        let email = self.email.trim();
        let plausible = email.contains('@') && !email.contains(char::is_whitespace) && email.len() <= 254;
        errors.check("email", plausible, "must be an email address");
        check_password(&self.password, errors);
    }
}

fn check_password(password: &str, errors: &mut Errors) {
    errors.check("password", password.len() >= 8, "must be at least 8 characters");
    errors.check("password", password.len() <= 1024, "must be at most 1024 characters");
}

#[derive(Deserialize)]
struct Email {
    email: String,
}

#[derive(Deserialize)]
struct Token {
    token: String,
}

#[derive(Deserialize)]
struct NewPassword {
    token: String,
    password: String,
}

impl Validate for NewPassword {
    fn validate(&self, errors: &mut Errors) {
        check_password(&self.password, errors);
    }
}

/// What a reset token carries. `version` is the user's `password_version` when it was issued:
/// a reset bumps the version, so each link works once.
#[derive(Serialize, Deserialize)]
struct Reset {
    id: i64,
    version: i64,
}

#[derive(Deserialize)]
struct Id {
    id: i64,
}

pub fn routes() -> Router<Ctx> {
    // Guessing passwords and flooding inboxes are the attacks here: 10 requests a minute per client IP.
    let limited = Router::new()
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .route("/auth/forgot-password", post(forgot_password))
        .route("/auth/reset-password", post(reset_password))
        .rate_limit(RateLimit::new(__RATE_LIMITER__, "auth", 10, 60));
    Router::new()
        .route("/auth/me", get(me))
        .require_auth()
        .route("/auth/verify", get(verify))
        .route("/auth/logout", post(logout))
        .merge(limited)
}

fn normalize(email: &str) -> String {
    email.trim().to_lowercase()
}

async fn find(ctx: &Ctx, email: &str) -> Result<Option<UserRow>> {
    ctx.db(__DB__)?.first("SELECT * FROM users WHERE email = ?1", (email,)).await
}

/// A link into this app, or into the frontend that sits in front of it: `APP_URL` is set under
/// `[wrangler.vars]` in doko.toml. It is not taken from the request, whose Host a client chooses.
fn link(ctx: &Ctx, path: &str, token: &str) -> Result<String> {
    let base = ctx.var("APP_URL").ok_or_else(|| Error::internal("set APP_URL under [wrangler.vars] in doko.toml"))?;
    Ok(format!("{}{path}?token={token}", base.trim_end_matches('/')))
}

fn expired() -> Error {
    Error::bad_request("this link is invalid or has expired")
}

async fn register(ctx: Ctx, Valid(c): Valid<Credentials>) -> Result<StatusCode> {
    let email = normalize(&c.email);
    let hash = auth::hash_password(&c.password).await?;
    let sql = "INSERT OR IGNORE INTO users (email, password_hash) VALUES (?1, ?2) RETURNING id";
    // The answer is the same whether or not the address was free, so accounts can't be probed.
    // Someone who registers twice gets no second mail; "forgot password" gets them back in.
    if let Some(new) = ctx.db(__DB__)?.first::<Id>(sql, (&email, &hash)).await? {
        let token = auth::token(&ctx, VERIFY, &new.id, VERIFY_TTL_SECS).await?;
        ctx.mail(__EMAIL__)?.send(&mailers::auth::verify(&email, &link(&ctx, "/auth/verify", &token)?)?).await?;
    }
    Ok(StatusCode::CREATED)
}

/// The link in the verification mail. To require a verified address, check `email_verified` in `login`.
async fn verify(ctx: Ctx, Query(q): Query<Token>) -> Result<&'static str> {
    let id: i64 = auth::read_token(&ctx, VERIFY, &q.token).await?.ok_or_else(expired)?;
    let sql = "UPDATE users SET email_verified_at = COALESCE(email_verified_at, datetime('now')) WHERE id = ?1";
    ctx.db(__DB__)?.run(sql, (id,)).await?;
    Ok("Your email address is verified.")
}

async fn login(ctx: Ctx, session: Session, Json(c): Json<Credentials>) -> Result<(SetCookie, Json<User>)> {
    let Some(row) = find(&ctx, &normalize(&c.email)).await? else {
        // Spend the time of a real check, so response times don't show which addresses have accounts.
        auth::hash_password(&c.password).await?;
        return Err(Error::unauthorized());
    };
    if !auth::verify_password(&c.password, &row.password_hash).await? {
        return Err(Error::unauthorized());
    }
    let user = User::from(row);
    Ok((session.set(&user).await?, Json(user)))
}

async fn logout(session: Session) -> Result<SetCookie> {
    session.clear().await
}

/// Read from the database, not the session, so a just-verified address shows up at once.
async fn me(ctx: Ctx, session: Session) -> Result<Json<User>> {
    let user: User = session.get()?.ok_or_else(Error::unauthorized)?;
    let row: Option<UserRow> = ctx.db(__DB__)?.first("SELECT * FROM users WHERE id = ?1", (user.id,)).await?;
    row.map(|row| Json(row.into())).ok_or_else(Error::unauthorized)
}

/// Always 202, so this can't be used to find out who has an account.
async fn forgot_password(ctx: Ctx, Json(body): Json<Email>) -> Result<StatusCode> {
    let email = normalize(&body.email);
    if let Some(row) = find(&ctx, &email).await? {
        let reset = Reset { id: row.id, version: row.password_version };
        let token = auth::token(&ctx, RESET, &reset, RESET_TTL_SECS).await?;
        // This page is your frontend's: a form that POSTs the token and the new password to /auth/reset-password.
        ctx.mail(__EMAIL__)?.send(&mailers::auth::reset(&email, &link(&ctx, "/reset-password", &token)?)?).await?;
    }
    Ok(StatusCode::ACCEPTED)
}

/// Sessions that are already open stay open: they live in the session store under random ids.
async fn reset_password(ctx: Ctx, Valid(new): Valid<NewPassword>) -> Result<StatusCode> {
    let reset: Reset = auth::read_token(&ctx, RESET, &new.token).await?.ok_or_else(expired)?;
    let hash = auth::hash_password(&new.password).await?;
    // Getting the mail proves the address too. The version check makes the link single-use.
    let sql = "UPDATE users SET password_hash = ?1, password_version = password_version + 1, \
               email_verified_at = COALESCE(email_verified_at, datetime('now')) \
               WHERE id = ?2 AND password_version = ?3";
    if ctx.db(__DB__)?.run(sql, (&hash, reset.id, reset.version)).await?.changes == Some(0) {
        return Err(expired());
    }
    Ok(StatusCode::NO_CONTENT)
}
