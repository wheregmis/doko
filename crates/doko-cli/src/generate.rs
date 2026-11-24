//! `doko generate ...` scaffolding.

use std::{fs, path::Path};

const MODULES_MARKER: &str = "// doko:modules";
const ROUTES_MARKER: &str = "// doko:routes";

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self",
    "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while", "abstract",
    "become", "box", "do", "final", "macro", "override", "priv", "try", "typeof", "unsized", "virtual", "yield",
];

const CONTROLLERS_MOD: &str = "\
//! Controllers. `doko generate controller <name>` adds to this file; keep the doko: markers.

use doko::{Ctx, axum::Router};

// doko:modules

pub fn routes() -> Router<Ctx> {
    Router::new()
    // doko:routes
}
";

const JOBS_MOD: &str = "\
//! Background jobs. `doko generate job <name>` adds to this file; keep the doko: markers.

use doko::Jobs;

// doko:modules

pub fn jobs() -> Jobs {
    Jobs::new()
    // doko:jobs
}
";

const MAILERS_MOD: &str = "\
//! Mailers. `doko generate mailer <name>` adds to this file; keep the doko: marker.

// doko:modules
";

/// Writes `src/mailers/<name>.rs` and declares it in `src/mailers/mod.rs`.
pub fn mailer(root: &Path, name: &str) -> Result<Vec<String>, String> {
    check_snake("mailer", name)?;
    add_module(root, "mailers", MAILERS_MOD, name, &mailer_template(name), "", "")
}

/// Writes `src/controllers/<name>.rs` and registers it in `src/controllers/mod.rs`.
/// Returns the paths it created or changed.
pub fn controller(root: &Path, name: &str) -> Result<Vec<String>, String> {
    check_snake("controller", name)?;
    add_module(root, "controllers", CONTROLLERS_MOD, name, &controller_template(name), ROUTES_MARKER, &format!(".merge({name}::routes())"))
}

/// Writes `src/jobs/<name>.rs` and registers it in `src/jobs/mod.rs`.
pub fn job(root: &Path, name: &str) -> Result<Vec<String>, String> {
    check_snake("job", name)?;
    let ty = camel(name);
    add_module(root, "jobs", JOBS_MOD, name, &job_template(name, &ty), "// doko:jobs", &format!(".add::<{name}::{ty}>()"))
}

fn check_snake(kind: &str, name: &str) -> Result<(), String> {
    let valid = name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !valid {
        return Err(format!("{kind} name `{name}` must be snake_case, e.g. `blog_posts`"));
    }
    if RUST_KEYWORDS.contains(&name) {
        return Err(format!("{kind} name `{name}` is a reserved Rust keyword"));
    }
    Ok(())
}

fn camel(name: &str) -> String {
    name.split('_').filter(|p| !p.is_empty()).map(|p| p[..1].to_ascii_uppercase() + &p[1..]).collect()
}

/// Writes `src/<dir>/<name>.rs` and adds `pub mod <name>;` plus `item` before `item_marker` in its mod.rs.
/// An empty `item_marker` means the module has nothing else to register.
fn add_module(
    root: &Path,
    dir_name: &str,
    default_mod: &str,
    name: &str,
    body: &str,
    item_marker: &str,
    item: &str,
) -> Result<Vec<String>, String> {
    let single_file = root.join("src").join(format!("{dir_name}.rs"));
    if single_file.exists() {
        return Err(format!(
            "src/{dir_name}.rs exists; the generator needs src/{dir_name}/mod.rs instead (move it there and add the doko: markers)"
        ));
    }
    let dir = root.join("src").join(dir_name);
    let file = dir.join(format!("{name}.rs"));
    if file.exists() {
        return Err(format!("{} already exists", file.display()));
    }
    let mod_rs = dir.join("mod.rs");
    let current = match fs::read_to_string(&mod_rs) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => default_mod.to_owned(),
        Err(e) => return Err(format!("reading {}: {e}", mod_rs.display())),
    };
    if !current.contains(MODULES_MARKER) || !current.contains(item_marker) {
        return Err(format!(
            "{} is missing the `{MODULES_MARKER}` / `{item_marker}` markers; add `pub mod {name};` and `{item}` by hand",
            mod_rs.display()
        ));
    }
    let mut updated = current.replacen(MODULES_MARKER, &format!("pub mod {name};\n{MODULES_MARKER}"), 1);
    if !item_marker.is_empty() {
        updated = updated.replacen(item_marker, &format!("{item}\n    {item_marker}"), 1);
    }

    fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    fs::write(&file, body).map_err(|e| format!("writing {}: {e}", file.display()))?;
    if let Err(e) = fs::write(&mod_rs, updated) {
        let _ = fs::remove_file(&file);
        return Err(format!("writing {}: {e}", mod_rs.display()));
    }
    Ok(vec![file.display().to_string(), mod_rs.display().to_string()])
}

