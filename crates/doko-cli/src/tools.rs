//! `doko routes` and `doko doctor`: looking at an app without running it.

use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
};

use crate::config::Output;

const METHODS: [&str; 8] = ["get", "post", "put", "patch", "delete", "head", "options", "any"];

#[derive(Debug, PartialEq)]
pub struct Route {
    pub methods: Vec<&'static str>,
    pub path: String,
    /// `file:line`
    pub at: String,
}

/// The `.route("/path", get(..).post(..))` calls in one source file.
// shortcut: this reads the source, it doesn't run the router, so a path under `.nest("/api", ..)`
// is listed without its prefix and routes built in a loop or a macro are missed.
pub fn routes_in(file: &str, src: &str) -> Vec<Route> {
    let mut routes = Vec::new();
    let mut from = 0;
    while let Some(found) = src[from..].find(".route(") {
        let start = from + found + ".route(".len();
        from = start;
        let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
        if src[line_start..start].trim_start().starts_with("//") {
            continue;
        }
        // The call's arguments: up to the parenthesis that closes it, skipping string contents.
        let (mut depth, mut in_string, mut escaped, mut end) = (1, false, false, src.len());
        for (i, c) in src[start..].char_indices() {
            match (in_string, c) {
                (true, _) if escaped => escaped = false,
                (true, '\\') => escaped = true,
                (_, '"') => in_string = !in_string,
                (false, '(') => depth += 1,
                (false, ')') => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let args = &src[start..end];
        let Some(path) = args.trim_start().strip_prefix('"').and_then(|rest| rest.split('"').next()) else { continue };
        let handlers = &args[args.find(',').map_or(args.len(), |i| i + 1)..];
        let calls = |method: &str| {
            handlers
                .match_indices(&format!("{method}("))
                .any(|(i, _)| !handlers[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_'))
        };
        let methods = METHODS.into_iter().filter(|m| calls(m)).collect();
        let line = src[..start].matches('\n').count() + 1;
        routes.push(Route { methods, path: path.to_owned(), at: format!("{file}:{line}") });
    }
    routes
}

/// Every route under `src/`, in file order, plus the one doko adds.
pub fn routes(root: &Path) -> Result<Vec<Route>, String> {
    fn walk(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
        let mut entries: Vec<_> = fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, files);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&root.join("src"), &mut files);
    if files.is_empty() {
        return Err("no src/*.rs here: run this in an app's directory".into());
    }
    let mut routes = Vec::new();
    for file in files {
        let src = fs::read_to_string(&file).map_err(|e| format!("reading {}: {e}", file.display()))?;
        let shown = file.strip_prefix(root).unwrap_or(&file).display().to_string();
        routes.extend(routes_in(&shown, &src));
    }
    routes.push(Route { methods: vec!["get"], path: "/_health".into(), at: "(doko)".into() });
    Ok(routes)
}

pub fn print_routes(routes: &[Route]) {
    let width = routes.iter().map(|r| r.path.len()).max().unwrap_or(0);
    for r in routes {
        let methods = if r.methods.is_empty() { "?".to_owned() } else { r.methods.join(",").to_uppercase() };
        println!("{methods:<18} {:<width$}  {}", r.path, r.at);
    }
}

/// The names set in a `.dev.vars` file.
pub fn dev_vars(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| Some(l.split_once('=')?.0.trim()))
        .collect()
}

/// The first line a command prints, if it runs and succeeds.
fn first_line(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().trim().to_owned())
}

/// The secrets set on the deployed worker for `env`, or `None` if they can't be listed (not
/// logged in, or the worker was never deployed).
pub fn deployed_secrets(env: &str) -> Option<String> {
    let mut args = vec!["--yes", crate::WRANGLER, "secret", "list"];
    if env != "dev" {
        args.extend(["--env", env]);
    }
    let out = Command::new("npx").args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The names in `required` that `wrangler secret list`'s JSON doesn't mention.
pub fn missing_secrets<'a>(required: &'a [String], listed: &str) -> Vec<&'a str> {
    required.iter().filter(|name| !listed.contains(&format!("\"{name}\""))).map(String::as_str).collect()
}

