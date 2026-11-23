use axum::{extract::Request, http::HeaderValue, middleware::Next, response::Response};
use serde_json::json;
use worker::{Date, console_log};

/// Logs one JSON line per request and echoes the request id as `x-request-id`.
pub(crate) async fn requests(req: Request, next: Next) -> Response {
    // std::time::Instant panics on wasm32; Date is the Workers clock.
    let start = Date::now().as_millis();
    let method = req.method().to_string();
    let path = req.uri().path().to_owned();
    // shortcut: cf-ray is the request id; it's absent under local `wrangler dev`, so ids show as "-" there.
    let id = req.headers().get("cf-ray").cloned();

    let mut res = next.run(req).await;

    let id_str = id.as_ref().and_then(|v| v.to_str().ok()).unwrap_or("-");
    console_log!(
        "{}",
        json!({
            "level": "info",
            "method": method,
            "path": path,
            "status": res.status().as_u16(),
            "ms": Date::now().as_millis() - start,
            "request_id": id_str,
        })
    );
    res.headers_mut().insert("x-request-id", id.unwrap_or(HeaderValue::from_static("-")));
    res
}
