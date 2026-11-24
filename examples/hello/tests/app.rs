//! Runs the hello worker under `wrangler dev`: `cargo test -p hello -- --ignored` (needs Node).

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    time::{Duration, Instant},
};

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

    // `key_by` counts each API key on its own, apart from the IP count above.
    let keyed = |key: &str| app.send("GET", "/limited", &[("x-api-key", key)], "").status;
    assert_eq!([keyed("a"), keyed("a"), keyed("a"), keyed("a")], [200, 200, 200, 429]);
    assert_eq!(keyed("b"), 200);
}

#[test]
#[ignore = "starts wrangler dev"]
fn cache_varies_and_files_stream() {
    let app = doko::test::App::start(&[]);

    // One cached copy per accept-language value.
    let language = |l: &str| app.send("GET", "/language", &[("accept-language", l)], "");
    let de = language("de");
    assert_eq!((de.body.as_str(), de.header("x-doko-cache"), de.header("vary")), ("de", Some("MISS"), Some("accept-language")));
    assert_eq!(language("en").body, "en", "another language is not served the German copy");
    let started = Instant::now();
    loop {
        // The first response is stored in the background, so the hit can lag it.
        let again = language("de");
        assert_eq!(again.body, "de");
        if again.header("x-doko-cache") == Some("HIT") {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "never cached");
        std::thread::sleep(Duration::from_millis(200));
    }

    // A small upload is one put; one larger than a part (8 MiB) is a multipart upload.
    for (key, size) in [("small.txt", 5), ("large.txt", 9 * 1024 * 1024)] {
        let path = format!("/files/{key}");
        let put = app.send("PUT", &path, &[("content-type", "text/plain")], &"x".repeat(size));
        assert_eq!(put.status, 201, "{}", put.body);
        let got = app.get(&path);
        assert_eq!((got.status, got.body.len(), got.header("content-type")), (200, size, Some("text/plain")));
    }
}