/// `doko doctor`: prints one line per check and returns how many failed. `env` adds the checks
/// that ask Cloudflare about that environment.
pub fn doctor(root: &Path, config: Result<&Output, &str>, env: Option<&str>) -> usize {
    let mut failed = 0;
    let mut check = |name: &str, result: Result<String, String>| match result {
        Ok(detail) => println!("ok       {name}: {detail}"),
        Err(fix) => {
            failed += 1;
            println!("PROBLEM  {name}: {fix}");
        }
    };
    check(
        "node",
        first_line("node", &["--version"])
            .ok_or_else(|| "not found; wrangler needs Node 20 or newer: https://nodejs.org".into()),
    );
    check("npx", first_line("npx", &["--version"]).ok_or_else(|| "not found; it ships with Node".into()));
    check(
        "wasm target",
        match first_line("rustup", &["target", "list", "--installed"]) {
            None => Ok("rustup not found, so not checked".into()),
            Some(_)
                if Command::new("rustup")
                    .args(["target", "list", "--installed"])
                    .output()
                    .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("wasm32-unknown-unknown")) =>
            {
                Ok("wasm32-unknown-unknown is installed".into())
            }
            Some(_) => Err("run `rustup target add wasm32-unknown-unknown`".into()),
        },
    );
    let out = match config {
        Ok(out) => {
            check("doko.toml", Ok("valid".into()));
            out
        }
        Err(e) => {
            check("doko.toml", Err(e.to_owned()));
            return failed;
        }
    };
    if !out.hyperdrive.is_empty() {
        check(
            "psql",
            first_line("psql", &["--version"]).ok_or_else(|| "not found; `doko migrate --pg` needs it".into()),
        );
    }
    let dev = fs::read_to_string(root.join(".dev.vars")).unwrap_or_default();
    let local = dev_vars(&dev);
    for secret in &out.secrets {
        let set = local.contains(&secret.as_str());
        check(
            &format!("secret {secret} (local)"),
            if set { Ok("set in .dev.vars".into()) } else { Err(format!("add `{secret}=..` to .dev.vars")) },
        );
    }
    for (name, bindings) in out.hyperdrive_without_id.iter().filter(|(name, _)| *name != "dev") {
        let result = if bindings.is_empty() {
            Ok("every Hyperdrive binding has an id".to_owned())
        } else {
            Err(format!(
                "[env.{name}.hyperdrive.{}] needs the `id` from `wrangler hyperdrive create`",
                bindings.join("|")
            ))
        };
        check(&format!("env {name}"), result);
    }
    if let Some(env) = env {
        let flag = if env == "dev" { String::new() } else { format!(" --env {env}") };
        match deployed_secrets(env) {
            None => check(
                &format!("secrets ({env})"),
                Err("could not list them: run `wrangler login`, or deploy this env once".into()),
            ),
            Some(listed) => {
                let missing = missing_secrets(&out.secrets, &listed);
                let result = if missing.is_empty() {
                    Ok(format!("{} required, all set", out.secrets.len()))
                } else {
                    Err(missing
                        .iter()
                        .map(|s| format!("`wrangler secret put {s}{flag}`"))
                        .collect::<Vec<_>>()
                        .join(", "))
                };
                check(&format!("secrets ({env})"), result);
            }
        }
    } else if !out.secrets.is_empty() {
        println!("note     deployed secrets are checked with `doko doctor --env <name>`");
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_routes_in_source() {
        let src = r#"
pub fn routes() -> Router<Ctx> {
    Router::new()
        .route("/notes", get(index).post(create))
        // .route("/old", get(old))
        .route(
            "/notes/{id}",
            get(show).put(update).delete(destroy),
        )
        .route("/boom", get(|| async { Err::<(), _>(Error::internal("a (broken) basket")) }))
        .route("/ws", any(upgrade))
        .route(PATH, target(handler))
}
"#;
        let found = routes_in("src/c.rs", src);
        let brief: Vec<(String, &str, &str)> =
            found.iter().map(|r| (r.methods.join(","), r.path.as_str(), r.at.as_str())).collect();
        assert_eq!(
            brief,
            [
                ("get,post".to_owned(), "/notes", "src/c.rs:4"),
                ("get,put,delete".to_owned(), "/notes/{id}", "src/c.rs:6"),
                ("get".to_owned(), "/boom", "src/c.rs:10"),
                ("any".to_owned(), "/ws", "src/c.rs:11"),
            ],
            "a commented-out route and one without a literal path are skipped; `target(` is not `get(`"
        );
    }

    #[test]
    fn reads_dev_vars_and_missing_secrets() {
        assert_eq!(
            dev_vars("# local only\nSESSION_SECRET=abc=def\n\n  API_TOKEN = x\n#OLD=1\n"),
            ["SESSION_SECRET", "API_TOKEN"]
        );
        let required = ["SESSION_SECRET".to_owned(), "STRIPE_KEY".to_owned()];
        assert_eq!(missing_secrets(&required, r#"[{"name":"SESSION_SECRET","type":"secret_text"}]"#), ["STRIPE_KEY"]);
        assert!(missing_secrets(&[], "[]").is_empty());
    }
}
