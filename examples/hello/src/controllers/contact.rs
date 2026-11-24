//! A contact form that emails the site owner.

use doko::{
    Ctx, Error, RateLimit, Result, RouterExt,
    axum::{Json, Router, http::StatusCode, routing::post},
};
use serde::Deserialize;

use crate::bindings::{EMAIL, RATE_LIMITER};
use crate::mailers::contact;

#[derive(Deserialize)]
struct Contact {
    email: String,
    message: String,
}

#[derive(Deserialize)]
struct Config {
    contact_to: String,
}

pub fn routes() -> Router<Ctx> {
    Router::new()
        .route("/contact", post(create))
        // Every request sends a mail, so cap what one client can send.
        .rate_limit(RateLimit::new(RATE_LIMITER, "contact", 5, 60))
}

async fn create(ctx: Ctx, Json(c): Json<Contact>) -> Result<StatusCode> {
    // The address becomes the mail's reply-to. This is a shape check, not proof that it exists.
    let plausible = matches!(c.email.split_once('@'), Some((name, domain)) if !name.is_empty() && domain.contains('.'))
        && !c.email.contains(char::is_whitespace);
    if !plausible || c.message.trim().is_empty() {
        return Err(Error::bad_request("a valid email and a message are required"));
    }
    let to = ctx.config::<Config>()?.contact_to;
    ctx.mail(EMAIL)?.send(&contact::message(&to, &c.email, &c.message)).await?;
    Ok(StatusCode::ACCEPTED)
}
