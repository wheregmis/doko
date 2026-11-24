//! The account flow from `doko generate auth`, against `wrangler dev`: `cargo test -- --ignored` (needs Node).

use std::collections::BTreeMap;

fn credentials<'a>(email: &'a str, password: &'a str) -> BTreeMap<&'a str, &'a str> {
    BTreeMap::from([("email", email), ("password", password)])
}

#[test]
#[ignore = "starts wrangler dev"]
fn registers_logs_in_and_out() {
    let app = doko::test::App::start(&["__DB__"]);
    assert_eq!(app.get("/auth/me").status, 401);

    let weak = app.post_json("/auth/register", &credentials("not-an-address", "short"));
    assert_eq!(weak.status, 422, "{}", weak.body);
    assert!(weak.body.contains("must be an email address") && weak.body.contains("at least 8"), "{}", weak.body);

    let ada = credentials("Ada@Example.com", "correct horse");
    assert_eq!(app.post_json("/auth/register", &ada).status, 201);
    assert_eq!(app.post_json("/auth/register", &ada).status, 201, "a taken address gets the same answer");

    assert_eq!(app.post_json("/auth/login", &credentials("ada@example.com", "wrong horse")).status, 401);
    assert_eq!(app.post_json("/auth/login", &credentials("nobody@example.com", "correct horse")).status, 401);
    let login = app.post_json("/auth/login", &credentials("ada@example.com", "correct horse"));
    assert_eq!(login.status, 200, "{}", login.body);
    let cookie = login.header("set-cookie").expect("a session cookie").split(';').next().unwrap().to_owned();

    let me = app.send("GET", "/auth/me", &[("cookie", &cookie)], "");
    assert_eq!(me.status, 200);
    assert!(me.body.contains("\"email\":\"ada@example.com\"") && me.body.contains("\"email_verified\":false"), "{}", me.body);

    assert_eq!(app.get("/auth/verify?token=forged.token").status, 400);
    assert_eq!(app.post_json("/auth/forgot-password", &BTreeMap::from([("email", "nobody@example.com")])).status, 202);

    assert_eq!(app.send("POST", "/auth/logout", &[("cookie", &cookie)], "").status, 200);
    assert_eq!(app.send("GET", "/auth/me", &[("cookie", &cookie)], "").status, 401);
}
