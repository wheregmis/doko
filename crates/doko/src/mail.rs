//! Email: sending through an `[email.NAME]` binding, and receiving in [`Hooks::on_email`](crate::Hooks::on_email).
//!
//! Sending needs a domain onboarded to Cloudflare Email Sending (`wrangler email sending enable <domain>`);
//! receiving needs an Email Routing rule that delivers an address to this worker. `wrangler dev`
//! simulates both: sent mail is logged, and `POST /cdn-cgi/local/email?from=..&to=..` with a raw
//! message as the body delivers one.

use mail_parser::MessageParser;
use worker::{
    ForwardableEmailMessage, SendEmail, SendEmailBuilder,
    js_sys::{self, Reflect},
    send::{SendFuture, SendWrapper},
};

use crate::{Error, Result};

/// Email Service errors carry a `code` such as `E_SENDER_DOMAIN_NOT_AVAILABLE`; keep it in the log.
fn js_error(e: js_sys::Error) -> Error {
    let code = Reflect::get(&e, &"code".into()).ok().and_then(|c| c.as_string()).unwrap_or_default();
    Error::internal(format!("email {code}: {}", String::from(e.message())))
}

/// An email to send with [`Mailer::send`].
///
/// ```ignore
/// ctx.mail(EMAIL)?.send(&Message {
///     from: "hello@example.com".into(),
///     to: vec![user.email],
///     subject: "Welcome".into(),
///     text: Some("Thanks for signing up.".into()),
///     ..Default::default()
/// }).await?;
/// ```
///
/// Addresses are plain (`ada@example.com`). `from` must be on a domain onboarded to Email Sending.
/// Set at least one of `text` and `html`; mail with both reaches more inboxes.
#[derive(Clone, Debug, Default)]
pub struct Message {
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    pub text: Option<String>,
    /// Escape anything a user typed before putting it in here.
    pub html: Option<String>,
    pub reply_to: Option<String>,
}

/// An `[email.NAME]` binding, from [`Ctx::mail`](crate::Ctx::mail).
pub struct Mailer(pub(crate) SendWrapper<SendEmail>);

impl Mailer {
    /// Sends `message` and returns its message id. Under `wrangler dev` the mail is logged, not sent.
    pub async fn send(&self, message: &Message) -> Result<String> {
        // An address with a line break could smuggle extra headers into the mail.
        let addresses = message.to.iter().chain([&message.from]).chain(&message.reply_to);
        if message.to.is_empty() || addresses.chain([&message.subject]).any(|s| s.contains(['\r', '\n'])) {
            return Err(Error::bad_request("an email needs a recipient, and no line breaks in its addresses or subject"));
        }
        SendFuture::new(async {
            let mut email = SendEmailBuilder::builder_with_str_and_slice(&message.from, &message.to, &message.subject);
            if let Some(text) = &message.text {
                email = email.text(text);
            }
            if let Some(html) = &message.html {
                email = email.html(html);
            }
            if let Some(reply_to) = &message.reply_to {
                email = email.reply_to(reply_to);
            }
            Ok(self.0.send_with_builder(&email.build()).await.map_err(js_error)?.message_id())
        })
        .await
    }

    /// The underlying binding, for attachments, cc/bcc and custom headers. Its futures are not `Send`.
    pub fn raw(&self) -> &SendEmail {
        &self.0
    }
}

/// An email delivered to this worker, passed to [`Hooks::on_email`](crate::Hooks::on_email).
pub struct InboundEmail(pub(crate) SendWrapper<ForwardableEmailMessage>);

/// The decoded content of an [`InboundEmail`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EmailBody {
    pub subject: Option<String>,
    /// The plain-text body, or the HTML body converted to text when there is no plain one.
    pub text: Option<String>,
    pub html: Option<String>,
}

impl InboundEmail {
    /// The envelope sender. It can differ from the `From` header, which [`header`](Self::header) gives.
    pub fn from(&self) -> String {
        self.0.from()
    }

    /// The address the mail was delivered to.
    pub fn to(&self) -> String {
        self.0.to()
    }

    /// A header as it was sent, e.g. `header("message-id")`. For the decoded subject, use [`read`](Self::read).
    pub fn header(&self, name: &str) -> Option<String> {
        self.0.headers().get(name).ok().flatten()
    }

    /// Reads and decodes the message. The content can be read only once.
    pub async fn read(&self) -> Result<EmailBody> {
        let raw = SendFuture::new(self.0.raw_bytes()).await?;
        parse(&raw).ok_or_else(|| Error::bad_request("not a readable email"))
    }

    /// Bounces the mail back to its sender with `reason`.
    pub fn reject(&self, reason: &str) {
        self.0.set_reject(reason);
    }

    /// Forwards the mail to `to`, which must be a verified destination address of the account.
    pub async fn forward(&self, to: &str) -> Result<()> {
        SendFuture::new(self.0.forward(to)).await.map(|_| ()).map_err(js_error)
    }

    /// The underlying message, for replies and forwarding with extra headers.
    pub fn raw(&self) -> &ForwardableEmailMessage {
        &self.0
    }
}

fn parse(raw: &[u8]) -> Option<EmailBody> {
    let message = MessageParser::default().parse(raw)?;
    Some(EmailBody {
        subject: message.subject().map(str::to_owned),
        text: message.body_text(0).map(|t| t.into_owned()),
        // `body_html` would also turn a plain-text body into HTML; only report HTML that was sent.
        html: message.html_body().find_map(|part| part.html_utf8()).map(|h| h.into_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_mail() {
        let plain = parse(b"From: a@example.com\r\nSubject: =?UTF-8?B?SGVsbG8g8J+Riw==?=\r\n\r\nHi there\r\n").unwrap();
        assert_eq!(plain.subject.as_deref(), Some("Hello \u{1F44B}"), "encoded subjects are decoded");
        assert_eq!(plain.text.as_deref().map(str::trim), Some("Hi there"));
        assert_eq!(plain.html, None);

        let multipart = b"Subject: Two parts\r\nContent-Type: multipart/alternative; boundary=b\r\n\r\n\
            --b\r\nContent-Type: text/plain\r\n\r\nplain\r\n--b\r\nContent-Type: text/html\r\n\r\n<p>rich</p>\r\n--b--\r\n";
        let body = parse(multipart).unwrap();
        assert_eq!(body.text.as_deref().map(str::trim), Some("plain"));
        assert_eq!(body.html.as_deref().map(str::trim), Some("<p>rich</p>"));
    }
}
