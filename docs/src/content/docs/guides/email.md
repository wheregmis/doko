---
title: "Email"
description: "Send mail through a typed binding and receive it in Hooks::on_email."
---

```toml
[email.EMAIL]             # optional: destination_address, allowed_destination_addresses,
                          # allowed_sender_addresses, remote (same as wrangler's send_email)
```

```rust
use doko::{InboundEmail, Message};

// Sending. Returns the message id.
ctx.mail(EMAIL)?.send(&Message {
    from: "hello@example.com".into(),
    to: vec![user.email],
    subject: "Welcome".into(),
    text: Some("Thanks for signing up.".into()),
    html: Some(format!("<p>Thanks for signing up, {}.</p>", doko::escape_html(&user.name))),
    ..Default::default()
}).await?;

// Receiving.
impl Hooks for App {
    async fn on_email(ctx: Ctx, email: InboundEmail) -> Result<()> {
        let body = email.read().await?;                  // subject, text, html: decoded
        save(&ctx, &email.from(), body.subject, body.text).await
        // also: email.to(), email.header("message-id"), email.reject("why"), email.forward("me@example.com")
    }
    // ...
}
```

- **Sending** needs a domain onboarded to Cloudflare Email Sending (`wrangler email sending enable example.com`), and `from` must be on it. It is for transactional mail, not newsletters.
- **Receiving** needs an Email Routing rule that delivers an address to this worker. Without an `on_email`, mail is rejected. An `Err` fails the delivery, so the sending server retries or bounces. doko logs the sender and recipient of every inbound mail.
- **More fields.** `cc`, `bcc`, `reply_to`, `headers: vec![("X-Entity-Ref-ID".into(), id)]` and `attachments: vec![Attachment { filename, content_type, content }]`. `from` and `reply_to` may carry a display name: `"Ada Lovelace <ada@example.com>"`. Inbound attachments are in `email.read().await?.attachments`.
- **Templates.** `doko::render_html(include_str!("welcome.html"), &json!({ "name": user.name }))?` fills `{{ name }}` placeholders (dots reach nested values) and HTML-escapes every value. `render_text` does the same without escaping, for `text`. A placeholder with no value is an error. There are no loops or conditions.
- **Escape user input** with `doko::escape_html` (or a template) before putting it in `html`. `send` refuses line breaks in addresses, the subject, headers and file names.
- **Locally,** `doko dev` logs sent mail instead of sending it (set `remote = true` on the binding to send for real). To deliver a test mail, POST a raw message, with a `Message-ID` header, to wrangler's local endpoint:

```bash
curl 'localhost:8787/cdn-cgi/local/email?from=ada@example.com&to=notes@example.com' --data-binary @mail.eml
```

`Mailer::raw()` and `InboundEmail::raw()` give the `worker` types, for inline images, raw MIME messages and replies.
