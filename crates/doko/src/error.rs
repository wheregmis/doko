use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use worker::console_error;

/// `Result` with [`Error`] as the default error, for handlers and app code.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// An HTTP error rendered as `{"error": "..."}`.
///
/// Any `std::error::Error` converts into a 500 via `?`. The 500 body is generic and the
/// real message goes to the log, so internals never leak to clients.
#[derive(Debug, Clone)]
pub struct Error {
    status: StatusCode,
    message: String,
    /// Per-field messages of a validation error, sent as `"fields"`.
    fields: BTreeMap<String, Vec<String>>,
}

impl Error {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into(), fields: BTreeMap::new() }
    }

    /// A 422 `{"error": "validation failed", "fields": {"email": ["is taken"]}}`.
    pub(crate) fn invalid(fields: BTreeMap<String, Vec<String>>) -> Self {
        Self { status: StatusCode::UNPROCESSABLE_ENTITY, message: "validation failed".into(), fields }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized")
    }

    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden")
    }

    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not found")
    }

    /// A 500. `message` is logged, not sent to the client.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.status.as_u16(), self.message)
    }
}

impl<E: std::error::Error> From<E> for Error {
    fn from(e: E) -> Self {
        Self::internal(e.to_string())
    }
}

impl Error {
    /// What the client sees. A server error's message stays in the log.
    fn body(&self) -> Value {
        let mut body = if self.status.is_server_error() {
            json!({ "error": self.status.canonical_reason().unwrap_or("error").to_lowercase() })
        } else {
            json!({ "error": self.message })
        };
        if !self.fields.is_empty() {
            body["fields"] = json!(self.fields);
        }
        body
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let mut res = (self.status, Json(self.body())).into_response();
        if self.status.is_server_error() {
            console_error!("{}", json!({ "level": "error", "status": self.status.as_u16(), "error": self.message }));
            // Picked up by doko's request middleware for `Hooks::on_error`.
            res.extensions_mut().insert(self);
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_errors_become_500() {
        let e: Error = "x".parse::<i32>().unwrap_err().into();
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(e.message().contains("invalid digit"));
    }

    #[test]
    fn bodies_hide_server_errors_and_list_fields() {
        assert_eq!(Error::internal("db password is hunter2").body(), json!({ "error": "internal server error" }));
        assert_eq!(Error::not_found().body(), json!({ "error": "not found" }));
        let invalid = Error::invalid(BTreeMap::from([("email".to_owned(), vec!["is taken".to_owned()])]));
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(invalid.body(), json!({ "error": "validation failed", "fields": { "email": ["is taken"] } }));
    }

    #[test]
    fn displays_status_and_message() {
        let e = Error::bad_request("missing body");
        assert_eq!(e.to_string(), "400: missing body");
    }
}
