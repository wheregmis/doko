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

const MODELS_MOD: &str = "\
//! Models: one file per table. `doko generate scaffold <name> ..` adds to this file; keep the doko: marker.

// doko:modules
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
    add_module(
        root,
        "controllers",
        CONTROLLERS_MOD,
        name,
        &controller_template(name),
        ROUTES_MARKER,
        &format!(".merge({name}::routes())"),
    )
}

/// Writes `src/jobs/<name>.rs` and registers it in `src/jobs/mod.rs`.
pub fn job(root: &Path, name: &str) -> Result<Vec<String>, String> {
    check_snake("job", name)?;
    let ty = camel(name);
    add_module(
        root,
        "jobs",
        JOBS_MOD,
        name,
        &job_template(name, &ty),
        "// doko:jobs",
        &format!(".add::<{name}::{ty}>()"),
    )
}

/// What a multi-file generator wrote, and what it could not do itself.
#[derive(Debug)]
pub struct Generated {
    pub written: Vec<String>,
    pub todo: Vec<String>,
}

/// `doko generate auth`: a users migration, `controllers/auth.rs`, `mailers/auth.rs` and a test,
/// plus the bindings, var and secret they need.
pub fn auth(root: &Path) -> Result<Generated, String> {
    let toml_path = root.join("doko.toml");
    let doko_toml = fs::read_to_string(&toml_path).map_err(|e| format!("reading doko.toml: {e}"))?;
    let doc: toml::Table = toml::from_str(&doko_toml).map_err(|e| format!("doko.toml: {e}"))?;
    let table = |path: &[&str]| path.iter().try_fold(&doc, |t, key| t.get(*key)?.as_table());
    let first = |kind: &str| table(&[kind]).and_then(|t| t.keys().next().cloned());

    // Use the app's own bindings where it has them; declare the rest.
    let mut add = String::new();
    let db = first("d1").unwrap_or_else(|| {
        let app = table(&["app"]).and_then(|a| a.get("name")?.as_str()).unwrap_or("app");
        add += &format!(
            "\n# The accounts table lives here: migrations/*_create_users.sql.\n[d1.DB]\ndatabase_name = \"{app}\"\n"
        );
        "DB".into()
    });
    let email = first("email").unwrap_or_else(|| {
        add += "\n# Sends the verification and password-reset mail. Under `doko dev` mail is logged, not sent.\n[email.EMAIL]\n";
        "EMAIL".into()
    });
    let limiter = table(&["do"])
        .and_then(|d| d.iter().find(|(_, v)| v.get("class_name").and_then(|c| c.as_str()) == Some("RateLimiter")))
        .map(|(name, _)| name.clone())
        .unwrap_or_else(|| {
            add += "\n# Limits login, registration and reset attempts per client.\n[do.RATE_LIMITER]\nclass_name = \"RateLimiter\"\n";
            "RATE_LIMITER".into()
        });
    if table(&["kv", "SESSIONS"]).is_none() && table(&["do", "SESSIONS"]).is_none() {
        add += "\n# Session data for doko::auth::Session.\n[kv.SESSIONS]\n";
    }
    let mut todo = Vec::new();
    if table(&["wrangler", "vars"]).is_some_and(|v| v.contains_key("APP_URL")) {
    } else if table(&["wrangler", "vars"]).is_some() {
        // A second [wrangler.vars] header would be invalid TOML.
        todo.push("add `APP_URL = \"http://localhost:8787\"` under [wrangler.vars] in doko.toml: the links in mail start with it".into());
    } else {
        add += "\n# Where the links in mail point. Set it per env: [env.prod.wrangler.vars].\n[wrangler.vars]\nAPP_URL = \"http://localhost:8787\"\n";
    }

    let migrations =
        root.join(table(&["d1", &db]).and_then(|d| d.get("migrations_dir")?.as_str()).unwrap_or("migrations"));
    let existing: Vec<String> = fs::read_dir(&migrations)
        .map(|dir| dir.filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned())).collect())
        .unwrap_or_default();
    for name in existing.iter().filter(|n| n.ends_with(".sql")) {
        let sql = fs::read_to_string(migrations.join(name)).unwrap_or_default().to_lowercase();
        if sql.contains("create table users") || sql.contains("create table if not exists users") {
            return Err(format!("{} already creates a `users` table", migrations.join(name).display()));
        }
    }
    let next = existing.iter().filter_map(|n| n.split('_').next()?.parse::<u32>().ok()).max().unwrap_or(0) + 1;
    let migration = migrations.join(format!("{next:04}_create_users.sql"));
    let test = root.join("tests/auth.rs");
    for path in [root.join("src/controllers/auth.rs"), root.join("src/mailers/auth.rs"), test.clone()] {
        if path.exists() {
            return Err(format!("{} already exists", path.display()));
        }
    }

    let fill = |template: &str| {
        template.replace("__DB__", &db).replace("__EMAIL__", &email).replace("__RATE_LIMITER__", &limiter)
    };
    let write = |path: &Path, body: &str| -> Result<String, String> {
        fs::create_dir_all(path.parent().expect("a file path"))
            .map_err(|e| format!("creating {}: {e}", path.display()))?;
        fs::write(path, body).map_err(|e| format!("writing {}: {e}", path.display()))?;
        Ok(path.display().to_string())
    };
    let controller = fill(include_str!("../templates/auth/controller.rs"));
    let mut written =
        add_module(root, "controllers", CONTROLLERS_MOD, "auth", &controller, ROUTES_MARKER, ".merge(auth::routes())")?;
    written.extend(add_module(
        root,
        "mailers",
        MAILERS_MOD,
        "auth",
        include_str!("../templates/auth/mailer.rs"),
        "",
        "",
    )?);
    written.push(write(&migration, include_str!("../templates/auth/migration.sql"))?);
    written.push(write(&test, &fill(include_str!("../templates/auth/test.rs")))?);
    if !add.is_empty() {
        written.push(write(&toml_path, &format!("{}\n{add}", doko_toml.trim_end()))?);
    }

    // Local secret only: .dev.vars is read by `wrangler dev` and never deployed.
    let dev_vars = root.join(".dev.vars");
    let current = fs::read_to_string(&dev_vars).unwrap_or_default();
    if !current.lines().any(|l| l.starts_with("SESSION_SECRET=")) {
        match random_hex() {
            Some(secret) => {
                let sep = if current.is_empty() || current.ends_with('\n') { "" } else { "\n" };
                written.push(write(&dev_vars, &format!("{current}{sep}SESSION_SECRET={secret}\n"))?);
            }
            None => todo.push("add `SESSION_SECRET=<a long random string>` to .dev.vars".into()),
        }
    }
    todo.push("before deploying: `wrangler secret put SESSION_SECRET`, and set `FROM` in src/mailers/auth.rs to an address on your mail domain".into());

    let lib = root.join("src/lib.rs");
    let (patched, manual) = patch_lib(&fs::read_to_string(&lib).unwrap_or_default());
    todo.extend(manual);
    if let Some(patched) = patched {
        written.push(write(&lib, &patched)?);
    }
    Ok(Generated { written, todo })
}