/// Needs the local Postgres from doko.toml's `[hyperdrive.PG]`, and `psql` for the migrations.
#[test]
#[ignore = "starts wrangler dev"]
fn postgres() {
    if TcpStream::connect("127.0.0.1:5433").is_err() {
        eprintln!("skipped: no Postgres at 127.0.0.1:5433 (see [hyperdrive.PG] in doko.toml)");
        return;
    }
    // `doko migrate --pg` applies migrations/pg/*.sql; a second run finds nothing to do.
    for _ in 0..2 {
        let migrated = std::process::Command::new("cargo")
            .args(["run", "-q", "-p", "doko-cli", "--", "migrate", "--pg"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .expect("running doko migrate --pg");
        assert!(migrated.success(), "doko migrate --pg failed");
    }

    let app = doko::test::App::start(&[]);
    assert_eq!(app.post_json("/todos", &json!({ "title": " " })).status, 400);
    // The database outlives the test, so the title is unique to this run.
    let title = format!("write tests {}", std::process::id());
    let created = app.post_json("/todos", &json!({ "title": title, "tags": ["a", "b"] }));
    assert_eq!(created.status, 201, "{}", created.body);
    let todo: Value = created.json();
    assert_eq!((&todo["title"], &todo["done"], &todo["tags"]), (&json!(title), &json!(false), &json!(["a", "b"])));
    assert!(todo["created_at"].as_str().is_some_and(|t| t.starts_with("20") && t.ends_with('Z')), "{todo}");

    let id = todo["id"].as_i64().unwrap();
    assert_eq!(app.send("POST", &format!("/todos/{id}/done"), &[], "").status, 204);
    assert_eq!(app.send("POST", "/todos/999999999/done", &[], "").status, 404);
    let todos: Vec<Value> = app.get("/todos").json();
    assert!(todos.iter().any(|t| t["id"] == id && t["done"] == true), "{todos:?}");
}

/// Calls Workers AI for real, which is billed to the logged-in account: run with `DOKO_TEST_AI=1`.
#[test]
#[ignore = "starts wrangler dev"]
fn ai() {
    if std::env::var("DOKO_TEST_AI").is_err() {
        eprintln!("skipped: set DOKO_TEST_AI=1 to call Workers AI");
        return;
    }
    let app = doko::test::App::start(&[]);
    assert_eq!(app.post_json("/summarize", &json!({ "text": " " })).status, 400);
    let text = "Durable Objects give each named object its own storage and a single thread, \
                which makes them a good fit for counters, chat rooms and rate limiters.";
    let summarized = app.post_json("/summarize", &json!({ "text": text }));
    assert_eq!(summarized.status, 200, "{}", summarized.body);
    assert!(summarized.json::<Value>()["summary"].as_str().is_some_and(|s| !s.is_empty()), "{}", summarized.body);

    let streamed = app.post_json("/summarize/stream", &json!({ "text": text }));
    assert_eq!((streamed.status, streamed.header("content-type")), (200, Some("text/event-stream")), "{}", streamed.body);
    assert!(streamed.body.contains("data: ") && streamed.body.contains("[DONE]"), "{}", streamed.body);
}

/// A minimal WebSocket client: the handshake, then one text frame each way.
fn websocket_roundtrip(app: &doko::test::App, path: &str, text: &str) -> String {
    let addr = app.url().trim_start_matches("http://");
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
    .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(line.contains(" 101 "), "no upgrade: {line}");
    while line != "\r\n" {
        line.clear();
        reader.read_line(&mut line).unwrap();
    }
    // Client frames are masked; an all-zero mask leaves the payload readable.
    let mut frame = vec![0x81, 0x80 | text.len() as u8, 0, 0, 0, 0];
    frame.extend_from_slice(text.as_bytes());
    stream.write_all(&frame).unwrap();
    let mut head = [0; 2];
    reader.read_exact(&mut head).unwrap();
    assert_eq!(head[0], 0x81, "expected one text frame");
    let mut payload = vec![0; head[1] as usize];
    reader.read_exact(&mut payload).unwrap();
    String::from_utf8(payload).unwrap()
}

#[test]
#[ignore = "starts wrangler dev"]
fn websockets() {
    let app = doko::test::App::start(&[]);
    assert_eq!(websocket_roundtrip(&app, "/rooms/lobby", "hello room"), "hello room");
    assert_eq!(app.get("/rooms/lobby").status, 426, "a plain GET is not an upgrade");
    // Stub::call: a JSON round trip with the room's object.
    let members = app.get("/rooms/lobby/members");
    // 0 or 1: the test's socket was dropped without a close frame, so the room may still list it.
    assert!(members.status == 200 && members.json::<u64>() <= 1, "{}", members.body);
}

#[test]
#[ignore = "starts wrangler dev"]
fn times_out_slow_requests() {
    let app = doko::test::App::start(&[]);
    let started = Instant::now();
    let res = app.get("/slow");
    assert_eq!((res.status, res.json::<Value>()), (504, json!({ "error": "gateway timeout" })));
    // request_timeout_secs = 10 in doko.toml; the handler itself would take a minute.
    assert!((9..30).contains(&started.elapsed().as_secs()), "{:?}", started.elapsed());
}

/// Signed by the throwaway private key whose public half is JWT_PUBLIC_KEY in .dev.vars; expires in 2100.
/// Its `iss` is the `jwt_issuer` in doko.toml.
const RS256_TOKEN: &str = "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJpZCI6NywidXNlcm5hbWUiOiJyc2EtdXNlciIsImV4cCI6NDEwMjQ0NDgwMCwiaXNzIjoiZG9rby1oZWxsbyJ9.Vl_jVmCm--8JGDHUtD2SdYcm76oENTwbqeV_X61j2SHI1XShlF4oyTiR8e50h_Rx37WPiBlV6nBXo1aYf5Zke5BeTXbz8jkKRakVBBre4K-n5KvIMlxphGt7jqDhpRuR_af6HypKkAYfXGoX3i1uJjCZZEABgbt5GVWqbFM9wxqxlRsqCk5YV0EnnrcwT5VrJiNzxdhAv3qS9eQYdd6Fi7mD1RcNUV268sxIFLa5fLI8yG8SJTA3EBjJPF9TqrW0uHFUKGhjBq4aLeyPn8GkebbbtLzJK-tA_xOgMpw4rg5SNJpZ3JNkocXG5XJQa15fWou_0FayfeFVKx5JEOoBjA";
/// The same, from an issuer this app does not trust.
const RS256_WRONG_ISSUER: &str = "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJpZCI6NywidXNlcm5hbWUiOiJyc2EtdXNlciIsImV4cCI6NDEwMjQ0NDgwMCwiaXNzIjoic29tZW9uZS1lbHNlIn0.pojD6cuV4EGm8ZEp0S952L0tOsAGDzvEb0e0QSMlMlFqM9tKtGqLVKL339QPQOPxwJLOH7r2DJpjyuBdhlTG4yssEyR0n5U9W4-WlL03wApqYprV5HSpc4Or5YA8aOsx7-OdcxURXAyFpJgEnxILHbQLVkxkZAlA3RYFxc5mGQnn0qx7HxFkk3Tn2Lvv9wqcICCNAwACrB43F8rcvTb8Fz5f6htEByWWiaZmfbovQfpt1YIYF3LOWBtjd7mVNQ15SmfQhKUe70GB4uPxI6syo4W2Z_RmbW7_svBGjlmo9LIf0jF30az5nIhbEF8UR-qYeGxcggN72XN0Cs0VHKH6Mw";

#[test]
#[ignore = "starts wrangler dev"]
fn auth() {
    let app = doko::test::App::start(&["DB"]);
    let creds = json!({ "username": "ada", "password": "correct horse" });

    assert_eq!(app.get("/me").status, 401);
    assert_eq!(app.post_json("/signup", &json!({ "username": "ada", "password": "short" })).status, 400);
    assert_eq!(app.post_json("/signup", &creds).status, 201);
    assert_eq!(app.post_json("/signup", &creds).status, 409);
    assert_eq!(app.post_json("/login", &json!({ "username": "ada", "password": "wrong horse" })).status, 401);
    assert_eq!(app.post_json("/login", &json!({ "username": "nobody", "password": "correct horse" })).status, 401);

    let login = app.post_json("/login", &creds);
    assert_eq!(login.status, 200, "{}", login.body);
    let set_cookie = login.header("set-cookie").expect("login sets a cookie");
    assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("Secure"), "{set_cookie}");
    // The cookie name and lifetime come from [auth] in doko.toml.
    assert!(set_cookie.starts_with("hello_session=") && set_cookie.contains("Max-Age=86400"), "{set_cookie}");
    let cookie = set_cookie.split(';').next().unwrap();

    // The session cookie opens the protected route; a tampered one does not.
    let me = app.send("GET", "/me", &[("cookie", cookie)], "");
    assert_eq!((me.status, me.json::<Value>()["username"].as_str()), (200, Some("ada")), "{}", me.body);
    assert_eq!(app.send("GET", "/me", &[("cookie", &format!("{cookie}x"))], "").status, 401);
    assert_eq!(app.send("GET", "/me", &[("cookie", "hello_session=forged.c2ln")], "").status, 401);

    // The JWT from login works as a bearer token, and so does an RS256 token from another issuer.
    let token = login.json::<Value>()["token"].as_str().unwrap().to_owned();
    let bearer = |t: &str| app.send("GET", "/api/me", &[("authorization", &format!("Bearer {t}"))], "");
    assert_eq!(bearer(&token).json::<Value>()["username"], "ada");
    assert_eq!(bearer(RS256_TOKEN).json::<Value>()["username"], "rsa-user");
    assert_eq!(bearer(&format!("{token}x")).status, 401);
    assert_eq!(bearer(&format!("{RS256_TOKEN}x")).status, 401);
    assert_eq!(bearer(RS256_WRONG_ISSUER).status, 401, "a valid signature from the wrong `iss` is refused");
    assert_eq!(app.get("/api/me").status, 401);

    // Roles: `/admin` needs a login at all (401), and then the admin role (403).
    assert_eq!(app.get("/admin").status, 401);
    assert_eq!(app.send("GET", "/admin", &[("cookie", cookie)], "").status, 403);
    assert_eq!(bearer(&token).status, 200);
    assert_eq!(app.send("GET", "/admin", &[("authorization", &format!("Bearer {token}"))], "").status, 403);
    let admin = json!({ "username": "admin", "password": "correct horse" });
    assert_eq!(app.post_json("/signup", &admin).status, 201);
    let admin_login = app.post_json("/login", &admin);
    let admin_cookie = admin_login.header("set-cookie").unwrap().split(';').next().unwrap().to_owned();
    let welcomed = app.send("GET", "/admin", &[("cookie", &admin_cookie)], "");
    assert_eq!((welcomed.status, welcomed.body.as_str()), (200, "welcome, admin"));

    // Logout ends the session on the server, so the old cookie stops working.
    let logout = app.send("POST", "/logout", &[("cookie", cookie)], "");
    assert_eq!(logout.status, 200);
    assert!(logout.header("set-cookie").unwrap().contains("Max-Age=0"));
    assert_eq!(app.send("GET", "/me", &[("cookie", cookie)], "").status, 401);
}

#[test]
#[ignore = "starts wrangler dev"]
fn email() {
    let app = doko::test::App::start(&["DB"]);

    // Sending: wrangler dev logs the mail instead of delivering it.
    assert_eq!(app.post_json("/contact", &json!({ "email": "not-an-address", "message": "hi" })).status, 400);
    assert_eq!(app.post_json("/contact", &json!({ "email": "ada@example.com", "message": " " })).status, 400);
    let sent = app.post_json("/contact", &json!({ "email": "ada@example.com", "message": "Hello there" }));
    assert_eq!(sent.status, 202, "{}", sent.body);

    // Receiving: wrangler dev's local endpoint delivers a raw message to Hooks::on_email.
    let raw = "From: Ada <ada@example.com>\r\nTo: notes@example.com\r\nSubject: =?UTF-8?B?THVuY2gg8J+NnT8=?=\r\n\
               Message-ID: <1@example.com>\r\nDate: Mon, 23 Nov 2026 10:00:00 +0000\r\n\r\nNoon?\r\n";
    let path = "/cdn-cgi/local/email?from=ada@example.com&to=notes@example.com";
    let delivered = app.send("POST", path, &[("content-type", "message/rfc822")], raw);
    assert_eq!(delivered.status, 200, "{}", delivered.body);
    let notes: Vec<Value> = app.get("/notes").json();
    assert!(notes.iter().any(|n| n["body"] == "mail from ada@example.com: Lunch \u{1F35D}?"), "{notes:?}");
}