/// `doko new`: a standalone app in `root/<name>`, with wrangler.toml already generated.
pub fn app(root: &Path, name: &str) -> Result<Vec<String>, String> {
    if !crate::config::is_valid_app_name(name) {
        return Err(format!(
            "app name `{name}` must be lowercase letters, digits and `-` (cannot end with `-`), e.g. `my-app`"
        ));
    }
    let dir = root.join(name);
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    let doko_toml = format!("[app]\nname = \"{name}\"\ncompatibility_date = \"{COMPAT_DATE}\"\n");
    let wrangler = crate::config::render(&doko_toml)?.wrangler_toml;
    let files = [
        ("Cargo.toml", app_cargo_toml(name)),
        ("doko.toml", doko_toml),
        ("wrangler.toml", wrangler),
        (".gitignore", "/target\nbuild/\nnode_modules/\n.wrangler/\n.dev.vars\n".into()),
        ("src/lib.rs", APP_LIB.into()),
        ("src/controllers/mod.rs", CONTROLLERS_MOD.into()),
        ("tests/smoke.rs", APP_SMOKE_TEST.into()),
    ];
    let write_files = || -> Result<Vec<String>, String> {
        let mut written = vec![];
        for (path, body) in files {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).map_err(|e| format!("creating {}: {e}", dir.display()))?;
            fs::write(&path, body).map_err(|e| format!("writing {}: {e}", path.display()))?;
            written.push(path.display().to_string());
        }
        Ok(written)
    };
    match write_files() {
        Ok(written) => Ok(written),
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

const COMPAT_DATE: &str = "2026-11-01";
const DOKO_GIT: &str = "https://github.com/wheregmis/doko";

fn app_cargo_toml(name: &str) -> String {
    format!(
        "\
[package]
name = \"{name}\"
version = \"0.1.0\"
edition = \"2024\"
publish = false

[lib]
crate-type = [\"cdylib\"]

[dependencies]
doko = {{ git = \"{DOKO_GIT}\" }}
# required: worker's #[event] macro expands to ::worker paths
worker = {{ version = \"0.9\", features = [\"http\", \"axum\", \"d1\", \"queue\"] }}
serde = {{ version = \"1\", features = [\"derive\"] }}

[dev-dependencies]
doko = {{ git = \"{DOKO_GIT}\", features = [\"test\"] }}

[profile.release]
lto = true
strip = true
codegen-units = 1
"
    )
}

const APP_LIB: &str = "\
mod controllers;

use doko::{
    Ctx, Hooks,
    axum::{Router, routing::get},
};

struct App;

impl Hooks for App {
    fn routes() -> Router<Ctx> {
        Router::new()
            .route(\"/health\", get(|| async { \"ok\" }))
            .merge(controllers::routes())
    }
}

doko::app!(App);
";

const APP_SMOKE_TEST: &str = "\
//! Runs against `wrangler dev`: `cargo test -- --ignored` (needs Node).

#[test]
#[ignore = \"starts wrangler dev\"]
fn health() {
    let app = doko::test::App::start(&[]);
    let res = app.get(\"/health\");
    assert_eq!(res.status, 200);
    assert_eq!(res.body, \"ok\");
}
";

fn mailer_template(name: &str) -> String {
    let subject = name.replace('_', " ");
    format!(
        "\
use doko::Message;

/// Send with `ctx.mail(EMAIL)?.send(&{name}::message(to)).await?`.
pub fn message(to: &str) -> Message {{
    Message {{
        // Must be on a domain onboarded to Email Sending.
        from: \"hello@example.com\".into(),
        to: vec![to.into()],
        subject: \"{subject}\".into(),
        text: Some(\"Hello!\\n\".into()),
        // Add `html` too for richer mail, escaping anything a user typed.
        ..Default::default()
    }}
}}
"
    )
}

fn job_template(name: &str, ty: &str) -> String {
    format!(
        "\
use doko::{{Ctx, Job, Result}};
use serde::{{Deserialize, Serialize}};

/// Enqueue with `ctx.enqueue(QUEUE, &{ty} {{ .. }})`.
#[derive(Serialize, Deserialize)]
pub struct {ty} {{}}

impl Job for {ty} {{
    /// Stored in every queued message: renaming it strands jobs already queued.
    const NAME: &'static str = \"{name}\";

    async fn perform(self, _ctx: Ctx) -> Result<()> {{
        Ok(())
    }}
}}
"
    )
}

fn controller_template(name: &str) -> String {
    let path = name.replace('_', "-");
    format!(
        "\
use doko::{{
    Ctx, Error, Result,
    axum::{{Json, Router, extract::Path, routing::get}},
}};

pub fn routes() -> Router<Ctx> {{
    Router::new()
        .route(\"/{path}\", get(index))
        .route(\"/{path}/{{id}}\", get(show))
}}

async fn index(_ctx: Ctx) -> Result<Json<Vec<String>>> {{
    Ok(Json(vec![]))
}}

async fn show(_ctx: Ctx, Path(_id): Path<String>) -> Result<Json<String>> {{
    Err(Error::not_found())
}}
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn tmp(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("doko-gen-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        TempDir(dir)
    }

    #[test]
    fn generates_and_registers_controllers() {
        let root = tmp("ok");
        controller(&root.0, "posts").unwrap();
        controller(&root.0, "blog_posts").unwrap();
        let m = fs::read_to_string(root.0.join("src/controllers/mod.rs")).unwrap();
        assert!(m.contains("pub mod posts;\npub mod blog_posts;\n// doko:modules"), "{m}");
        assert!(m.contains(".merge(posts::routes())\n    .merge(blog_posts::routes())\n    // doko:routes"), "{m}");
        let c = fs::read_to_string(root.0.join("src/controllers/blog_posts.rs")).unwrap();
        assert!(c.contains("\"/blog-posts/{id}\""), "{c}");
        assert!(controller(&root.0, "posts").unwrap_err().contains("already exists"));
        fs::write(root.0.join("src/controllers.rs"), "").unwrap();
        assert!(controller(&root.0, "other").unwrap_err().contains("src/controllers.rs"));
    }

    #[test]
    fn generates_and_registers_jobs() {
        let root = tmp("job");
        job(&root.0, "send_welcome").unwrap();
        let m = fs::read_to_string(root.0.join("src/jobs/mod.rs")).unwrap();
        assert!(m.contains("pub mod send_welcome;\n// doko:modules"), "{m}");
        assert!(m.contains(".add::<send_welcome::SendWelcome>()\n    // doko:jobs"), "{m}");
        let j = fs::read_to_string(root.0.join("src/jobs/send_welcome.rs")).unwrap();
        assert!(j.contains("impl Job for SendWelcome") && j.contains("\"send_welcome\""), "{j}");
        fs::write(root.0.join("src/jobs.rs"), "").unwrap();
        assert!(job(&root.0, "other").unwrap_err().contains("src/jobs.rs"));
    }

    #[test]
    fn generates_and_declares_mailers() {
        let root = tmp("mailer");
        mailer(&root.0, "welcome").unwrap();
        mailer(&root.0, "password_reset").unwrap();
        let m = fs::read_to_string(root.0.join("src/mailers/mod.rs")).unwrap();
        assert!(m.ends_with("pub mod welcome;\npub mod password_reset;\n// doko:modules\n"), "{m}");
        let w = fs::read_to_string(root.0.join("src/mailers/password_reset.rs")).unwrap();
        assert!(w.contains("subject: \"password reset\".into()") && w.contains("text: Some(\"Hello!\\n\".into())"), "{w}");
        assert!(mailer(&root.0, "welcome").unwrap_err().contains("already exists"));
    }

    #[test]
    fn generates_apps() {
        let root = tmp("app");
        let files = app(&root.0, "my-app").unwrap();
        assert_eq!(files.len(), 7);
        let w = fs::read_to_string(root.0.join("my-app/wrangler.toml")).unwrap();
        assert!(w.contains("name = \"my-app\""), "{w}");
        assert!(app(&root.0, "my-app").unwrap_err().contains("already exists"));
        assert!(app(&root.0, "My_App").is_err());
        assert!(app(&root.0, "my-app-").is_err());
    }

    #[test]
    fn rejects_bad_names_keywords_and_missing_markers() {
        let root = tmp("bad");
        assert!(controller(&root.0, "Posts").is_err());
        assert!(controller(&root.0, "../x").is_err());
        assert!(controller(&root.0, "async").unwrap_err().contains("reserved Rust keyword"));
        assert!(job(&root.0, "type").unwrap_err().contains("reserved Rust keyword"));
        assert!(controller(&root.0, "mod").unwrap_err().contains("reserved Rust keyword"));
        fs::create_dir_all(root.0.join("src/controllers")).unwrap();
        fs::write(root.0.join("src/controllers/mod.rs"), "pub mod old;\n").unwrap();
        assert!(controller(&root.0, "posts").unwrap_err().contains("markers"));
        assert!(!root.0.join("src/controllers/posts.rs").exists(), "no partial writes");
    }
}
