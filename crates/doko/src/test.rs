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
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use serde::{Serialize, de::DeserializeOwned};

const WRANGLER: &str = "wrangler@4";
const STARTUP: Duration = Duration::from_secs(600);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
static COUNTER: AtomicU64 = AtomicU64::new(0);

struct StateDir(Option<PathBuf>);

impl Drop for StateDir {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

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
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut state_guard =
            StateDir(Some(std::env::temp_dir().join(format!("doko-test-{}-{port}-{seq}", std::process::id()))));
        let state_path = state_guard.0.as_ref().expect("state dir present");

        for db in migrate {
            let status = wrangler(&root, &["d1", "migrations", "apply", db, "--local", "--persist-to"])
                .arg(state_path)
                .status()
                .expect("running npx wrangler");
            assert!(status.success(), "migrating {db} failed: {status}");
        }

        let child = {
            let mut cmd = wrangler(&root, &["dev", "--ip", "127.0.0.1", "--port", &port.to_string(), "--persist-to"]);
            cmd.arg(state_path);
            #[cfg(unix)]
            std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
            cmd.spawn().expect("running npx wrangler dev")
        };
        let state = state_guard.0.take().expect("state dir present");
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build()
            .into();
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

    fn url_for(&self, path: &str) -> String {
        join_url(&self.base, path)
    }

    pub fn get(&self, path: &str) -> Response {
        self.read(self.agent.get(self.url_for(path)).call(), path)
    }

    pub fn delete(&self, path: &str) -> Response {
        self.read(self.agent.delete(self.url_for(path)).call(), path)
    }

    pub fn post_json<T: Serialize>(&self, path: &str, body: &T) -> Response {
        let req = self.agent.post(self.url_for(path)).header("content-type", "application/json");
        self.read(req.send(serde_json::to_string(body).expect("serializing body")), path)
    }

    fn read(&self, res: Result<ureq::http::Response<ureq::Body>, ureq::Error>, path: &str) -> Response {
        let mut res = res.unwrap_or_else(|e| panic!("request to {path} failed: {e}"));
        let body = res.body_mut().read_to_string().unwrap_or_else(|e| panic!("reading {path} body: {e}"));
        Response { status: res.status().as_u16(), body }
    }
}

fn join_url(base: &str, path: &str) -> String {
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // npx starts wrangler, which starts workerd: stop the whole process group, not just npx.
        #[cfg(unix)]
        let _ = Command::new("kill").args(["-TERM", &format!("-{}", self.child.id())]).status();
        #[cfg(not(unix))]
        let _ = self.child.kill();

        let mut exited = false;
        for _ in 0..40 {
            if matches!(self.child.try_wait(), Ok(Some(_)) | Err(_)) {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !exited {
            // shortcut: on Windows only npx is killed and workerd may linger; use a job object if that matters.
            #[cfg(unix)]
            let _ = Command::new("kill").args(["-KILL", &format!("-{}", self.child.id())]).status();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.state);
    }
}

fn wrangler(root: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("npx");
    cmd.current_dir(root).args(["--yes", WRANGLER]).args(args).stdin(Stdio::null());
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_paths() {
        assert_eq!(join_url("http://127.0.0.1:8787", "/notes"), "http://127.0.0.1:8787/notes");
        assert_eq!(join_url("http://127.0.0.1:8787", "notes"), "http://127.0.0.1:8787/notes");
        assert_eq!(join_url("http://127.0.0.1:8787", ""), "http://127.0.0.1:8787/");
    }

    #[test]
    fn parses_json_response() {
        let res = Response { status: 200, body: r#"{"ok":true}"#.into() };
        let val: serde_json::Value = res.json();
        assert_eq!(val["ok"], true);
    }
}
