//! `doko`: generates `wrangler.toml` from `doko.toml`, drives wrangler, and scaffolds code.

mod config;
mod generate;

use std::{
    fs,
    path::Path,
    process::{Command, ExitCode},
};

const USAGE: &str = "\
usage:
  doko config                         regenerate wrangler.toml (and src/bindings.rs) from doko.toml
  doko dev [wrangler args]            regenerate, then `wrangler dev`
  doko deploy [wrangler args]         regenerate, then `wrangler deploy` (e.g. --env prod)
  doko migrate [wrangler args]        apply D1 migrations to every D1 binding (local unless --remote)
  doko generate controller <name>     scaffold src/controllers/<name>.rs (alias: g)
  doko generate migration <name> [DB] new D1 migration file (DB needed with several D1 bindings)";
const WRANGLER: &str = "wrangler@4";
const BINDINGS_RS: &str = "src/bindings.rs";

/// Regenerates wrangler.toml and src/bindings.rs, returning the D1 binding names.
fn write_config() -> Result<Vec<String>, String> {
    let src = fs::read_to_string("doko.toml").map_err(|e| format!("reading doko.toml: {e}"))?;
    let out = config::render(&src)?;
    fs::write("wrangler.toml", out.wrangler_toml).map_err(|e| format!("writing wrangler.toml: {e}"))?;
    if let Some(rs) = out.bindings_rs {
        // Skip unchanged writes so cargo doesn't rebuild for nothing.
        if fs::read_to_string(BINDINGS_RS).ok().as_deref() != Some(rs.as_str()) {
            fs::write(BINDINGS_RS, rs).map_err(|e| format!("writing {BINDINGS_RS}: {e}"))?;
        }
    }
    Ok(out.d1)
}

fn wrangler<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Result<(), String> {
    let status = Command::new("npx")
        .args(["--yes", WRANGLER])
        .args(args)
        .status()
        .map_err(|e| format!("running npx {WRANGLER}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("wrangler exited with {status}")) }
}

fn run(args: &[String]) -> Result<(), String> {
    let strs: Vec<&str> = args.iter().map(String::as_str).collect();
    match strs.as_slice() {
        ["config"] => {
            write_config()?;
            println!("wrote wrangler.toml");
            Ok(())
        }
        ["dev" | "deploy", ..] => {
            write_config()?;
            wrangler(args)
        }
        ["migrate", rest @ ..] => {
            let dbs = write_config()?;
            if dbs.is_empty() {
                return Err("doko.toml declares no [d1.<BINDING>] databases".into());
            }
            for db in dbs {
                wrangler(&[&["d1", "migrations", "apply", db.as_str()], rest].concat())?;
            }
            Ok(())
        }
        ["generate" | "g", "controller", name] => {
            for path in generate::controller(Path::new("."), name)? {
                println!("wrote {path}");
            }
            println!("make sure your app has `mod controllers;` and merges `controllers::routes()` in Hooks::routes");
            Ok(())
        }
        ["generate" | "g", "migration", name, db @ ..] => {
            let dbs = write_config()?;
            let db = match (db, dbs.as_slice()) {
                ([db], _) if dbs.iter().any(|d| d == db) => *db,
                ([db], _) => return Err(format!("`{db}` is not a [d1.<BINDING>] in doko.toml")),
                ([], [only]) => only.as_str(),
                ([], []) => return Err("doko.toml declares no [d1.<BINDING>] databases".into()),
                _ => return Err(format!("several D1 bindings; name one: doko g migration {name} <{}>", dbs.join("|"))),
            };
            wrangler(&["d1", "migrations", "create", db, name])
        }
        _ => Err(USAGE.to_owned()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
