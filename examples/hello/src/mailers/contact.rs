use doko::{Message, escape_html};

/// A contact form submission, sent to the site owner. Replying to it answers the visitor.
pub fn message(to: &str, visitor: &str, text: &str) -> Message {
    Message {
        // Must be on a domain onboarded to Email Sending.
        from: "Hello Contact Form <contact@example.com>".into(),
        to: vec![to.into()],
        subject: "New contact form message".into(),
        text: Some(format!("From: {visitor}\n\n{text}\n")),
        // The visitor's words are escaped, so they can't inject markup into the mail.
        html: Some(format!("<p>From: {}</p><p>{}</p>", escape_html(visitor), escape_html(text))),
        reply_to: Some(visitor.into()),
        ..Default::default()
    }
}
