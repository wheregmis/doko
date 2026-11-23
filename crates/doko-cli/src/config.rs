//! `doko.toml` → `wrangler.toml` + `src/bindings.rs`.

use std::{collections::BTreeMap, fmt::Write};

use serde::{Deserialize, Serialize};
use toml::{Table, Value};

pub const BUILD_COMMAND: &str = "cargo install -q worker-build@^0.9 && worker-build --release";
/// Environment used when wrangler runs without `--env`.
const DEFAULT_ENV: &str = "dev";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DokoToml {
    app: App,
    /// App config, read at runtime with `ctx.config::<T>()`.
    #[serde(default)]
    config: Table,
    /// Cron tasks: `name = "schedule"`. Handlers are registered in `Hooks::tasks`.
    #[serde(default)]
    tasks: BTreeMap<String, String>,
    #[serde(flatten)]
    bindings: Bindings,
    #[serde(default)]
    env: BTreeMap<String, Env>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct App {
    name: String,
    compatibility_date: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Env {
    /// Deep-merged over the top-level `[config]`.
    #[serde(default)]
    config: Table,
    /// Each task listed here replaces the top-level schedule of the same name.
    #[serde(default)]
    tasks: BTreeMap<String, String>,
    /// Each binding listed here replaces the top-level one of the same name.
    #[serde(flatten)]
    bindings: Bindings,
}

// Field names match wrangler's, so wrangler docs apply as-is.
#[derive(Deserialize, Default, Clone)]
struct Bindings {
    #[serde(default)]
    kv: BTreeMap<String, Kv>,
    #[serde(default)]
    r2: BTreeMap<String, R2>,
    #[serde(default)]
    d1: BTreeMap<String, D1>,
    #[serde(default)]
    queues: BTreeMap<String, Queue>,
}

/// `id` is optional: wrangler simulates locally, and provisions on deploy.
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
struct Kv {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
struct R2 {
    bucket_name: String,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
struct D1 {
    database_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    database_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    migrations_dir: Option<String>,
}

/// A queue this worker both sends to (as the binding) and consumes (running `Hooks::jobs`).
/// Serialized as wrangler's `[[queues.consumers]]` entry.
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
struct Queue {
    queue: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_batch_size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_batch_timeout: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_retries: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_delay: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dead_letter_queue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_concurrency: Option<u32>,
}

#[derive(Serialize)]
struct Queues {
    producers: Vec<Producer>,
    consumers: Vec<Queue>,
}

#[derive(Serialize)]
struct Producer {
    binding: String,
    queue: String,
}

#[derive(Serialize)]
struct Wrangler<'a> {
    name: &'a str,
    main: &'a str,
    compatibility_date: &'a str,
    build: Build<'a>,
    #[serde(flatten)]
    resources: Resources,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<&'a str, Resources>,
}

#[derive(Serialize)]
struct Build<'a> {
    command: &'a str,
}

#[derive(Serialize)]
struct Triggers {
    crons: Vec<String>,
}

// Wrangler envs don't inherit vars or bindings, so every env gets a fully merged copy.
#[derive(Serialize)]
struct Resources {
    // Always written, even empty: wrangler envs inherit triggers, and doko owns the schedule.
    triggers: Triggers,
    vars: Vars,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    kv_namespaces: Vec<Bound<Kv>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    r2_buckets: Vec<Bound<R2>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    d1_databases: Vec<Bound<D1>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    queues: Option<Queues>,
}

#[derive(Serialize)]
struct Bound<T> {
    binding: String,
    #[serde(flatten)]
    inner: T,
}

#[derive(Serialize)]
#[allow(non_snake_case)]
struct Vars {
    DOKO_ENV: String,
    DOKO_CONFIG: Table,
    DOKO_TASKS: BTreeMap<String, String>,
}

pub struct Output {
    pub wrangler_toml: String,
    /// Contents of `src/bindings.rs`, when doko.toml declares any bindings.
    pub bindings_rs: Option<String>,
    /// D1 binding names, for `doko migrate`.
    pub d1: Vec<String>,
}

pub fn render(doko_toml: &str) -> Result<Output, String> {
    let cfg: DokoToml = toml::from_str(doko_toml).map_err(|e| format!("doko.toml: {e}"))?;
    for name in cfg.env.keys() {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err(format!("doko.toml: env name `{name}` must be lowercase letters, digits or `-`"));
        }
    }
    let resources_for = |env: &str| -> Result<(Resources, Bindings), String> {
        let mut config = cfg.config.clone();
        let mut b = cfg.bindings.clone();
        let mut tasks = cfg.tasks.clone();
        if let Some(e) = cfg.env.get(env) {
            merge(&mut config, &e.config);
            tasks.extend(e.tasks.clone());
            b.queues.extend(e.bindings.queues.clone());
            b.kv.extend(e.bindings.kv.clone());
            b.r2.extend(e.bindings.r2.clone());
            b.d1.extend(e.bindings.d1.clone());
        }
        let at = if env == DEFAULT_ENV { String::new() } else { format!(" [env.{env}]") };
        check_binding_names(&b).map_err(|e| format!("doko.toml{at}: {e}"))?;
        check_tasks(&tasks).map_err(|e| format!("doko.toml{at}: {e}"))?;
        let mut crons: Vec<String> = tasks.values().cloned().collect();
        crons.sort();
        crons.dedup();
        let queues = (!b.queues.is_empty()).then(|| Queues {
            producers: b.queues.iter().map(|(k, q)| Producer { binding: k.clone(), queue: q.queue.clone() }).collect(),
            consumers: b.queues.values().cloned().collect(),
        });
        let r = Resources {
            triggers: Triggers { crons },
            vars: Vars { DOKO_ENV: env.to_owned(), DOKO_CONFIG: config, DOKO_TASKS: tasks },
            kv_namespaces: bound(&b.kv),
            r2_buckets: bound(&b.r2),
            d1_databases: bound(&b.d1),
            queues,
        };
        Ok((r, b))
    };

    let (resources, mut all) = resources_for(DEFAULT_ENV)?;
    let mut env = BTreeMap::new();
    // `dev` is the top level, not a wrangler env.
    for name in cfg.env.keys().filter(|k| *k != DEFAULT_ENV) {
        let (r, b) = resources_for(name)?;
        all.kv.extend(b.kv);
        all.r2.extend(b.r2);
        all.d1.extend(b.d1);
        all.queues.extend(b.queues);
        env.insert(name.as_str(), r);
    }
    let app = &cfg.app;
    let wrangler = Wrangler {
        name: &app.name,
        main: "build/index.js",
        compatibility_date: &app.compatibility_date,
        build: Build { command: BUILD_COMMAND },
        resources,
        env,
    };
    let body = toml::to_string(&wrangler).map_err(|e| e.to_string())?;
    Ok(Output {
        wrangler_toml: format!("# Generated by doko from doko.toml. Do not edit.\n{body}"),
        bindings_rs: bindings_rs(&all),
        d1: all.d1.into_keys().collect(),
    })
}

fn bound<T: Clone>(m: &BTreeMap<String, T>) -> Vec<Bound<T>> {
    m.iter().map(|(k, v)| Bound { binding: k.clone(), inner: v.clone() }).collect()
}

fn check_tasks(tasks: &BTreeMap<String, String>) -> Result<(), String> {
    for (name, schedule) in tasks {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
            return Err(format!("task `{name}` must be lowercase letters, digits, `_` or `-`"));
        }
        // Workers cron: 5 fields (minute hour day month weekday).
        if schedule.split_whitespace().count() != 5 {
            return Err(format!("task `{name}`: schedule `{schedule}` must have 5 fields, e.g. \"0 3 * * *\""));
        }
    }
    Ok(())
}

