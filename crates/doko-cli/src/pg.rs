//! Postgres migrations: `migrations/pg/*.sql`, applied in name order by `doko migrate --pg`.
//!
//! Wrangler has no Postgres tooling, so this drives `psql`. Applied files are recorded in a
//! `_doko_migrations` table, and each file runs in one transaction together with its record.

use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

pub const DIR: &str = "migrations/pg";
const CREATE_TABLE: &str = "CREATE TABLE IF NOT EXISTS _doko_migrations (name text PRIMARY KEY, applied_at timestamptz NOT NULL DEFAULT now())";

/// File names are written into SQL as they are, so they are kept to characters that need no quoting.
fn is_migration_file(name: &str) -> bool {
    name.strip_suffix(".sql").is_some_and(|stem| {
        !stem.is_empty() && stem.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    })
}

/// The migration files in `dir`, in the order they apply.
pub fn files(dir: &Path) -> Result<Vec<String>, String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("reading {}: {e}", dir.display())),
    };
    let mut names = Vec::new();
    for entry in entries {
        let name =
            entry.map_err(|e| format!("reading {}: {e}", dir.display()))?.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".sql") {
            continue;
        }
        if !is_migration_file(&name) {
            return Err(format!("{}/{name}: migration names must be lowercase letters, digits and `_`", dir.display()));
        }
        names.push(name);
    }
    names.sort();
    Ok(names)
}

/// The files not yet in `applied`, which is one name per line.
fn pending(files: Vec<String>, applied: &str) -> Vec<String> {
    files.into_iter().filter(|f| !applied.lines().any(|a| a.trim() == f)).collect()
}

/// `0003_<name>.sql` when two migrations exist.
fn next_file(existing: &[String], name: &str) -> String {
    let last = existing.iter().filter_map(|f| f.split('_').next()?.parse::<u32>().ok()).max().unwrap_or(0);
    format!("{:04}_{name}.sql", last + 1)
}

/// Creates the next empty migration file and returns its path.
pub fn generate(dir: &Path, name: &str) -> Result<String, String> {
    let file = next_file(&files(dir)?, name);
    if !is_migration_file(&file) {
        return Err(format!("migration name `{name}` must be lowercase letters, digits and `_`, e.g. `create_notes`"));
    }
    fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let path = dir.join(&file);
    fs::write(&path, format!("-- Migration: {name}\n")).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(path.display().to_string())
}

/// The URL without its credentials, for printing.
fn target(url: &str) -> &str {
    url.rsplit_once('@').map_or(url, |(_, host)| host)
}

fn psql(url: &str, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
    // -X: no psqlrc. ON_ERROR_STOP: a failing statement fails the run instead of being skipped.
    let mut child = Command::new("psql")
        .args([url, "-X", "-q", "-v", "ON_ERROR_STOP=1"])
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("running psql: {e}. `doko migrate --pg` needs the Postgres client `psql` on your PATH"))?;
    if let (Some(sql), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(sql.as_bytes()).map_err(|e| format!("writing to psql: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("running psql: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("psql exited with {}", out.status))
    }
}

/// Applies the pending migrations in `dir` to the database at `url`.
pub fn migrate(dir: &Path, url: &str) -> Result<(), String> {
    let files = files(dir)?;
    if files.is_empty() {
        return Err(format!(
            "no migrations in {}: create one with `doko generate migration <name> --pg`",
            dir.display()
        ));
    }
    println!("migrating {}", target(url));
    psql(url, &["-c", CREATE_TABLE], None)?;
    let applied = psql(url, &["-At", "-c", "SELECT name FROM _doko_migrations"], None)?;
    let pending = pending(files, &applied);
    for file in &pending {
        let path = dir.join(file);
        let sql = fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        // One transaction: a migration is recorded exactly when all of it applied.
        let script = format!("{sql}\n;\nINSERT INTO _doko_migrations (name) VALUES ('{file}');\n");
        psql(url, &["--single-transaction", "-f", "-"], Some(&script)).map_err(|e| format!("{file}: {e}"))?;
        println!("applied {file}");
    }
    println!("{} applied, {} already up to date", pending.len(), applied.lines().count());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_numbers_and_filters_migrations() {
        let dir = std::env::temp_dir().join(format!("doko-pg-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert!(files(&dir).unwrap().is_empty(), "a missing directory has no migrations");

        assert!(generate(&dir, "create_notes").unwrap().ends_with("0001_create_notes.sql"));
        assert!(generate(&dir, "add_tags").unwrap().ends_with("0002_add_tags.sql"));
        assert!(generate(&dir, "Bad-Name").is_err());
        fs::write(dir.join("README.md"), "not a migration").unwrap();
        let all = files(&dir).unwrap();
        assert_eq!(all, ["0001_create_notes.sql", "0002_add_tags.sql"]);

        assert_eq!(pending(all.clone(), "0001_create_notes.sql\n"), ["0002_add_tags.sql"]);
        assert!(pending(all, "0001_create_notes.sql\n0002_add_tags.sql\n").is_empty());

        fs::write(dir.join("0003_it's.sql"), "").unwrap();
        assert!(files(&dir).unwrap_err().contains("lowercase"), "a name that would need quoting is refused");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn hides_credentials_when_printing() {
        assert_eq!(target("postgres://user:secret@db.example:5432/app"), "db.example:5432/app");
        assert_eq!(target("postgres://localhost/app"), "postgres://localhost/app");
    }
}
