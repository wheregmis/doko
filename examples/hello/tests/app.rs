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

/// Signed by the throwaway private key whose public half is JWT_PUBLIC_KEY in .dev.vars; expires in 2100.
const RS256_TOKEN: &str = "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJpZCI6NywidXNlcm5hbWUiOiJyc2EtdXNlciIsImV4cCI6NDEwMjQ0NDgwMH0.EESzC-7209pg2ekFVlQBFMzhXkw8ZAM_L_zeWrkZllsSHSWZEXHMCaJ8o2jV7skWPiE_0mNY40C-h6WtOPrPWoxhxnOj__gxzbjxCwwCd79ijeDAGdGTHLxi8EX-F7jzLAvnPKZeKMEAQ8rKiGHoZmUmJt0gUj93FAF08eyqvMVQ03DqVeTFpM11ssk2vvnY_rBbDFzE9GpFPt3o9fJneFePYVKa4qqLfZD1zdbfHGmmiAeNXROgrcfw_RQZnLceH22shM86_bKg9GIAatBHIw-xi_dsx4VZx7toIuYf7UYR5tTDTrlhSNgPQSFt2rngBxTUwhm1DSlZQB9jPdYXjw";

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
    let cookie = set_cookie.split(';').next().unwrap();

    // The session cookie opens the protected route; a tampered one does not.
    let me = app.send("GET", "/me", &[("cookie", cookie)], "");
    assert_eq!((me.status, me.json::<Value>()["username"].as_str()), (200, Some("ada")), "{}", me.body);
    assert_eq!(app.send("GET", "/me", &[("cookie", &format!("{cookie}x"))], "").status, 401);
    assert_eq!(app.send("GET", "/me", &[("cookie", "doko_session=forged.c2ln")], "").status, 401);

    // The JWT from login works as a bearer token, and so does an RS256 token from another issuer.
    let token = login.json::<Value>()["token"].as_str().unwrap().to_owned();
    let bearer = |t: &str| app.send("GET", "/api/me", &[("authorization", &format!("Bearer {t}"))], "");
    assert_eq!(bearer(&token).json::<Value>()["username"], "ada");
    assert_eq!(bearer(RS256_TOKEN).json::<Value>()["username"], "rsa-user");
    assert_eq!(bearer(&format!("{token}x")).status, 401);
    assert_eq!(bearer(&format!("{RS256_TOKEN}x")).status, 401);
    assert_eq!(app.get("/api/me").status, 401);

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
