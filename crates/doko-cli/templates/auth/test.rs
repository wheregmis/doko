//! The account flow from `doko generate auth`, against `wrangler dev`: `doko test` (needs Node).

use std::collections::BTreeMap;

fn credentials<'a>(email: &'a str, password: &'a str) -> BTreeMap<&'a str, &'a str> {
    BTreeMap::from([("email", email), ("password", password)])
}

/// The `token` in the link of a mail.
fn token(mail: &doko::test::Mail) -> &str {
    mail.text.split("token=").nth(1).expect("a link with a token").split_whitespace().next().unwrap()
}

// The routes below are limited to 10 requests a minute per client, and this test is one client:
// it makes 9. Start another `App` for more.
#[test]
#[ignore = "starts wrangler dev"]
fn registers_verifies_logs_in_and_resets() {
    let app = doko::test::App::start(&["__DB__"]);
    assert_eq!(app.get("/auth/me").status, 401);

    let weak = app.post_json("/auth/register", &credentials("not-an-address", "short"));
    assert_eq!(weak.status, 422, "{}", weak.body);
    assert!(weak.body.contains("must be an email address") && weak.body.contains("at least 8"), "{}", weak.body);

    let ada = credentials("Ada@Example.com", "correct horse");
    assert_eq!(app.post_json("/auth/register", &ada).status, 201);
    assert_eq!(app.post_json("/auth/register", &ada).status, 201, "a taken address gets the same answer");

    // The link in the verification mail verifies the address; a made-up token does not.
    let mail = app.mail_to("ada@example.com");
    assert_eq!(mail.subject, "Confirm your email address");
    assert_eq!(app.get("/auth/verify?token=forged.token").status, 400);
    assert_eq!(app.get(&format!("/auth/verify?token={}", token(&mail))).status, 200);

    // A client keeps the session cookie, like a browser.
    let browser = app.client();
    assert_eq!(browser.post_json("/auth/login", &credentials("ada@example.com", "wrong horse")).status, 401);
    assert_eq!(browser.get("/auth/me").status, 401);
    let login = browser.post_json("/auth/login", &credentials("ada@example.com", "correct horse"));
    assert_eq!(login.status, 200, "{}", login.body);
    let me = browser.get("/auth/me");
    assert!(me.body.contains("\"email\":\"ada@example.com\"") && me.body.contains("\"email_verified\":true"), "{}", me.body);
    assert_eq!(browser.send("POST", "/auth/logout", &[], "").status, 200);
    assert_eq!(browser.get("/auth/me").status, 401);

    // Forgot password: a mail with a link that works once.
    assert_eq!(app.post_json("/auth/forgot-password", &BTreeMap::from([("email", "ada@example.com")])).status, 202);
    let mail = app.mail_to("ada@example.com");
    assert_eq!(mail.subject, "Reset your password");
    let reset = BTreeMap::from([("token", token(&mail)), ("password", "battery staple")]);
    assert_eq!(app.post_json("/auth/reset-password", &reset).status, 204);
    assert_eq!(app.post_json("/auth/reset-password", &reset).status, 400, "the link works once");
    assert_eq!(app.post_json("/auth/login", &credentials("ada@example.com", "battery staple")).status, 200);
}
