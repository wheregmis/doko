//! Signup, login and logout. Login starts a cookie session and also returns a JWT for API clients.

use doko::{
    Ctx, Error, RateLimit, Result, RouterExt,
    auth::{self, Claims, Session, SetCookie},
    axum::{
        Json, Router,
        http::StatusCode,
        routing::{get, post},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::bindings::{DB, RATE_LIMITER};

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

/// What the session and the JWT carry. `require_role` reads `role` from it.
#[derive(Serialize, Deserialize)]
struct User {
    id: i64,
    username: String,
    #[serde(default)]
    role: String,
}

#[derive(Deserialize)]
struct UserRow {
    id: i64,
    username: String,
    password_hash: String,
}

pub fn routes() -> Router<Ctx> {
    let public = Router::new()
        .route("/signup", post(signup))
        .route("/login", post(login))
        // Password guessing is the attack here, so these are limited per client IP.
        .rate_limit(RateLimit::new(RATE_LIMITER, "auth", 10, 60))
        .route("/logout", post(logout))
        // `Claims` answers 401 itself when the bearer token is missing or invalid.
        .route("/api/me", get(api_me));
    // 401 without a session or token, 403 unless it carries the role.
    let admin = Router::new().route("/admin", get(|| async { "welcome, admin" })).require_role("admin");
    Router::new().route("/me", get(me)).require_auth().merge(admin).merge(public)
}

async fn signup(ctx: Ctx, Json(c): Json<Credentials>) -> Result<StatusCode> {
    if c.username.trim().is_empty() || c.password.len() < 8 {
        return Err(Error::bad_request("username must not be empty and password needs 8+ characters"));
    }
    let hash = auth::hash_password(&c.password).await?;
    let sql = "INSERT OR IGNORE INTO users (username, password_hash) VALUES (?1, ?2)";
    if ctx.db(DB)?.run(sql, (&c.username, &hash)).await?.changes == Some(0) {
        return Err(Error::new(StatusCode::CONFLICT, "username is taken"));
    }
    Ok(StatusCode::CREATED)
}

async fn login(ctx: Ctx, session: Session, Json(c): Json<Credentials>) -> Result<(SetCookie, Json<Value>)> {
    let row: Option<UserRow> = ctx.db(DB)?.first("SELECT * FROM users WHERE username = ?1", (&c.username,)).await?;
    // The same 401 for an unknown user and a wrong password, so usernames can't be probed.
    let row = row.ok_or_else(Error::unauthorized)?;
    if !auth::verify_password(&c.password, &row.password_hash).await? {
        return Err(Error::unauthorized());
    }
    // A real app stores roles with the user; here the name decides.
    let role = if row.username == "admin" { "admin" } else { "member" }.to_owned();
    let user = User { id: row.id, username: row.username, role };
    let token = auth::sign(&ctx, &user, 60 * 60).await?;
    Ok((session.set(&user).await?, Json(json!({ "token": token }))))
}

async fn logout(session: Session) -> Result<SetCookie> {
    session.clear().await
}

/// Behind `require_auth`. A request let in by a JWT has no session, hence the 401 fallback.
async fn me(session: Session) -> Result<Json<User>> {
    session.get()?.map(Json).ok_or_else(Error::unauthorized)
}

async fn api_me(Claims(user): Claims<User>) -> Json<User> {
    Json(user)
}
