---
title: "Generators"
description: "Scaffold controllers, jobs, mailers, resources, auth and migrations."
---

```bash
doko generate controller posts
```

This writes `src/controllers/posts.rs` and registers it in `src/controllers/mod.rs`. Add `mod controllers;` once, and `.merge(controllers::routes())` in `Hooks::routes`.

```bash
doko generate job send_welcome
```

This writes `src/jobs/send_welcome.rs` (a `SendWelcome` job) and registers it in `src/jobs/mod.rs`. Add `mod jobs;` once, and return `jobs::jobs()` from `Hooks::jobs`.

```bash
doko generate mailer welcome
```

This writes `src/mailers/welcome.rs`, a function that builds a `Message`, and declares it in `src/mailers/mod.rs`. Add `mod mailers;` once. See [Email](/doko/guides/email/).

```bash
doko generate scaffold post title:string body:text published:bool
```

This writes a whole resource. Then `doko migrate && doko dev`.

| File | What |
|---|---|
| `migrations/NNNN_create_posts.sql` | the `posts` table: your fields plus `id`, `created_at`, `updated_at` |
| `src/models/post.rs` | `Post`, `PostInput` with its validation, and `Post::page`, `find`, `create`, `update`, `delete` as plain SQL |
| `src/controllers/posts.rs` | `GET /posts` (paginated), `POST /posts`, `GET`, `PUT` and `DELETE /posts/{id}` |
| `tests/posts.rs` | the five routes as an integration test |

- **Field types:** `string`, `text`, `int`, `float`, `bool`. Every field is `NOT NULL`; text fields get a "must not be empty" rule to start from.
- **No ORM.** The model is SQL you can read and change, and the generator runs once: later changes are yours, in a new migration and the model file.
- **The routes are public** until you add `.require_auth()`.
- It uses the app's first `[d1.*]` binding, or adds `[d1.DB]`. Plurals are the regular English ones (`category` → `categories`); rename by hand for others.

```bash
doko generate auth
```

This writes a working account system and wires it into the app:

| File | What |
|---|---|
| `src/controllers/auth.rs` | `POST /auth/register`, `GET /auth/verify`, `POST /auth/login`, `POST /auth/logout`, `GET /auth/me`, `POST /auth/forgot-password`, `POST /auth/reset-password` |
| `src/mailers/auth.rs` | the verification and password-reset mail |
| `migrations/NNNN_create_users.sql` | the `users` table |
| `tests/auth.rs` | the flow as an integration test |

It also adds what those need: the `[d1.*]`, `[email.*]`, `[kv.SESSIONS]` and `RateLimiter` bindings it doesn't find in doko.toml, `APP_URL` under `[wrangler.vars]`, a `SESSION_SECRET` in `.dev.vars`, and the modules and `RateLimiter` export in `src/lib.rs`. Whatever it can't add safely it prints as a `todo:` line. Then `doko migrate && doko dev`.

- Register and forgot-password answer the same whether or not the address has an account, and a failed login takes as long as a successful one, so accounts can't be probed.
- Register, login and both password routes are limited to 10 requests a minute per client.
- A reset link works once and for an hour. It points at `APP_URL/reset-password?token=..`: that page is your frontend's, and it POSTs the token and the new password to `/auth/reset-password`.
- Before deploying, run `wrangler secret put SESSION_SECRET` and set `FROM` in the mailer to an address on your mail domain.
- The code is yours after that: the generator runs once and doesn't update it.
