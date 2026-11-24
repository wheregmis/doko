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
//!
//! Beyond requests: [`App::client`] keeps cookies like a browser, [`App::mail_to`] returns the mail
//! the worker sent, [`App::wait_for_job`] waits for a background job, and
//! [`App::start_with_seeds`] loads SQL files before the worker starts.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::{BufRead, BufReader, Read},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Serialize, de::DeserializeOwned};

const WRANGLER: &str = "wrangler@4";
const STARTUP: Duration = Duration::from_secs(600);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAIL_TIMEOUT: Duration = Duration::from_secs(10);
const JOB_TIMEOUT: Duration = Duration::from_secs(30);
static COUNTER: AtomicU64 = AtomicU64::new(0);
/// Tests run in parallel, but each start rebuilds the worker into the same `build/` directory.
static STARTING: Mutex<()> = Mutex::new(());

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
    /// Everything `wrangler dev` printed, with colors removed.
    output: Arc<Mutex<Vec<String>>>,
    /// How far into `output` [`mail_to`](Self::mail_to) and [`wait_for_job`](Self::wait_for_job) have read.
    mail_read: Mutex<usize>,
    jobs_read: Mutex<usize>,
}

/// A mail the worker sent through a `[email.*]` binding. Under `wrangler dev` it is recorded, not delivered.
#[derive(Debug, Clone, PartialEq)]
pub struct Mail {
    pub from: String,
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: String,
}

/// Requests that keep cookies between them, like one browser: `let ada = app.client();`.
/// After a login through it, later requests are signed in.
pub struct Client<'a> {
    app: &'a App,
    cookies: RefCell<BTreeMap<String, String>>,
}

impl Client<'_> {
    pub fn get(&self, path: &str) -> Response {
        self.send("GET", path, &[], "")
    }

    pub fn delete(&self, path: &str) -> Response {
        self.send("DELETE", path, &[], "")
    }

    pub fn post_json<T: Serialize>(&self, path: &str, body: &T) -> Response {
        self.send("POST", path, &[], &serde_json::to_string(body).expect("serializing body"))
    }

    /// Like [`App::send`], with this client's cookies added and the response's cookies kept.
    pub fn send(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Response {
        let cookie = self.cookies.borrow().iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
        let mut headers = headers.to_vec();
        if !cookie.is_empty() {
            headers.push(("cookie", &cookie));
        }
        let res = self.app.send(method, path, &headers, body);
        store_cookies(&mut self.cookies.borrow_mut(), &res.headers);
        res
    }
}

/// Applies a response's `set-cookie` headers to a jar. An empty value or `Max-Age=0` removes the cookie.
fn store_cookies(jar: &mut BTreeMap<String, String>, headers: &[(String, String)]) {
    for (_, cookie) in headers.iter().filter(|(name, _)| name == "set-cookie") {
        let mut parts = cookie.split(';').map(str::trim);
        let Some((name, value)) = parts.next().and_then(|pair| pair.split_once('=')) else { continue };
        if value.is_empty() || parts.any(|attr| attr.eq_ignore_ascii_case("max-age=0")) {
            jar.remove(name);
        } else {
            jar.insert(name.to_owned(), value.to_owned());
        }
    }
}

fn strip_colors(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // An escape sequence ends at its first letter.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A mail as `wrangler dev` announces it: the headers, and the files holding the bodies.
#[derive(Debug, PartialEq)]
struct MailLog {
    /// The output line after the announcement.
    end: usize,
    from: String,
    to: String,
    subject: String,
    text_file: String,
    html_file: String,
}

// shortcut: this reads wrangler's own log format for `send_email`, which is not a stable interface;
// if a wrangler release changes it, `mail_to` times out with the output it saw.
fn parse_mails(lines: &[String]) -> Vec<MailLog> {
    let mut mails = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].contains("send_email binding called") {
            i += 1;
            continue;
        }
        let mut fields = BTreeMap::new();
        i += 1;
        while i < lines.len() && !lines[i].contains("send_email binding called") && !fields.contains_key("HTML") {
            if let Some((name, value)) = lines[i].split_once(": ") {
                fields.entry(name.trim().to_owned()).or_insert_with(|| value.trim().to_owned());
            }
            i += 1;
        }
        let mut take = |name: &str| fields.remove(name).unwrap_or_default();
        mails.push(MailLog {
            end: i,
            from: take("From"),
            to: take("To"),
            subject: take("Subject"),
            text_file: take("Text"),
            html_file: take("HTML"),
        });
    }
    mails
}

