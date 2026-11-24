//! Email: sending through an `[email.NAME]` binding, and receiving in [`Hooks::on_email`](crate::Hooks::on_email).
//!
//! Sending needs a domain onboarded to Cloudflare Email Sending (`wrangler email sending enable <domain>`);
//! receiving needs an Email Routing rule that delivers an address to this worker. `wrangler dev`
//! simulates both: sent mail is logged, and `POST /cdn-cgi/local/email?from=..&to=..` with a raw
//! message as the body delivers one.

use mail_parser::MessageParser;
use worker::{
    EmailAddress, EmailAttachment, ForwardableEmailMessage, SendEmail, SendEmailBuilder,
    js_sys::{self, Reflect, Uint8Array},
    send::{SendFuture, SendWrapper},
    wasm_bindgen::JsCast,
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
/// `from` must be on a domain onboarded to Email Sending. It and `reply_to` may carry a display
/// name: `Ada Lovelace <ada@example.com>`. Recipients are plain addresses.
/// Set at least one of `text` and `html`; mail with both reaches more inboxes.
#[derive(Clone, Debug, Default)]
pub struct Message {
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub text: Option<String>,
    /// Put anything a user typed through [`escape_html`] first.
    pub html: Option<String>,
    pub reply_to: Option<String>,
    /// Extra headers, e.g. `("X-Entity-Ref-ID".into(), id)`. Email Sending decides which names it accepts.
    pub headers: Vec<(String, String)>,
    pub attachments: Vec<Attachment>,
}

/// A file attached to a [`Message`], or found in an [`EmailBody`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attachment {
    pub filename: String,
    /// The MIME type, e.g. `application/pdf`.
    pub content_type: String,
    pub content: Vec<u8>,
}

/// Escapes text for use in HTML, in element content and in quoted attributes alike.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Splits `Name <address>` into its parts. A plain address has no name.
fn split_address(address: &str) -> (Option<&str>, &str) {
    let named = address.trim().strip_suffix('>').and_then(|a| a.rsplit_once('<'));
    match named {
        Some((name, address)) => (Some(name.trim().trim_matches('"')).filter(|n| !n.is_empty()), address.trim()),
        None => (None, address.trim()),
    }
}

impl Message {
    /// Every value that ends up in a mail header, where a line break could smuggle in another one.
    fn header_values(&self) -> impl Iterator<Item = &String> {
        let recipients = self.to.iter().chain(&self.cc).chain(&self.bcc);
        let headers = self.headers.iter().flat_map(|(name, value)| [name, value]);
        let attachments = self.attachments.iter().flat_map(|a| [&a.filename, &a.content_type]);
        recipients.chain([&self.from, &self.subject]).chain(&self.reply_to).chain(headers).chain(attachments)
    }
}

/// An `[email.NAME]` binding, from [`Ctx::mail`](crate::Ctx::mail).
pub struct Mailer(pub(crate) SendWrapper<SendEmail>);

