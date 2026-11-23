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
  doko config                      regenerate wrangler.toml from doko.toml
  doko dev [wrangler args]         regenerate, then `wrangler dev`
  doko deploy [wrangler args]      regenerate, then `wrangler deploy` (e.g. --env prod)
  doko generate controller <name>  scaffold src/controllers/<name>.rs (alias: g)";
const WRANGLER: &str = "wrangler@4";

fn write_wrangler_toml() -> Result<(), String> {
    let src = fs::read_to_string("doko.toml").map_err(|e| format!("reading doko.toml: {e}"))?;
    fs::write("wrangler.toml", config::render(&src)?).map_err(|e| format!("writing wrangler.toml: {e}"))
}

fn wrangler(args: &[String]) -> Result<(), String> {
    write_wrangler_toml()?;
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
            write_wrangler_toml()?;
            println!("wrote wrangler.toml");
            Ok(())
        }
        ["dev" | "deploy", ..] => wrangler(args),
        ["generate" | "g", "controller", name] => {
            for path in generate::controller(Path::new("."), name)? {
                println!("wrote {path}");
            }
            println!("make sure your app has `mod controllers;` and merges `controllers::routes()` in Hooks::routes");
            Ok(())
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
