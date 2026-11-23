//! Integration tests against a real local worker (`wrangler dev`), behind the `test` feature.
//!
//! ```ignore
//! #[test]
//! #[ignore = "starts wrangler dev"]
//! fn creates_notes() {
//!     let app = doko::test::App::start(&["DB"]); // D1 bindings to migrate first
//!     let res = app.post_json("/notes", &serde_json::json!({ "body": "hi" }));
//!     assert_eq!(res.status, 201);
//! }
//! ```
//!
//! Each [`App`] builds the worker, runs it on a free port with its own empty local state, and stops
//! it on drop. Startup includes a release build, so share one `App` across the asserts in a test.

use std::{
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use serde::{Serialize, de::DeserializeOwned};

const WRANGLER: &str = "wrangler@4";
const STARTUP: Duration = Duration::from_secs(600);

/// A `wrangler dev` process for the crate under test. Stopped when dropped.
pub struct App {
    child: Child,
    state: PathBuf,
    base: String,
    agent: ureq::Agent,
}

/// A response: status and body as text.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

impl Response {
    /// Parses the body as JSON. Panics if it isn't `T`.
    pub fn json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("response is not the expected JSON ({e}): {}", self.body))
    }
}

impl App {
    /// Applies migrations for each D1 binding in `migrate`, then starts `wrangler dev` and waits
    /// until it answers. Runs in the crate being tested, using its wrangler.toml. Panics on failure.
    pub fn start(migrate: &[&str]) -> Self {
        let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("run under cargo test"));
        let port = TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).expect("no free port").port();
        let state = std::env::temp_dir().join(format!("doko-test-{}-{port}", std::process::id()));

        for db in migrate {
            let state = state.to_str().unwrap();
            let status = wrangler(&root, &["d1", "migrations", "apply", db, "--local", "--persist-to", state])
                .status()
                .expect("running npx wrangler");
            assert!(status.success(), "migrating {db} failed: {status}");
        }

        let child = {
            let mut cmd = wrangler(&root, &["dev", "--ip", "127.0.0.1", "--port", &port.to_string()]);
            cmd.args(["--persist-to", state.to_str().unwrap()]);
            #[cfg(unix)]
            std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
            cmd.spawn().expect("running npx wrangler dev")
        };
        let agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let mut app = App { child, state, base: format!("http://127.0.0.1:{port}"), agent };

        let started = Instant::now();
        loop {
            if let Some(status) = app.child.try_wait().expect("checking wrangler dev") {
                panic!("wrangler dev exited before it was ready: {status}");
            }
            if app.agent.get(&app.base).call().is_ok() {
                return app;
            }
            assert!(started.elapsed() < STARTUP, "wrangler dev not ready after {STARTUP:?}");
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// The base URL, e.g. `http://127.0.0.1:61234`.
    pub fn url(&self) -> &str {
        &self.base
    }

    pub fn get(&self, path: &str) -> Response {
        self.read(self.agent.get(format!("{}{path}", self.base)).call(), path)
    }

    pub fn delete(&self, path: &str) -> Response {
        self.read(self.agent.delete(format!("{}{path}", self.base)).call(), path)
    }

    pub fn post_json<T: Serialize>(&self, path: &str, body: &T) -> Response {
        let req = self.agent.post(format!("{}{path}", self.base)).header("content-type", "application/json");
        self.read(req.send(serde_json::to_string(body).expect("serializing body")), path)
    }

    fn read(&self, res: Result<ureq::http::Response<ureq::Body>, ureq::Error>, path: &str) -> Response {
        let mut res = res.unwrap_or_else(|e| panic!("request to {path} failed: {e}"));
        let body = res.body_mut().read_to_string().unwrap_or_else(|e| panic!("reading {path} body: {e}"));
        Response { status: res.status().as_u16(), body }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // npx starts wrangler, which starts workerd: stop the whole process group, not just npx.
        #[cfg(unix)]
        let _ = Command::new("kill").args(["-TERM", &format!("-{}", self.child.id())]).status();
        // shortcut: on Windows only npx is killed and workerd may linger; use a job object if that matters.
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.state);
    }
}

fn wrangler(root: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("npx");
    cmd.current_dir(root).args(["--yes", WRANGLER]).args(args).stdin(Stdio::null());
    cmd
}