fn random_hex() -> Option<String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    // shortcut: no /dev/urandom on Windows, where the user is told to set the secret; use a crate if that matters.
    fs::File::open("/dev/urandom").ok()?.read_exact(&mut bytes).ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Declares `modules` in the source of `src/lib.rs` where they are missing, and returns what is
/// left to do by hand.
fn add_mods(out: &mut String, modules: &[&str]) -> Vec<String> {
    let mut todo = Vec::new();
    for module in modules {
        if out.lines().any(|l| l.trim() == format!("mod {module};")) {
            continue;
        }
        // Before the first item, so inner doc comments stay at the top.
        match out
            .lines()
            .find(|l| l.starts_with("mod ") || l.starts_with("use "))
            .map(|l| l.as_ptr() as usize - out.as_ptr() as usize)
        {
            Some(at) => out.insert_str(at, &format!("mod {module};\n")),
            None => todo.push(format!("add `mod {module};` to src/lib.rs")),
        }
    }
    if !out.contains("controllers::routes()") {
        todo.push("merge `controllers::routes()` into Hooks::routes in src/lib.rs".into());
    }
    todo
}

/// A scaffold column: its name, SQL type, Rust type and a sample JSON value for the test.
struct Field<'a> {
    name: &'a str,
    sql: &'static str,
    rust: &'static str,
    sample: &'static str,
    changed: &'static str,
}

