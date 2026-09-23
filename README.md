# rusdb

Fast, minimal database viewer for macOS. Rust + GPUI (Zed's UI framework).
PostgreSQL and MySQL. Design and decisions: [PLAN.md](PLAN.md).

![rusdb](docs/screenshot.png)

## Run

```sh
cargo run --release
```

Requires Rust 1.98 (pinned via `rust-toolchain.toml`, rustup installs it automatically).

- `+` in the sidebar adds a connection (password goes to the macOS Keychain).
- Click a connection to connect and use it in the current tab; click a table to query it.
- `⌘↵` runs the selection or the whole editor, `⌘T` new tab, `⌘W` close tab.

## Test

```sh
docker compose up -d
RUSDB_TEST_PG=postgres:postgres@127.0.0.1:5432/postgres \
RUSDB_TEST_MYSQL=rusdb:rusdb@127.0.0.1:3306/app \
cargo test --workspace
```

Linux builds (dev only) need `libxkbcommon-dev libxkbcommon-x11-dev`.
