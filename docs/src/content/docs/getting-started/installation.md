---
title: "Installation"
description: "Install the doko CLI and create your first Cloudflare Worker."
---

```bash
rustup target add wasm32-unknown-unknown
cargo install doko-cli
doko new my-app && cd my-app && doko dev
```

The library is published as `doko-rs` (the name `doko` was taken) and imported as `doko`; `doko new` writes the dependency for you:

```toml
doko = { package = "doko-rs", version = "0.1" }
```

To run the example from a checkout instead: `cargo install --path crates/doko-cli && cd examples/hello && doko dev`.

`doko dev` / `doko deploy` regenerate `wrangler.toml` from `doko.toml`, then run wrangler. `doko config` only regenerates. Extra args go to wrangler.
