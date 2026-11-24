# Contributing to doko

Thanks for helping fill the basket. Bug reports, fixes, docs and new features are all welcome.

## Reporting issues

Open an issue at <https://github.com/wheregmis/doko/issues>. For a bug, include your doko and `wrangler` versions, the relevant part of `doko.toml`, and what you expected to happen.

For a larger change, open an issue first, so we can agree on the shape before you write the code.

## Development setup

```bash
rustup target add wasm32-unknown-unknown
git clone https://github.com/wheregmis/doko
cd doko
cargo build --workspace
```

The workspace has two published crates and one example:

- `crates/doko` (published as `doko-rs`, imported as `doko`): the framework library.
- `crates/doko-cli` (published as `doko-cli`, installs the `doko` binary): the command line tool and its generators.
- `examples/hello`: an example app, also used by the integration tests.

## Checks

CI runs these on every pull request. Run them locally before you push:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo clippy -p doko-rs -p hello --all-features --locked --target wasm32-unknown-unknown -- -D warnings
cargo test --workspace --all-features --locked
```

The integration tests that start `wrangler dev` are `#[ignore]`d in CI because they need Node and `wrangler`. Run them locally with `doko test`.

## Pull requests

- Keep each pull request focused on one change, and add tests for new behaviour.
- Update the README when you add or change a user-facing feature.
- Write the pull request title as a [conventional commit](https://www.conventionalcommits.org), because it becomes the commit message on `master` and decides the release.

## Releases

Releases are cut by [release-please](https://github.com/googleapis/release-please) from conventional commits on `master`:

- `fix: ..` makes a patch release and `feat: ..` a minor one. A breaking change, `feat!: ..`, also makes a minor release while the version is below 1.0. `docs:`, `chore:`, `ci:`, `test:` and `refactor:` commits don't make a release.
- release-please keeps a release pull request open with the next version and the changelog. Merging that pull request tags the commit, creates the GitHub release, and publishes `doko-rs` and `doko-cli` to crates.io.
- Both crates share one version, set in the root `Cargo.toml`.

## License

By contributing, you agree that your contributions are dual licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option, as described in the [README](README.md#license).
