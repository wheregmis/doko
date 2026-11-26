---
title: "Auth"
description: "Cookie sessions, JWTs, password hashing and require_auth."
---

`doko::auth` is wired by name, so there is nothing to configure in code:

| Feature | Needs |
|---|---|
| Sessions | secret `SESSION_SECRET`, and `[kv.SESSIONS]` in doko.toml |
| JWTs (HS256, issued by you) | secret `JWT_SECRET` |
| JWTs (RS256, issued elsewhere) | secret `JWT_PUBLIC_KEY` (the issuer's public key as PEM), or `jwks_url` below |

Set secrets with `wrangler secret put`, or in `.dev.vars` locally. The optional `[auth]` table tunes the rest, and `[env.<name>.auth]` overrides it per key:

```toml
[auth]
session_ttl_secs = 86400            # default: 7 days
session_cookie = "sid"              # default: doko_session
jwt_issuer = "https://id.example"   # required `iss`; auth::sign adds it
jwt_audience = "my-api"             # required `aud`; auth::sign adds it
jwks_url = "https://id.example/.well-known/jwks.json"   # RS256 keys, picked by the token's `kid`
```

```rust
use doko::{RouterExt, auth::{self, Claims, Session, SetCookie}};

Router::new()
    .route("/me", get(me))
    .require_auth()                       // 401 without a session or a valid bearer JWT
    .route("/login", post(login))
    .route("/logout", post(logout))
    .route("/api/me", get(api_me))

async fn login(ctx: Ctx, session: Session, Json(c): Json<Credentials>) -> Result<(SetCookie, Json<Value>)> {
    let user = find_user(&ctx, &c.username).await?.ok_or_else(Error::unauthorized)?;
    if !auth::verify_password(&c.password, &user.password_hash).await? {
        return Err(Error::unauthorized());
    }
    let token = auth::sign(&ctx, &user, 3600).await?;                 // HS256 JWT, valid for an hour
    Ok((session.set(&user).await?, Json(json!({ "token": token }))))  // return the cookie
}

async fn logout(session: Session) -> Result<SetCookie> { session.clear().await }

async fn me(session: Session) -> Result<Json<User>> {
    session.get()?.map(Json).ok_or_else(Error::unauthorized)
}

async fn api_me(Claims(user): Claims<User>) -> Json<User> { Json(user) }   // 401 unless the token is valid
```

- **Sessions.** The cookie (`HttpOnly; Secure; SameSite=Lax`) holds a random id signed with `SESSION_SECRET`; the data lives in KV for 7 days. `session.set` and `session.clear` return a `SetCookie` that must go out with the response. `set` always issues a new id.
- **Strongly consistent sessions.** KV can serve a session for up to a minute after logout in other regions. If that matters, declare `[do.SESSIONS] class_name = "SessionStore"` instead of `[kv.SESSIONS]` and export it: `doko::app!(App, objects: [SessionStore])`.
- **JWTs.** `Claims<T>` reads `Authorization: Bearer`, checks the signature, `exp`, `nbf` and the configured `iss` / `aud`, and deserializes the claims into `T`. `auth::sign` adds `iat` and `exp`. Each algorithm has its own key, so a token can't choose HS256 to be checked against the RS256 public key.
- **JWKS.** With `jwks_url`, RS256 tokens are checked against the key their `kid` names. The key set is kept in memory for an hour; an unknown `kid` refetches it, at most once a minute, so rotated keys are picked up without a deploy. Set `jwt_issuer` too, or any tenant of a shared identity provider can mint tokens you accept.
- **Roles.** `.require_role("admin")` is `.require_auth()` plus a 403 unless the session value or the JWT claims carry the role, as `"role": "admin"` or `"roles": ["admin", ..]`. What roles exist and who has them is up to the app.
- **Signed tokens.** `auth::token(&ctx, "reset-password", &user.id, 3600).await?` makes a tamper-proof token for a link in a mail, and `auth::read_token::<i64>(&ctx, "reset-password", &token).await?` gives the data back, or `None` if the token is forged, expired or was made for another purpose. Tokens are signed with `SESSION_SECRET` and can't pass as a session cookie or a JWT. Their data is readable, so carry ids, not secrets.
- **Passwords.** `auth::hash_password` and `verify_password` use PBKDF2-SHA256 with a random salt at 100,000 iterations (the Workers maximum), in the runtime's native crypto.
- **`.require_auth()`** applies to the routes added before it. Put `.rate_limit(..)` on login and signup routes: see [examples/hello/src/controllers/auth.rs](https://github.com/wheregmis/doko/blob/master/examples/hello/src/controllers/auth.rs).