fn parse_field(spec: &str) -> Result<Field<'_>, String> {
    let (name, kind) =
        spec.split_once(':').ok_or_else(|| format!("field `{spec}` must be name:type, e.g. `title:string`"))?;
    check_snake("field", name)?;
    if ["id", "created_at", "updated_at"].contains(&name) {
        return Err(format!("field `{name}` is added to every scaffold already"));
    }
    let (sql, rust, sample, changed) = match kind {
        "string" | "text" => ("TEXT", "String", "\"first\"", "\"second\""),
        "int" | "integer" => ("INTEGER", "i64", "1", "2"),
        "float" => ("REAL", "f64", "1.5", "2.5"),
        "bool" | "boolean" => ("INTEGER", "bool", "true", "false"),
        _ => return Err(format!("field `{name}`: type `{kind}` is not one of string, text, int, float, bool")),
    };
    Ok(Field { name, sql, rust, sample, changed })
}

// shortcut: English regular plurals only (post → posts, category → categories, box → boxes);
// rename the table and routes by hand for anything else.
fn plural(name: &str) -> String {
    let vowel_y = ["ay", "ey", "iy", "oy", "uy"].iter().any(|v| name.ends_with(v));
    match name {
        n if n.ends_with('y') && !vowel_y => format!("{}ies", &n[..n.len() - 1]),
        n if ["s", "x", "z", "ch", "sh"].iter().any(|e| n.ends_with(e)) => format!("{n}es"),
        n => format!("{n}s"),
    }
}

/// `doko generate scaffold post title:string body:text`: a migration, `models/post.rs`,
/// `controllers/posts.rs` (list, show, create, update, delete) and a test.
pub fn scaffold(root: &Path, name: &str, specs: &[&str]) -> Result<Generated, String> {
    check_snake("scaffold", name)?;
    let fields = specs.iter().map(|s| parse_field(s)).collect::<Result<Vec<_>, _>>()?;
    if fields.is_empty() {
        return Err(format!("a scaffold needs at least one field: doko generate scaffold {name} title:string"));
    }
    // Params are bound as one tuple, and serde's tuples end at 16; update binds every field plus the id.
    if fields.len() > 15 {
        return Err("a scaffold takes at most 15 fields; add the rest to the generated files by hand".into());
    }
    let names: Vec<&str> = fields.iter().map(|f| f.name).collect();
    if let Some(dup) = names.iter().enumerate().find(|(i, n)| names[..*i].contains(n)) {
        return Err(format!("field `{}` is listed twice", dup.1));
    }
    let table = plural(name);
    let ty = camel(name);

    let toml_path = root.join("doko.toml");
    let doko_toml = fs::read_to_string(&toml_path).map_err(|e| format!("reading doko.toml: {e}"))?;
    let doc: toml::Table = toml::from_str(&doko_toml).map_err(|e| format!("doko.toml: {e}"))?;
    let d1 = doc.get("d1").and_then(|d| d.as_table());
    let db = d1.and_then(|t| t.keys().next().cloned());
    let migrations =
        root.join(db.as_ref().and_then(|db| d1?.get(db)?.get("migrations_dir")?.as_str()).unwrap_or("migrations"));
    let existing: Vec<String> = fs::read_dir(&migrations)
        .map(|dir| dir.filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned())).collect())
        .unwrap_or_default();
    let next = existing.iter().filter_map(|n| n.split('_').next()?.parse::<u32>().ok()).max().unwrap_or(0) + 1;
    let migration = migrations.join(format!("{next:04}_create_{table}.sql"));
    let test = root.join(format!("tests/{table}.rs"));
    for path in
        [root.join(format!("src/models/{name}.rs")), root.join(format!("src/controllers/{table}.rs")), test.clone()]
    {
        if path.exists() {
            return Err(format!("{} already exists", path.display()));
        }
    }
    if existing.iter().any(|n| n.ends_with(&format!("_create_{table}.sql"))) {
        return Err(format!("a migration already creates `{table}`"));
    }

    let write = |path: &Path, body: &str| -> Result<String, String> {
        fs::create_dir_all(path.parent().expect("a file path"))
            .map_err(|e| format!("creating {}: {e}", path.display()))?;
        fs::write(path, body).map_err(|e| format!("writing {}: {e}", path.display()))?;
        Ok(path.display().to_string())
    };
    let db_name = db.clone().unwrap_or_else(|| "DB".into());
    let mut written =
        add_module(root, "models", MODELS_MOD, name, &model_template(name, &ty, &table, &db_name, &fields), "", "")?;
    written.extend(add_module(
        root,
        "controllers",
        CONTROLLERS_MOD,
        &table,
        &resource_controller_template(name, &ty, &table),
        ROUTES_MARKER,
        &format!(".merge({table}::routes())"),
    )?);
    written.push(write(&migration, &scaffold_migration(&table, &fields))?);
    written.push(write(&test, &scaffold_test(&table, &db_name, &fields))?);
    if db.is_none() {
        let app = doc.get("app").and_then(|a| a.get("name")?.as_str()).unwrap_or("app");
        let add = format!("\n# The app's database: migrations/*.sql.\n[d1.DB]\ndatabase_name = \"{app}\"\n");
        written.push(write(&toml_path, &format!("{}\n{add}", doko_toml.trim_end()))?);
    }
    let lib = root.join("src/lib.rs");
    let mut src = fs::read_to_string(&lib).unwrap_or_default();
    let before = src.clone();
    let todo = add_mods(&mut src, &["models", "controllers", "bindings"]);
    if src != before {
        written.push(write(&lib, &src)?);
    }
    Ok(Generated { written, todo })
}

