---
title: "Config and environments"
description: "Typed config merged per environment, vars, secrets and raw wrangler settings."
---

```toml
[config]                 # any TOML; read with ctx.config::<YourConfig>()
greeting = "hello"

[env.prod.config]        # deep-merged over [config] for `--env prod`
greeting = "hello (prod)"
```

```toml
[secrets]
required = ["STRIPE_KEY"]     # names only: the values are never in this file
```

Secrets are set with `wrangler secret put` (or in `.dev.vars` locally). Naming the ones the app can't run without lets `doko dev` warn, `doko doctor` report and `doko deploy` refuse when one is missing. `SESSION_SECRET` is added to the list by itself when the app has a session store.

No `--env` means `dev`, and `[env.dev.config]` applies there. `ctx.env_name()` gives the active env. Use `ctx.var(name)` for plain vars, and `ctx.secret(name)` for secrets set with `wrangler secret put` (or `.dev.vars` locally). Never put secrets in doko.toml.

## Other wrangler settings

doko writes `wrangler.toml`, so a wrangler setting doko has no table of its own for goes under `[wrangler]`. It is copied into `wrangler.toml` as written, with wrangler's own key names:

```toml
[wrangler]
workers_dev = false
compatibility_flags = ["nodejs_compat"]

[wrangler.vars]                  # plain vars, read with ctx.var("SUPPORT_EMAIL")
SUPPORT_EMAIL = "help@example.com"

[wrangler.assets]                # static files served from ./public
directory = "public"

[env.prod.wrangler]              # deep-merged over [wrangler] for `--env prod`
routes = [{ pattern = "app.example.com", custom_domain = true }]
```

- **Every env gets the merged result**, like bindings do, so a service binding or var declared once under `[wrangler]` is not lost in `prod`. Tables are merged key by key; arrays and plain values are replaced.
- **Logs are on.** doko writes `[observability] enabled = true` unless `[wrangler.observability]` says otherwise.
- **What doko already writes can't be set here:** the name, bindings that have a doko table (`[kv.*]`, `[d1.*]` and the rest), `triggers`, `migrations`, and vars starting with `DOKO_`. doko.toml is rejected with a message naming the key.
- Write var values as strings: `ctx.var` reads text.