fn check_binding_names(b: &Bindings) -> Result<(), String> {
    let names = b.kv.keys().chain(b.r2.keys()).chain(b.d1.keys()).chain(b.queues.keys());
    let mut seen = std::collections::BTreeSet::new();
    for name in names {
        let valid = name.starts_with(|c: char| c.is_ascii_uppercase())
            && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if !valid {
            return Err(format!("binding `{name}` must be UPPER_SNAKE_CASE"));
        }
        if name.starts_with("DOKO_") {
            return Err(format!("binding `{name}`: the DOKO_ prefix is reserved"));
        }
        if !seen.insert(name) {
            return Err(format!("binding `{name}` is declared more than once"));
        }
    }
    Ok(())
}

fn bindings_rs(b: &Bindings) -> Option<String> {
    if b.kv.is_empty() && b.r2.is_empty() && b.d1.is_empty() && b.queues.is_empty() {
        return None;
    }
    let mut s = String::from("//! Generated by doko from doko.toml. Do not edit.\n#![allow(dead_code)]\n\n");
    let kinds: [(&str, Vec<&String>); 4] = [
        ("Kv", b.kv.keys().collect()),
        ("R2", b.r2.keys().collect()),
        ("D1", b.d1.keys().collect()),
        ("Queue", b.queues.keys().collect()),
    ];
    for (kind, names) in kinds {
        for name in names {
            let _ = writeln!(s, "pub const {name}: doko::{kind}Binding = doko::{kind}Binding::new(\"{name}\");");
        }
    }
    Some(s)
}

