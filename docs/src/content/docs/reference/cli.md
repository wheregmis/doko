---
title: "The doko command"
description: "Every doko CLI command and what doko deploy checks first."
---

| Command | What |
|---|---|
| `doko dev` | regenerate, warn about required secrets missing from `.dev.vars`, then `wrangler dev` |
| `doko deploy [--env prod]` | regenerate, check the secrets and Hyperdrive ids, apply pending D1 migrations, then `wrangler deploy` |
| `doko test` | every test, including the ones that start `wrangler dev` |
| `doko routes` | the app's routes, with methods and where each is defined |
| `doko doctor [--env prod]` | checks Node, the wasm target, `psql` (if you use Postgres), doko.toml and local secrets; with `--env`, the deployed secrets too |
| `doko migrate` | apply D1 migrations (`--pg` for Postgres) |
| `doko db seed` | run `seeds/*.sql` against the local database (`--remote --env prod` for a deployed one) |
| `doko db reset` | delete the local D1 data, then migrate and seed again. Local only |
| `doko console` | a SQL prompt on the local D1 database (`--remote` for a deployed one; `--pg` opens `psql`) |
| `doko generate ..` | see [Generators](/doko/guides/generators/) |

**Before a deploy,** `doko deploy` refuses to go on when:

- a secret the app needs is not set for that env. It lists the `wrangler secret put` commands to run;
- a `[hyperdrive.*]` binding has no `id` for that env;
- a D1 database still has pending migrations after you were asked to apply them. Wrangler shows what is pending and asks first; `--no-migrate` skips this step.

On a first deploy there is no worker to hold secrets yet, so the secrets can only be warned about. Postgres migrations are not applied by `doko deploy`: run `doko migrate --pg` with `DATABASE_URL` set.

**`doko routes`** reads the source, it doesn't run the app. A path under `.nest("/api", ..)` is listed without its prefix.