/// The job name in one of doko's job log lines, if the job finished.
fn finished_job(line: &str) -> Option<String> {
    let json = line.get(line.find('{')?..=line.rfind('}')?)?;
    let log: serde_json::Value = serde_json::from_str(json).ok()?;
    (log["status"] == "done").then(|| log["job"].as_str().map(str::to_owned))?
}

/// A response: status, headers and body as text.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    /// The first header called `name` (lowercase), e.g. `res.header("set-cookie")`.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// Parses the body as JSON. Panics if it isn't `T`.
    pub fn json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("response is not the expected JSON ({e}): {}", self.body))
    }
}

impl App {
    /// Applies migrations for each D1 binding in `migrate`, then starts `wrangler dev` and waits
    /// until it answers. Runs in the crate being tested, using its wrangler.toml. Panics on failure.
    pub fn start(migrate: &[&str]) -> Self {
        Self::start_with_seeds(migrate, &[])
    }

    /// Like [`start`](Self::start), then runs SQL files against D1 bindings before the worker
    /// starts: `App::start_with_seeds(&["DB"], &[("DB", "tests/seeds/users.sql")])`. Paths are
    /// relative to the crate.
    pub fn start_with_seeds(migrate: &[&str], seeds: &[(&str, &str)]) -> Self {
        // A test that panicked while starting must not block the others.
        let _starting = STARTING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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
        for (db, file) in seeds {
            let status = wrangler(&root, &["d1", "execute", db, "--local", "--yes", "--file", file, "--persist-to"])
                .arg(state_path)
                .status()
                .expect("running npx wrangler");
            assert!(status.success(), "seeding {db} from {file} failed: {status}");
        }

        let child = {
            let mut cmd = wrangler(&root, &["dev", "--ip", "127.0.0.1", "--port", &port.to_string(), "--persist-to"]);
            cmd.arg(state_path);
            #[cfg(unix)]
            std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
            cmd.spawn().expect("running npx wrangler dev")
        };
        let mut child = child;
        let output = Arc::new(Mutex::new(Vec::new()));
        // Still printed, so a failing test shows the worker's logs.
        echo_and_keep(child.stdout.take().expect("piped stdout"), output.clone());
        echo_and_keep(child.stderr.take().expect("piped stderr"), output.clone());
        let state = state_guard.0.take().expect("state dir present");
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build()
            .into();
        let base = format!("http://127.0.0.1:{port}");
        let mut app = App { child, state, base, agent, output, mail_read: Mutex::new(0), jobs_read: Mutex::new(0) };

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

    /// A client that keeps cookies between its requests.
    pub fn client(&self) -> Client<'_> {
        Client { app: self, cookies: RefCell::new(BTreeMap::new()) }
    }

    /// Everything `wrangler dev` has printed so far, a line each: the worker's own logs included.
    pub fn output(&self) -> Vec<String> {
        self.output.lock().expect("output lock").clone()
    }