impl Mailer {
    /// Sends `message` and returns its message id. Under `wrangler dev` the mail is logged, not sent.
    pub async fn send(&self, message: &Message) -> Result<String> {
        if message.to.is_empty() || message.header_values().any(|s| s.contains(['\r', '\n'])) {
            return Err(Error::bad_request(
                "an email needs a recipient, and no line breaks in its addresses, subject, headers or file names",
            ));
        }
        SendFuture::new(async {
            let address = |a: &str| match split_address(a) {
                (Some(name), email) => Ok(EmailAddress::new(name, email)),
                (None, email) => Err(email.to_owned()),
            };
            let email = SendEmailBuilder::new_with_str_and_slice(split_address(&message.from).1, &message.to, &message.subject);
            if let Ok(from) = address(&message.from) {
                email.set_from_with_email_address(&from);
            }
            match message.reply_to.as_deref().map(address) {
                Some(Ok(named)) => email.set_reply_to_with_email_address(&named),
                Some(Err(plain)) => email.set_reply_to(&plain),
                None => {}
            }
            if !message.cc.is_empty() {
                email.set_cc_with_slice(&message.cc);
            }
            if !message.bcc.is_empty() {
                email.set_bcc_with_slice(&message.bcc);
            }
            if let Some(text) = &message.text {
                email.set_text(text);
            }
            if let Some(html) = &message.html {
                email.set_html(html);
            }
            if !message.headers.is_empty() {
                let headers = js_sys::Object::new();
                for (name, value) in &message.headers {
                    Reflect::set(&headers, &name.into(), &value.into()).map_err(|e| Error::internal(format!("{e:?}")))?;
                }
                email.set_headers(headers.unchecked_ref());
            }
            if !message.attachments.is_empty() {
                let files: Vec<EmailAttachment> = message
                    .attachments
                    .iter()
                    .map(|a| {
                        let content = Uint8Array::from(a.content.as_slice());
                        EmailAttachment::new_attachment_with_typed_array(&a.filename, &a.content_type, &content)
                    })
                    .collect();
                email.set_attachments(&files);
            }
            Ok(self.0.send_with_builder(&email).await.map_err(js_error)?.message_id())
        })
        .await
    }

    /// The underlying binding, for inline images and raw MIME messages. Its futures are not `Send`.
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
    pub attachments: Vec<Attachment>,
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
        attachments: message
            .attachments()
            .map(|part| Attachment {
                filename: part.attachment_name().unwrap_or_default().to_owned(),
                content_type: part.content_type().map_or_else(
                    || "application/octet-stream".to_owned(),
                    |ct| format!("{}/{}", ct.ctype(), ct.subtype().unwrap_or("octet-stream")),
                ),
                content: part.decoded().into_owned(),
            })
            .collect(),
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
        assert!(body.attachments.is_empty());

        let attached = b"Subject: Report\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n\
            --b\r\nContent-Type: text/plain\r\n\r\nsee attached\r\n\
            --b\r\nContent-Type: text/csv\r\nContent-Disposition: attachment; filename=\"r.csv\"\r\n\
            Content-Transfer-Encoding: base64\r\n\r\nYSxiCjEsMg==\r\n--b--\r\n";
        let body = parse(attached).unwrap();
        assert_eq!(body.text.as_deref().map(str::trim), Some("see attached"));
        let file = Attachment { filename: "r.csv".into(), content_type: "text/csv".into(), content: b"a,b\n1,2".to_vec() };
        assert_eq!(body.attachments, [file]);
    }

    #[test]
    fn splits_named_addresses() {
        assert_eq!(split_address("ada@example.com"), (None, "ada@example.com"));
        assert_eq!(split_address("Ada Lovelace <ada@example.com>"), (Some("Ada Lovelace"), "ada@example.com"));
        assert_eq!(split_address(" \"Lovelace, Ada\" < ada@example.com > "), (Some("Lovelace, Ada"), "ada@example.com"));
        assert_eq!(split_address("<ada@example.com>"), (None, "ada@example.com"));
    }

    #[test]
    fn escapes_html_and_finds_line_breaks_in_headers() {
        assert_eq!(escape_html(r#"<a href="x">Tom & 'Jerry'</a>"#), "&lt;a href=&quot;x&quot;&gt;Tom &amp; &#39;Jerry&#39;&lt;/a&gt;");
        let clean = Message { from: "a@example.com".into(), to: vec!["b@example.com".into()], ..Default::default() };
        assert!(!clean.header_values().any(|v| v.contains('\n')));
        for dirty in [
            Message { cc: vec!["c@example.com\nBcc: x@example.com".into()], ..clean.clone() },
            Message { headers: vec![("X-A".into(), "1\r\nX-B: 2".into())], ..clean.clone() },
            Message { attachments: vec![Attachment { filename: "a\n.txt".into(), ..Default::default() }], ..clean.clone() },
        ] {
            assert!(dirty.header_values().any(|v| v.contains(['\r', '\n'])));
        }
    }
}
