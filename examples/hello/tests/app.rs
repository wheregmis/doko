//! Runs the hello worker under `wrangler dev`: `cargo test -p hello -- --ignored` (needs Node).

use std::time::{Duration, Instant};

use serde_json::{Value, json};

#[test]
#[ignore = "starts wrangler dev"]
fn notes_and_jobs() {
    let app = doko::test::App::start(&["DB"]);

    let health: Value = app.get("/health").json();
    assert_eq!(health, json!({ "status": "ok", "env": "dev" }));

    assert_eq!(app.post_json("/notes", &json!({ "body": " " })).status, 400);
    let res = app.post_json("/notes", &json!({ "body": "three little words" }));
    assert_eq!(res.status, 201, "{}", res.body);
    let id = res.json::<Value>()["id"].as_i64().unwrap();

    // The CountWords job fills `words` in the background, then purges the cached note.
    let started = Instant::now();
    loop {
        let note: Value = app.get(&format!("/notes/{id}")).json();
        if note["words"] == 3 {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "CountWords never ran: {note}");
        std::thread::sleep(Duration::from_millis(500));
    }

    assert_eq!(app.get("/notes/999999").status, 404);
}

#[test]
#[ignore = "starts wrangler dev"]
fn rate_limits() {
    let app = doko::test::App::start(&[]);
    for _ in 0..3 {
        assert_eq!(app.get("/limited").status, 200);
    }
    let res = app.get("/limited");
    assert_eq!(res.status, 429, "{}", res.body);
    assert_eq!(res.json::<Value>(), json!({ "error": "too many requests" }));
    assert_eq!(app.get("/health").status, 200, "other routes are not limited");
}
