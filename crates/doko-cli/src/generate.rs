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

/// Writes `src/controllers/<name>.rs` and registers it in `src/controllers/mod.rs`.
/// Returns the paths it created or changed.
pub fn controller(root: &Path, name: &str) -> Result<Vec<String>, String> {
    let valid = name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !valid {
        return Err(format!("controller name `{name}` must be snake_case, e.g. `blog_posts`"));
    }
    let dir = root.join("src/controllers");
    let file = dir.join(format!("{name}.rs"));
    if file.exists() {
        return Err(format!("{} already exists", file.display()));
    }
    let mod_rs = dir.join("mod.rs");
    let current = match fs::read_to_string(&mod_rs) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => CONTROLLERS_MOD.to_owned(),
        Err(e) => return Err(format!("reading {}: {e}", mod_rs.display())),
    };
    if !current.contains(MODULES_MARKER) || !current.contains(ROUTES_MARKER) {
        return Err(format!(
            "{} is missing the `{MODULES_MARKER}` / `{ROUTES_MARKER}` markers; add `pub mod {name};` and `.merge({name}::routes())` by hand",
            mod_rs.display()
        ));
    }
    let updated = current
        .replacen(MODULES_MARKER, &format!("pub mod {name};\n{MODULES_MARKER}"), 1)
        .replacen(ROUTES_MARKER, &format!(".merge({name}::routes())\n    {ROUTES_MARKER}"), 1);

    fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    fs::write(&file, controller_template(name)).map_err(|e| format!("writing {}: {e}", file.display()))?;
    fs::write(&mod_rs, updated).map_err(|e| format!("writing {}: {e}", mod_rs.display()))?;
    Ok(vec![file.display().to_string(), mod_rs.display().to_string()])
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