    /// The next mail sent to `to` that an earlier call hasn't returned, waiting up to 10 seconds
    /// for it. Panics if none arrives. `to` matches when the recipients contain it.
    pub fn mail_to(&self, to: &str) -> Mail {
        let mut read = self.mail_read.lock().expect("mail lock");
        let started = Instant::now();
        loop {
            let found = parse_mails(&self.output()).into_iter().find(|m| m.end > *read && m.to.contains(to));
            // The announcement is printed before the body files are complete.
            if let Some(log) = found
                && let (Ok(text), Ok(html)) =
                    (std::fs::read_to_string(&log.text_file), std::fs::read_to_string(&log.html_file))
            {
                *read = log.end;
                return Mail { from: log.from, to: log.to, subject: log.subject, text, html };
            }
            assert!(started.elapsed() < MAIL_TIMEOUT, "no new mail to {to} within {MAIL_TIMEOUT:?}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Waits up to 30 seconds until a job called `name` (its `Job::NAME`) has finished that an
    /// earlier call hasn't counted. Panics if none does: a job that keeps failing never finishes.
    pub fn wait_for_job(&self, name: &str) {
        let mut read = self.jobs_read.lock().expect("jobs lock");
        let started = Instant::now();
        loop {
            let output = self.output();
            let done =
                output.iter().enumerate().skip(*read).find(|(_, line)| finished_job(line).as_deref() == Some(name));
            if let Some((at, _)) = done {
                *read = at + 1;
                return;
            }
            assert!(started.elapsed() < JOB_TIMEOUT, "job `{name}` did not finish within {JOB_TIMEOUT:?}");
            std::thread::sleep(Duration::from_millis(100));
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
        self.send("GET", path, &[], "")
    }

    pub fn delete(&self, path: &str) -> Response {
        self.send("DELETE", path, &[], "")
    }

    pub fn post_json<T: Serialize>(&self, path: &str, body: &T) -> Response {
        self.send("POST", path, &[], &serde_json::to_string(body).expect("serializing body"))
    }

    /// Any request, with headers: `app.send("GET", "/me", &[("cookie", cookie)], "")`.
    /// A non-empty `body` is sent as JSON unless `headers` set a `content-type`.
    pub fn send(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Response {
        let mut req = ureq::http::Request::builder().method(method).uri(self.url_for(path));
        if !body.is_empty() && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
            req = req.header("content-type", "application/json");
        }
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        let req = req.body(body.to_owned()).unwrap_or_else(|e| panic!("building request to {path}: {e}"));
        let mut res = self.agent.run(req).unwrap_or_else(|e| panic!("request to {path} failed: {e}"));
        let body = res.body_mut().read_to_string().unwrap_or_else(|e| panic!("reading {path} body: {e}"));
        let headers = res
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        Response { status: res.status().as_u16(), headers, body }
    }
}

/// Copies a child's output to this process's stderr and into `lines`, on its own thread.
fn echo_and_keep(stream: impl Read + Send + 'static, lines: Arc<Mutex<Vec<String>>>) {
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            eprintln!("{line}");
            lines.lock().expect("output lock").push(strip_colors(&line));
        }
    });
}

fn join_url(base: &str, path: &str) -> String {
    if path.starts_with('/') { format!("{base}{path}") } else { format!("{base}/{path}") }
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
    fn keeps_and_drops_cookies() {
        let mut jar = BTreeMap::new();
        let set = |v: &str| vec![("set-cookie".to_owned(), v.to_owned()), ("x-other".to_owned(), "a=b".to_owned())];
        store_cookies(&mut jar, &set("sid=abc.def; HttpOnly; Path=/"));
        store_cookies(&mut jar, &set("theme=dark"));
        assert_eq!(
            jar,
            BTreeMap::from([("sid".to_owned(), "abc.def".to_owned()), ("theme".to_owned(), "dark".to_owned())])
        );
        store_cookies(&mut jar, &set("sid=gone; Max-Age=0; Path=/"));
        store_cookies(&mut jar, &set("theme=; Path=/"));
        assert!(jar.is_empty(), "{jar:?}");
    }

    #[test]
    fn reads_mail_and_jobs_from_wrangler_output() {
        let lines: Vec<String> = [
            "\u{1b}[32m[wrangler:info]\u{1b}[0m send_email binding called with MessageBuilder:",
            "From: \"Accounts\" <accounts@example.com>",
            "To: ada@example.com",
            "Subject: Re: your order: shipped",
            "",
            "Text: /tmp/a.txt",
            "HTML: /tmp/a.html",
            "{\"level\":\"info\",\"job\":\"count_words\",\"attempt\":1,\"status\":\"done\"}",
            "\u{1b}[31m✘ [ERROR]\u{1b}[0m {\"level\":\"error\",\"job\":\"count_words\",\"attempt\":2,\"error\":\"boom\"}",
            "[wrangler:info] send_email binding called with MessageBuilder:",
            "To: bob@example.com",
        ]
        .iter()
        .map(|l| strip_colors(l))
        .collect();
        let mails = parse_mails(&lines);
        assert_eq!(mails.len(), 2);
        assert_eq!(
            (mails[0].to.as_str(), mails[0].subject.as_str(), mails[0].end),
            ("ada@example.com", "Re: your order: shipped", 7)
        );
        assert_eq!((mails[0].text_file.as_str(), mails[0].html_file.as_str()), ("/tmp/a.txt", "/tmp/a.html"));
        assert_eq!((mails[1].to.as_str(), mails[1].text_file.as_str()), ("bob@example.com", ""), "still being printed");
        assert_eq!(finished_job(&lines[7]).as_deref(), Some("count_words"));
        assert_eq!(finished_job(&lines[8]), None, "a failed attempt is not a finished job");
        assert_eq!(finished_job("GET /notes 200"), None);
    }

    #[test]
    fn parses_json_response() {
        let res = Response { status: 200, headers: vec![("x-a".into(), "1".into())], body: r#"{"ok":true}"#.into() };
        assert_eq!((res.header("x-a"), res.header("x-b")), (Some("1"), None));
        let val: serde_json::Value = res.json();
        assert_eq!(val["ok"], true);
    }
}
