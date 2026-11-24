//! The mail `controllers/auth.rs` sends. Written by `doko generate auth`.

use doko::{Message, Result, render_html, render_text};
use serde::Serialize;

/// Must be on a domain onboarded to Cloudflare Email Sending. Under `doko dev`, mail is logged, not sent.
const FROM: &str = "Accounts <accounts@example.com>";

#[derive(Serialize)]
struct Vars<'a> {
    link: &'a str,
}

fn message(to: &str, subject: &str, text: &str, html: &str, link: &str) -> Result<Message> {
    let vars = Vars { link };
    Ok(Message {
        from: FROM.into(),
        to: vec![to.into()],
        subject: subject.into(),
        text: Some(render_text(text, &vars)?),
        html: Some(render_html(html, &vars)?),
        ..Default::default()
    })
}

pub fn verify(to: &str, link: &str) -> Result<Message> {
    message(
        to,
        "Confirm your email address",
        "Welcome! Confirm your email address by opening this link:\n\n{{ link }}\n",
        "<p>Welcome! <a href=\"{{ link }}\">Confirm your email address</a>.</p>",
        link,
    )
}

pub fn reset(to: &str, link: &str) -> Result<Message> {
    message(
        to,
        "Reset your password",
        "Open this link within an hour to choose a new password:\n\n{{ link }}\n\n\
         If you didn't ask for this, ignore this mail: your password stays as it is.\n",
        "<p><a href=\"{{ link }}\">Choose a new password</a>. The link works for an hour.</p>\
         <p>If you didn't ask for this, ignore this mail: your password stays as it is.</p>",
        link,
    )
}
