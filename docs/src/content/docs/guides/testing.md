---
title: "Integration tests"
description: "Run your worker under wrangler dev and test it over HTTP."
---

`doko::test::App` runs your worker under `wrangler dev` (on a free port, with empty local state) and gives you an HTTP client. Enable it for tests only:

```toml
[dev-dependencies]
doko = { ..., features = ["test"] }
```

```rust
#[test]
#[ignore = "starts wrangler dev"]
fn creates_notes() {
    let app = doko::test::App::start(&["DB"]);   // applies DB's migrations first
    let res = app.post_json("/notes", &json!({ "body": "hi" }));
    assert_eq!(res.status, 201);
    let note: Note = res.json();
}
```

`app.send(method, path, headers, body)` sends any request, and `res.header("set-cookie")` reads a response header.

```bash
doko test                  # every test, including the ones that start wrangler dev
doko test --test auth      # arguments go to cargo test
```

`doko test` regenerates `wrangler.toml` and runs `cargo test -- --include-ignored`. It needs Node, and startup includes a release build, so share one `App` across the asserts in a test. Tests may run in parallel: each `App` has its own port and its own empty local state, and they start one at a time.

| Helper | What |
|---|---|
| `App::start_with_seeds(&["DB"], &[("DB", "tests/seeds/users.sql")])` | runs SQL files after the migrations, before the worker starts |
| `app.client()` | a client that keeps cookies between its requests, like one browser: log in through it and its later requests are signed in |
| `app.mail_to("ada@example.com")` | the next mail sent to that address, waiting up to 10 seconds: `from`, `to`, `subject`, `text`, `html` |
| `app.wait_for_job("count_words")` | waits up to 30 seconds until a job with that `Job::NAME` has finished |
| `app.output()` | everything the worker and wrangler printed, a line each, for anything else |

```rust
let browser = app.client();
browser.post_json("/auth/login", &credentials);
assert_eq!(browser.get("/auth/me").status, 200);

app.post_json("/auth/forgot-password", &json!({ "email": "ada@example.com" }));
let mail = app.mail_to("ada@example.com");
assert_eq!(mail.subject, "Reset your password");
```

`mail_to` and `wait_for_job` each return a mail or a job once, so a second call waits for the next one. Queues, D1, KV, Durable Objects, email and the Cache API all run locally. Postgres needs a database at the `local_connection_string`, and an app with `[ai]` needs `wrangler login` to start. See [examples/hello/tests/app.rs](https://github.com/wheregmis/doko/blob/master/examples/hello/tests/app.rs).