fn scaffold_migration(table: &str, fields: &[Field]) -> String {
    let columns: String = fields.iter().map(|f| format!("  {} {} NOT NULL,\n", f.name, f.sql)).collect();
    format!(
        "\
CREATE TABLE {table} (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
{columns}  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"
    )
}

fn model_template(name: &str, ty: &str, table: &str, db: &str, fields: &[Field]) -> String {
    let join = |f: &dyn Fn(usize, &Field) -> String, sep: &str| {
        fields.iter().enumerate().map(|(i, x)| f(i, x)).collect::<Vec<_>>().join(sep)
    };
    let struct_fields = join(&|_, f| format!("    pub {}: {},\n", f.name, f.rust), "");
    // SQLite hands booleans back as 0 and 1.
    let read_bool =
        |f: &Field| if f.rust == "bool" { "    #[serde(deserialize_with = \"doko::bool_from_sql\")]\n" } else { "" };
    let row_fields = join(&|_, f| format!("{}    pub {}: {},\n", read_bool(f), f.name, f.rust), "");
    let rules: String = fields
        .iter()
        .filter(|f| f.rust == "String")
        .map(|f| {
            format!("        errors.check(\"{0}\", !self.{0}.trim().is_empty(), \"must not be empty\");\n", f.name)
        })
        .collect();
    let rules = if rules.is_empty() {
        "        let _ = errors; // e.g. errors.check(\"price\", self.price >= 0, \"must not be negative\");\n"
            .to_owned()
    } else {
        rules
    };
    let columns = join(&|_, f| f.name.to_owned(), ", ");
    let placeholders = join(&|i, _| format!("?{}", i + 1), ", ");
    let assignments = join(&|i, f| format!("{} = ?{}", f.name, i + 1), ", ");
    let args = join(&|_, f| format!("&input.{}", f.name), ", ");
    let id_param = fields.len() + 1;
    format!(
        "\
//! The `{table}` table. Written by `doko generate scaffold`; it is your code now.

use doko::{{Ctx, Errors, Page, Paged, Result, Validate}};
use serde::{{Deserialize, Serialize}};

use crate::bindings::{db};

/// A row of `{table}`.
#[derive(Serialize, Deserialize)]
pub struct {ty} {{
    pub id: i64,
{row_fields}    pub created_at: String,
    pub updated_at: String,
}}

/// What a client sends to create or update a {name}.
#[derive(Deserialize)]
pub struct {ty}Input {{
{struct_fields}}}

impl Validate for {ty}Input {{
    fn validate(&self, errors: &mut Errors) {{
{rules}    }}
}}

#[derive(Deserialize)]
struct Count {{
    n: i64,
}}

impl {ty} {{
    /// One page, newest first.
    pub async fn page(ctx: &Ctx, page: Page) -> Result<Paged<{ty}>> {{
        let db = ctx.db({db})?;
        let sql = \"SELECT * FROM {table} ORDER BY id DESC LIMIT ?1 OFFSET ?2\";
        let items = db.all(sql, (page.limit(), page.offset())).await?;
        let total: Option<Count> = db.first(\"SELECT COUNT(*) AS n FROM {table}\", ()).await?;
        Ok(page.of(items, total.map_or(0, |c| c.n)))
    }}

    pub async fn find(ctx: &Ctx, id: i64) -> Result<Option<{ty}>> {{
        ctx.db({db})?.first(\"SELECT * FROM {table} WHERE id = ?1\", (id,)).await
    }}

    pub async fn create(ctx: &Ctx, input: &{ty}Input) -> Result<Option<{ty}>> {{
        let sql = \"INSERT INTO {table} ({columns}) VALUES ({placeholders}) RETURNING *\";
        ctx.db({db})?.first(sql, ({args},)).await
    }}

    /// `None` when there is no such row.
    pub async fn update(ctx: &Ctx, id: i64, input: &{ty}Input) -> Result<Option<{ty}>> {{
        let sql = \"UPDATE {table} SET {assignments}, updated_at = datetime('now') WHERE id = ?{id_param} RETURNING *\";
        ctx.db({db})?.first(sql, ({args}, id)).await
    }}

    /// Whether a row was deleted.
    pub async fn delete(ctx: &Ctx, id: i64) -> Result<bool> {{
        Ok(ctx.db({db})?.run(\"DELETE FROM {table} WHERE id = ?1\", (id,)).await?.changes != Some(0))
    }}
}}
"
    )
}

fn resource_controller_template(name: &str, ty: &str, table: &str) -> String {
    let path = table.replace('_', "-");
    format!(
        "\
//! `/{path}`: list, show, create, update and delete. Written by `doko generate scaffold`; it is your code now.
//! These routes are public: add `.require_auth()` after the ones that shouldn't be.

use doko::{{
    Ctx, Error, Page, Paged, Result, Valid,
    axum::{{Json, Router, extract::Path, http::StatusCode, routing::get}},
}};

use crate::models::{name}::{{{ty}, {ty}Input}};

pub fn routes() -> Router<Ctx> {{
    Router::new()
        .route(\"/{path}\", get(index).post(create))
        .route(\"/{path}/{{id}}\", get(show).put(update).delete(destroy))
}}

/// `GET /{path}?page=1&per_page=20`
async fn index(ctx: Ctx, page: Page) -> Result<Json<Paged<{ty}>>> {{
    Ok(Json({ty}::page(&ctx, page).await?))
}}

async fn show(ctx: Ctx, Path(id): Path<i64>) -> Result<Json<{ty}>> {{
    {ty}::find(&ctx, id).await?.map(Json).ok_or_else(Error::not_found)
}}

async fn create(ctx: Ctx, Valid(input): Valid<{ty}Input>) -> Result<(StatusCode, Json<{ty}>)> {{
    let created = {ty}::create(&ctx, &input).await?.ok_or_else(|| Error::internal(\"insert returned no row\"))?;
    Ok((StatusCode::CREATED, Json(created)))
}}

async fn update(ctx: Ctx, Path(id): Path<i64>, Valid(input): Valid<{ty}Input>) -> Result<Json<{ty}>> {{
    {ty}::update(&ctx, id, &input).await?.map(Json).ok_or_else(Error::not_found)
}}

async fn destroy(ctx: Ctx, Path(id): Path<i64>) -> Result<StatusCode> {{
    if {ty}::delete(&ctx, id).await? {{ Ok(StatusCode::NO_CONTENT) }} else {{ Err(Error::not_found()) }}
}}
"
    )
}

fn scaffold_test(table: &str, db: &str, fields: &[Field]) -> String {
    let path = table.replace('_', "-");
    let json = |pick: &dyn Fn(&Field) -> &'static str| {
        let pairs: Vec<String> =
            fields.iter().map(|f| format!("\\\"{}\\\":{}", f.name, pick(f).replace('"', "\\\""))).collect();
        format!("{{{}}}", pairs.join(","))
    };
    let (sample, changed) = (json(&|f| f.sample), json(&|f| f.changed));
    let first = &fields[0];
    let expect_changed = format!("\\\"{}\\\":{}", first.name, first.changed.replace('"', "\\\""));
    format!(
        "\
//! `/{path}` from `doko generate scaffold`, against `wrangler dev`: `doko test` (needs Node).

const JSON: [(&str, &str); 1] = [(\"content-type\", \"application/json\")];

#[test]
#[ignore = \"starts wrangler dev\"]
fn {table}_crud() {{
    let app = doko::test::App::start(&[\"{db}\"]);
    assert!(app.get(\"/{path}\").body.contains(\"\\\"total\\\":0\"));
    assert_eq!(app.send(\"POST\", \"/{path}\", &JSON, \"{{}}\").status, 422, \"a body without the fields\");

    let created = app.send(\"POST\", \"/{path}\", &JSON, \"{sample}\");
    assert_eq!(created.status, 201, \"{{}}\", created.body);
    assert!(created.body.contains(\"\\\"id\\\":1\"), \"{{}}\", created.body);
    assert_eq!(app.send(\"POST\", \"/{path}\", &JSON, \"{sample}\").status, 201);

    let page = app.get(\"/{path}?per_page=1\");
    assert!(page.body.contains(\"\\\"total\\\":2\") && page.body.contains(\"\\\"total_pages\\\":2\"), \"{{}}\", page.body);
    assert!(page.body.contains(\"\\\"id\\\":2\") && !page.body.contains(\"\\\"id\\\":1\"), \"newest first: {{}}\", page.body);

    let updated = app.send(\"PUT\", \"/{path}/1\", &JSON, \"{changed}\");
    assert_eq!(updated.status, 200, \"{{}}\", updated.body);
    assert!(app.get(\"/{path}/1\").body.contains(\"{expect_changed}\"));

    assert_eq!(app.delete(\"/{path}/1\").status, 204);
    assert_eq!(app.get(\"/{path}/1\").status, 404);
    assert_eq!(app.delete(\"/{path}/1\").status, 404);
    assert_eq!(app.send(\"PUT\", \"/{path}/1\", &JSON, \"{changed}\").status, 404);
}}
"
    )
}

/// Adds what `generate auth` needs to `src/lib.rs`: the modules, and `RateLimiter` among the
/// exported objects. Returns the new source if it changed, and what is left to do by hand.
fn patch_lib(src: &str) -> (Option<String>, Vec<String>) {
    let mut out = src.to_owned();
    let mut todo = add_mods(&mut out, &["mailers", "controllers", "bindings"]);
    let call = out.find("doko::app!(").and_then(|start| Some((start, start + out[start..].find(");")?)));
    match call {
        Some((start, end)) if !out[start..end].contains("RateLimiter") => {
            match out[start..end].find("objects: [") {
                Some(list) => out.insert_str(start + list + "objects: [".len(), "RateLimiter, "),
                None => out.insert_str(end, ", objects: [RateLimiter]"),
            }
            if !out[..start].contains("RateLimiter") {
                out.insert_str(start, "use doko::RateLimiter;\n\n");
            }
        }
        Some(_) => {}
        None => todo.push("export the rate limiter in src/lib.rs: `doko::app!(App, objects: [RateLimiter]);` with `use doko::RateLimiter;`".into()),
    }
    ((out != src).then_some(out), todo)
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
/// The published library is `doko-rs`; apps still import it as `doko`.
const DOKO_VERSION: &str = "0.1";

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
doko = {{ package = \"doko-rs\", version = \"{DOKO_VERSION}\" }}
# required: worker's #[event] macro expands to ::worker paths
worker = {{ version = \"0.9\", features = [\"http\", \"axum\", \"d1\", \"queue\"] }}
serde = {{ version = \"1\", features = [\"derive\"] }}

[dev-dependencies]
doko = {{ package = \"doko-rs\", version = \"{DOKO_VERSION}\", features = [\"test\"] }}

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
//! Runs against `wrangler dev`: `doko test` (needs Node).

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
        assert!(
            w.contains("subject: \"password reset\".into()") && w.contains("text: Some(\"Hello!\\n\".into())"),
            "{w}"
        );
        assert!(mailer(&root.0, "welcome").unwrap_err().contains("already exists"));
    }

    #[test]
    fn generates_auth_into_a_new_app() {
        let root = tmp("auth");
        app(&root.0, "shop").unwrap();
        let dir = root.0.join("shop");
        let out = auth(&dir).unwrap();
        for file in [
            "src/controllers/auth.rs",
            "src/mailers/auth.rs",
            "migrations/0001_create_users.sql",
            "tests/auth.rs",
            ".dev.vars",
        ] {
            assert!(dir.join(file).exists(), "{file}");
        }
        let toml = fs::read_to_string(dir.join("doko.toml")).unwrap();
        let rendered = crate::config::render(&toml).expect("the patched doko.toml is valid");
        let bindings = rendered.bindings_rs.unwrap();
        for binding in ["DB", "EMAIL", "RATE_LIMITER", "SESSIONS"] {
            assert!(bindings.contains(&format!("pub const {binding}:")), "{binding}");
        }
        assert!(rendered.wrangler_toml.contains("APP_URL = \"http://localhost:8787\""));
        let controller = fs::read_to_string(dir.join("src/controllers/auth.rs")).unwrap();
        assert!(
            controller.contains("use crate::bindings::{DB, EMAIL, RATE_LIMITER};") && !controller.contains("__"),
            "{controller}"
        );
        let lib = fs::read_to_string(dir.join("src/lib.rs")).unwrap();
        assert!(lib.starts_with("mod bindings;\nmod mailers;\nmod controllers;\n"), "{lib}");
        assert!(lib.ends_with("use doko::RateLimiter;\n\ndoko::app!(App, objects: [RateLimiter]);\n"), "{lib}");
        assert_eq!(out.todo.len(), 1, "only the deploy reminder: {:?}", out.todo);
        assert!(auth(&dir).unwrap_err().contains("already"), "runs once");
    }

    #[test]
    fn auth_reuses_bindings_and_reports_what_it_cannot_patch() {
        let root = tmp("auth-existing");
        fs::create_dir_all(root.0.join("migrations")).unwrap();
        fs::write(root.0.join("migrations/0007_create_posts.sql"), "CREATE TABLE posts (id INTEGER);").unwrap();
        fs::write(
            root.0.join("doko.toml"),
            "[app]\nname = \"x\"\ncompatibility_date = \"2026-11-01\"\n[d1.MAIN]\ndatabase_name = \"x\"\n\
             [do.LIMITS]\nclass_name = \"RateLimiter\"\n[do.SESSIONS]\nclass_name = \"SessionStore\"\n[wrangler.vars]\nA = \"b\"\n",
        )
        .unwrap();
        let out = auth(&root.0).unwrap();
        assert!(root.0.join("migrations/0008_create_users.sql").exists());
        let controller = fs::read_to_string(root.0.join("src/controllers/auth.rs")).unwrap();
        assert!(controller.contains("use crate::bindings::{MAIN, EMAIL, LIMITS};"), "{controller}");
        let toml = fs::read_to_string(root.0.join("doko.toml")).unwrap();
        assert!(
            toml.contains("[email.EMAIL]") && !toml.contains("[kv.SESSIONS]") && !toml.contains("[d1.DB]"),
            "{toml}"
        );
        assert!(crate::config::render(&toml).is_ok());
        let todo = out.todo.join("\n");
        assert!(
            todo.contains("APP_URL") && todo.contains("mod bindings;") && todo.contains("objects: [RateLimiter]"),
            "{todo}"
        );

        let users = tmp("auth-users");
        fs::create_dir_all(users.0.join("migrations")).unwrap();
        fs::write(users.0.join("migrations/0001_users.sql"), "CREATE TABLE users (id INTEGER);").unwrap();
        fs::write(users.0.join("doko.toml"), "[app]\nname = \"x\"\ncompatibility_date = \"2026-11-01\"\n").unwrap();
        assert!(auth(&users.0).unwrap_err().contains("already creates a `users` table"));
        assert!(!users.0.join("src").exists(), "nothing written");
    }

    #[test]
    fn patches_lib_with_existing_objects() {
        let (out, todo) =
            patch_lib("mod bindings;\nmod controllers;\nuse doko::Hooks;\ndoko::app!(App, objects: [Room]);\n");
        let out = out.unwrap();
        assert!(out.starts_with("mod mailers;\nmod bindings;"), "{out}");
        assert!(out.contains("use doko::RateLimiter;\n\ndoko::app!(App, objects: [RateLimiter, Room]);"), "{out}");
        assert_eq!(todo, ["merge `controllers::routes()` into Hooks::routes in src/lib.rs"]);
        let done = "mod bindings;\nmod controllers;\nmod mailers;\nuse doko::RateLimiter;\nfn r() { controllers::routes() }\ndoko::app!(App, objects: [RateLimiter]);\n";
        assert_eq!(patch_lib(done), (None, vec![]));
    }

    #[test]
    fn scaffolds_a_resource() {
        let root = tmp("scaffold");
        app(&root.0, "shop").unwrap();
        let dir = root.0.join("shop");
        let out = scaffold(&dir, "category", &["name:string", "position:int", "price:float", "visible:bool"]).unwrap();
        assert!(out.todo.is_empty(), "{:?}", out.todo);
        let read = |f: &str| fs::read_to_string(dir.join(f)).unwrap_or_else(|_| panic!("{f}"));
        let sql = read("migrations/0001_create_categories.sql");
        assert!(sql.contains("  name TEXT NOT NULL,\n  position INTEGER NOT NULL,\n  price REAL NOT NULL,\n  visible INTEGER NOT NULL,\n"), "{sql}");
        let model = read("src/models/category.rs");
        assert!(model.contains("pub struct CategoryInput {\n    pub name: String,\n    pub position: i64,\n    pub price: f64,\n    pub visible: bool,\n}"), "{model}");
        assert!(
            model.contains(
                "    #[serde(deserialize_with = \"doko::bool_from_sql\")]\n    pub visible: bool,\n    pub created_at"
            ),
            "{model}"
        );
        assert_eq!(model.matches("bool_from_sql").count(), 1, "the input struct reads JSON booleans");
        assert!(
            model.contains(
                "INSERT INTO categories (name, position, price, visible) VALUES (?1, ?2, ?3, ?4) RETURNING *"
            ),
            "{model}"
        );
        assert!(
            model.contains(
                "SET name = ?1, position = ?2, price = ?3, visible = ?4, updated_at = datetime('now') WHERE id = ?5"
            ),
            "{model}"
        );
        assert!(model.contains("(&input.name, &input.position, &input.price, &input.visible, id)"), "{model}");
        assert!(
            model.contains("errors.check(\"name\", !self.name.trim().is_empty()")
                && !model.contains("errors.check(\"position\""),
            "{model}"
        );
        let controller = read("src/controllers/categories.rs");
        assert!(
            controller.contains("use crate::models::category::{Category, CategoryInput};")
                && controller.contains("\"/categories/{id}\""),
            "{controller}"
        );
        assert!(read("src/controllers/mod.rs").contains(".merge(categories::routes())"));
        let test = read("tests/categories.rs");
        assert!(test.contains(r#""{\"name\":\"first\",\"position\":1,\"price\":1.5,\"visible\":true}""#), "{test}");
        assert!(read("src/lib.rs").starts_with("mod bindings;\nmod models;\nmod controllers;\n"));
        assert!(crate::config::render(&read("doko.toml")).unwrap().bindings_rs.unwrap().contains("pub const DB:"));

        // A second resource reuses the database and takes the next migration number.
        scaffold(&dir, "blog_post", &["title:text"]).unwrap();
        assert!(dir.join("migrations/0002_create_blog_posts.sql").exists());
        assert!(read("src/controllers/blog_posts.rs").contains("\"/blog-posts\""));
        assert_eq!(read("doko.toml").matches("[d1.DB]").count(), 1);
        assert!(scaffold(&dir, "category", &["name:string"]).unwrap_err().contains("already exists"));
    }

    #[test]
    fn rejects_bad_scaffolds_and_pluralizes() {
        let root = tmp("scaffold-bad");
        assert!(scaffold(&root.0, "post", &[]).unwrap_err().contains("at least one field"));
        assert!(scaffold(&root.0, "post", &["title"]).unwrap_err().contains("name:type"));
        assert!(scaffold(&root.0, "post", &["title:varchar"]).unwrap_err().contains("not one of"));
        assert!(scaffold(&root.0, "post", &["id:int"]).unwrap_err().contains("already"));
        assert!(scaffold(&root.0, "post", &["a:int", "a:int"]).unwrap_err().contains("twice"));
        assert!(scaffold(&root.0, "Post", &["a:int"]).is_err());
        assert!(!root.0.join("src").exists(), "nothing written");
        for (one, many) in [
            ("post", "posts"),
            ("category", "categories"),
            ("day", "days"),
            ("box", "boxes"),
            ("match", "matches"),
            ("address", "addresses"),
        ] {
            assert_eq!(plural(one), many);
        }
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
