use doko::{Message, Result, render_html};
use serde_json::json;

/// A contact form submission, sent to the site owner. Replying to it answers the visitor.
pub fn message(to: &str, visitor: &str, text: &str) -> Result<Message> {
    Ok(Message {
        // Must be on a domain onboarded to Email Sending.
        from: "Hello Contact Form <contact@example.com>".into(),
        to: vec![to.into()],
        subject: "New contact form message".into(),
        text: Some(format!("From: {visitor}\n\n{text}\n")),
        // Every value is escaped, so the visitor's words can't inject markup into the mail.
        html: Some(render_html(
            "<p>From: {{ visitor }}</p><p>{{ text }}</p>",
            &json!({ "visitor": visitor, "text": text }),
        )?),
        reply_to: Some(visitor.into()),
        ..Default::default()
    })
}
