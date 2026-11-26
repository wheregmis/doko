---
title: "Postgres"
description: "Hyperdrive-backed Postgres queries in D1's shape, with migrations."
---

Postgres goes through [Hyperdrive](https://developers.cloudflare.com/hyperdrive/), which pools connections close to the database. It is behind a feature, so apps that only use D1 don't carry the driver:

```toml
# Cargo.toml
doko = { package = "doko-rs", version = "0.1", features = ["pg"] }
```

```toml
# doko.toml
[hyperdrive.PG]
local_connection_string = "postgres://app:app@127.0.0.1:5432/app"   # used by `doko dev` and `doko migrate --pg`

[env.prod.hyperdrive.PG]
id = "<id from `wrangler hyperdrive create`>"
```

```rust
use crate::bindings::PG;

let pg = ctx.pg(PG).await?;
let todos: Vec<Todo> = pg.all("SELECT * FROM todos WHERE done = $1 ORDER BY id", (false,)).await?;
let todo: Option<Todo> = pg.first("INSERT INTO todos (title) VALUES ($1) RETURNING *", (&title,)).await?;
let changed: u64 = pg.run("UPDATE todos SET done = true WHERE id = $1", (id,)).await?;
pg.batch(vec![pg.statement("..", (a,))?, pg.statement("..", (b,))?]).await?;   // one transaction
```

- **Same shape as D1:** `all`, `first`, `run`, `statement` + `batch`, with params as a tuple and rows read by column name. The differences are the placeholders (`$1`, not `?1`), `ctx.pg(..)` being async because it connects, and `run` returning the number of changed rows.
- **Types.** Text, integers, floats, booleans, `json`/`jsonb`, UUIDs, dates and timestamps are read (the last three as strings). Cast anything else in SQL: `price::text`. Params bind as the type Postgres infers for the placeholder; for a type doko doesn't encode, such as a timestamp, write `$1::text::timestamptz`.
- **Local credentials only** go in `local_connection_string`: doko.toml is committed. The real database's credentials live in the Hyperdrive config.
- A binding without an `id` gets a placeholder so `doko dev` works; deploying that env fails until you set the real one.

## Postgres migrations

```bash
doko generate migration create_todos --pg   # migrations/pg/0001_create_todos.sql
doko migrate --pg                           # apply to the local database
DATABASE_URL=postgres://.. doko migrate --pg    # apply to any other database
```

Wrangler has no Postgres tooling, so this runs `psql`, which must be installed. Each file runs in one transaction and is recorded in a `_doko_migrations` table, so a rerun applies only what is new.