fn merge(base: &mut Table, over: &Table) {
    for (k, v) in over {
        match (base.get_mut(k), v) {
            (Some(Value::Table(b)), Value::Table(o)) => merge(b, o),
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = "[app]\nname = \"hello\"\ncompatibility_date = \"2026-11-01\"\n";

    fn out(extra: &str) -> Output {
        render(&format!("{APP}{extra}")).unwrap()
    }

    fn parse(extra: &str) -> Table {
        toml::from_str(&out(extra).wrangler_toml).unwrap()
    }

    fn err(extra: &str) -> String {
        render(&format!("{APP}{extra}")).err().expect("expected an error")
    }

    #[test]
    fn renders_minimal_app() {
        let v = parse("");
        assert_eq!(v["name"].as_str(), Some("hello"));
        assert_eq!(v["main"].as_str(), Some("build/index.js"));
        assert_eq!(v["build"]["command"].as_str(), Some(BUILD_COMMAND));
        assert_eq!(v["vars"]["DOKO_ENV"].as_str(), Some("dev"));
        assert!(v["vars"]["DOKO_CONFIG"].as_table().unwrap().is_empty());
        assert!(v["triggers"]["crons"].as_array().unwrap().is_empty());
        assert!(v["vars"]["DOKO_TASKS"].as_table().unwrap().is_empty());
        for key in ["env", "kv_namespaces", "r2_buckets", "d1_databases", "queues"] {
            assert!(!v.contains_key(key), "{key}");
        }
        assert!(out("").bindings_rs.is_none());
    }

    #[test]
    fn renders_tasks_per_env() {
        let v = parse(
            "[tasks]\ncleanup = \"0 3 * * *\"\ndigest = \"0 3 * * *\"\nping = \"*/5 * * * *\"\n\
             [env.prod.tasks]\nping = \"0 * * * *\"\n[env.staging]\n",
        );
        assert_eq!(v["triggers"]["crons"].as_array().unwrap().len(), 2, "schedules are deduped");
        assert_eq!(v["vars"]["DOKO_TASKS"]["ping"].as_str(), Some("*/5 * * * *"));
        let prod = &v["env"]["prod"];
        assert_eq!(prod["vars"]["DOKO_TASKS"]["ping"].as_str(), Some("0 * * * *"));
        assert_eq!(prod["vars"]["DOKO_TASKS"]["cleanup"].as_str(), Some("0 3 * * *"), "inherited");
        assert!(prod["triggers"]["crons"].as_array().unwrap().contains(&"0 * * * *".into()));
        assert_eq!(v["env"]["staging"]["triggers"]["crons"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn rejects_bad_tasks() {
        assert!(err("[tasks]\nCleanup = \"0 3 * * *\"\n").contains("lowercase"));
        assert!(err("[tasks]\ncleanup = \"daily\"\n").contains("5 fields"));
    }

    #[test]
    fn renders_queues() {
        let o = out("[queues.JOBS]\nqueue = \"jobs\"\nmax_retries = 3\ndead_letter_queue = \"jobs-dlq\"\n[env.prod.queues.JOBS]\nqueue = \"jobs-prod\"\n");
        let v: Table = toml::from_str(&o.wrangler_toml).unwrap();
        let q = &v["queues"];
        assert_eq!(q["producers"][0]["binding"].as_str(), Some("JOBS"));
        assert_eq!(q["producers"][0]["queue"].as_str(), Some("jobs"));
        assert_eq!(q["consumers"][0]["queue"].as_str(), Some("jobs"));
        assert_eq!(q["consumers"][0]["max_retries"].as_integer(), Some(3));
        assert_eq!(q["consumers"][0]["dead_letter_queue"].as_str(), Some("jobs-dlq"));
        assert!(q["consumers"][0].get("max_batch_size").is_none());
        assert_eq!(v["env"]["prod"]["queues"]["consumers"][0]["queue"].as_str(), Some("jobs-prod"));
        assert!(o.bindings_rs.unwrap().contains("pub const JOBS: doko::QueueBinding"));
        assert!(err("[queues.JOBS]\nqueue = \"j\"\nretries = 1\n").contains("retries"));
    }

    #[test]
    fn deep_merges_env_config() {
        let v = parse(
            "[config]\ngreeting = \"hi\"\n[config.db]\nsize = 1\nname = \"a\"\n\
             [env.dev.config]\ngreeting = \"hi dev\"\n\
             [env.prod.config.db]\nsize = 9\n",
        );
        assert_eq!(v["vars"]["DOKO_CONFIG"]["greeting"].as_str(), Some("hi dev"));
        let env = v["env"].as_table().unwrap();
        assert!(!env.contains_key("dev"), "dev is the top level");
        let prod = &env["prod"]["vars"];
        assert_eq!(prod["DOKO_ENV"].as_str(), Some("prod"));
        assert_eq!(prod["DOKO_CONFIG"]["greeting"].as_str(), Some("hi"));
        assert_eq!(prod["DOKO_CONFIG"]["db"]["size"].as_integer(), Some(9));
        assert_eq!(prod["DOKO_CONFIG"]["db"]["name"].as_str(), Some("a"));
    }

    const BINDINGS: &str = "\
[kv.CACHE]
[r2.FILES]
bucket_name = \"files-dev\"
[d1.DB]
database_name = \"app-dev\"
[env.prod.kv.CACHE]
id = \"abc\"
[env.prod.r2.FILES]
bucket_name = \"files-prod\"
";

    #[test]
    fn renders_bindings_into_every_env() {
        let v = parse(BINDINGS);
        assert_eq!(v["kv_namespaces"][0]["binding"].as_str(), Some("CACHE"));
        assert!(v["kv_namespaces"][0].get("id").is_none());
        assert_eq!(v["r2_buckets"][0]["bucket_name"].as_str(), Some("files-dev"));
        assert_eq!(v["d1_databases"][0]["database_name"].as_str(), Some("app-dev"));
        let prod = &v["env"]["prod"];
        assert_eq!(prod["kv_namespaces"][0]["id"].as_str(), Some("abc"));
        assert_eq!(prod["r2_buckets"][0]["bucket_name"].as_str(), Some("files-prod"));
        assert_eq!(prod["d1_databases"][0]["database_name"].as_str(), Some("app-dev"), "inherited");
    }

    #[test]
    fn generates_typed_bindings() {
        let o = out(BINDINGS);
        let rs = o.bindings_rs.unwrap();
        assert!(rs.contains("pub const CACHE: doko::KvBinding = doko::KvBinding::new(\"CACHE\");"), "{rs}");
        assert!(rs.contains("pub const FILES: doko::R2Binding"), "{rs}");
        assert!(rs.contains("pub const DB: doko::D1Binding"), "{rs}");
        assert_eq!(o.d1, ["DB"]);
    }

    #[test]
    fn rejects_bad_bindings() {
        assert!(err("[kv.cache]\n").contains("UPPER_SNAKE_CASE"));
        assert!(err("[kv.DOKO_X]\n").contains("reserved"));
        assert!(err("[kv.X]\n[r2.X]\nbucket_name = \"b\"\n").contains("more than once"));
        assert!(err("[r2.X]\n").contains("bucket_name"));
        assert!(err("[env.prod.kv.bad]\n").contains("[env.prod]"));
    }

    #[test]
    fn rejects_typos() {
        let e = render("[app]\nname = \"a\"\ncompatibilty_date = \"x\"\n").err().unwrap();
        assert!(e.contains("compatibilty_date"), "{e}");
        assert!(err("[env.prod]\nconfg = {}\n").contains("confg"));
        assert!(err("[kv.X]\nidd = \"1\"\n").contains("idd"));
    }

    #[test]
    fn rejects_bad_env_names() {
        assert!(err("[env.Prod]\n").contains("Prod"));
    }
}
