use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use worker::console_error;

/// `Result` with [`Error`] as the default error, for handlers and app code.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// An HTTP error rendered as `{"error": "..."}`.
///
/// Any `std::error::Error` converts into a 500 via `?`. The 500 body is generic and the
/// real message goes to the log, so internals never leak to clients.
#[derive(Debug)]
pub struct Error {
    status: StatusCode,
    message: String,
}

impl Error {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
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

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let message = if self.status.is_server_error() {
            console_error!("{}", json!({ "level": "error", "status": self.status.as_u16(), "error": self.message }));
            self.status.canonical_reason().unwrap_or("error").to_lowercase()
        } else {
            self.message
        };
        (self.status, Json(json!({ "error": message }))).into_response()
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
    fn displays_status_and_message() {
        let e = Error::bad_request("missing body");
        assert_eq!(e.to_string(), "400: missing body");
    }
}
