//! `doko generate ...` scaffolding.

use std::{fs, path::Path};

const MODULES_MARKER: &str = "// doko:modules";
const ROUTES_MARKER: &str = "// doko:routes";

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

/// Writes `src/controllers/<name>.rs` and registers it in `src/controllers/mod.rs`.
/// Returns the paths it created or changed.
pub fn controller(root: &Path, name: &str) -> Result<Vec<String>, String> {
    check_snake("controller", name)?;
    add_module(root, "controllers", CONTROLLERS_MOD, name, &controller_template(name), ROUTES_MARKER, &format!(".merge({name}::routes())"))
}

/// Writes `src/jobs/<name>.rs` and registers it in `src/jobs/mod.rs`.
pub fn job(root: &Path, name: &str) -> Result<Vec<String>, String> {
    check_snake("job", name)?;
    if root.join("src/jobs.rs").exists() {
        return Err("src/jobs.rs exists; the generator needs src/jobs/mod.rs instead (move it there and add the doko: markers)".into());
    }
    let ty = camel(name);
    add_module(root, "jobs", JOBS_MOD, name, &job_template(name, &ty), "// doko:jobs", &format!(".add::<{name}::{ty}>()"))
}

fn check_snake(kind: &str, name: &str) -> Result<(), String> {
    let valid = name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if valid { Ok(()) } else { Err(format!("{kind} name `{name}` must be snake_case, e.g. `blog_posts`")) }
}

fn camel(name: &str) -> String {
    name.split('_').filter(|p| !p.is_empty()).map(|p| p[..1].to_ascii_uppercase() + &p[1..]).collect()
}

/// Writes `src/<dir>/<name>.rs` and adds `pub mod <name>;` plus `item` before `item_marker` in its mod.rs.
fn add_module(
    root: &Path,
    dir: &str,
    default_mod: &str,
    name: &str,
    body: &str,
    item_marker: &str,
    item: &str,
) -> Result<Vec<String>, String> {
    let dir = root.join("src").join(dir);
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
    let updated = current
        .replacen(MODULES_MARKER, &format!("pub mod {name};\n{MODULES_MARKER}"), 1)
        .replacen(item_marker, &format!("{item}\n    {item_marker}"), 1);

    fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    fs::write(&file, body).map_err(|e| format!("writing {}: {e}", file.display()))?;
    fs::write(&mod_rs, updated).map_err(|e| format!("writing {}: {e}", mod_rs.display()))?;
    Ok(vec![file.display().to_string(), mod_rs.display().to_string()])
}

/// `doko new`: a standalone app in `root/<name>`, with wrangler.toml already generated.
pub fn app(root: &Path, name: &str) -> Result<Vec<String>, String> {
    let valid = name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !valid {
        return Err(format!("app name `{name}` must be lowercase letters, digits and `-`, e.g. `my-app`"));
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
    let mut written = vec![];
    for (path, body) in files {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        fs::write(&path, body).map_err(|e| format!("writing {}: {e}", path.display()))?;
        written.push(path.display().to_string());
    }
    Ok(written)
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

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("doko-gen-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn generates_and_registers_controllers() {
        let root = tmp("ok");
        controller(&root, "posts").unwrap();
        controller(&root, "blog_posts").unwrap();
        let m = fs::read_to_string(root.join("src/controllers/mod.rs")).unwrap();
        assert!(m.contains("pub mod posts;\npub mod blog_posts;\n// doko:modules"), "{m}");
        assert!(m.contains(".merge(posts::routes())\n    .merge(blog_posts::routes())\n    // doko:routes"), "{m}");
        let c = fs::read_to_string(root.join("src/controllers/blog_posts.rs")).unwrap();
        assert!(c.contains("\"/blog-posts/{id}\""), "{c}");
        assert!(controller(&root, "posts").unwrap_err().contains("already exists"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn generates_and_registers_jobs() {
        let root = tmp("job");
        job(&root, "send_welcome").unwrap();
        let m = fs::read_to_string(root.join("src/jobs/mod.rs")).unwrap();
        assert!(m.contains("pub mod send_welcome;\n// doko:modules"), "{m}");
        assert!(m.contains(".add::<send_welcome::SendWelcome>()\n    // doko:jobs"), "{m}");
        let j = fs::read_to_string(root.join("src/jobs/send_welcome.rs")).unwrap();
        assert!(j.contains("impl Job for SendWelcome") && j.contains("\"send_welcome\""), "{j}");
        fs::write(root.join("src/jobs.rs"), "").unwrap();
        assert!(job(&root, "other").unwrap_err().contains("src/jobs.rs"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn generates_apps() {
        let root = tmp("app");
        let files = app(&root, "my-app").unwrap();
        assert_eq!(files.len(), 7);
        let w = fs::read_to_string(root.join("my-app/wrangler.toml")).unwrap();
        assert!(w.contains("name = \"my-app\""), "{w}");
        assert!(app(&root, "my-app").unwrap_err().contains("already exists"));
        assert!(app(&root, "My_App").is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rejects_bad_names_and_missing_markers() {
        let root = tmp("bad");
        assert!(controller(&root, "Posts").is_err());
        assert!(controller(&root, "../x").is_err());
        fs::create_dir_all(root.join("src/controllers")).unwrap();
        fs::write(root.join("src/controllers/mod.rs"), "pub mod old;\n").unwrap();
        assert!(controller(&root, "posts").unwrap_err().contains("markers"));
        assert!(!root.join("src/controllers/posts.rs").exists(), "no partial writes");
        fs::remove_dir_all(&root).unwrap();
    }
}
