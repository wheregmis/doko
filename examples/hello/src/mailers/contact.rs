use doko::Message;

/// A contact form submission, sent to the site owner. Replying to it answers the visitor.
pub fn message(to: &str, visitor: &str, text: &str) -> Message {
    Message {
        // Must be on a domain onboarded to Email Sending.
        from: "contact@example.com".into(),
        to: vec![to.into()],
        subject: "New contact form message".into(),
        // Text only: the visitor's words would need escaping before going into `html`.
        text: Some(format!("From: {visitor}\n\n{text}\n")),
        reply_to: Some(visitor.into()),
        ..Default::default()
    }
}
