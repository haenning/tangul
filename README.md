# rusdb

Fast, minimal database viewer for macOS. Rust + GPUI (Zed's UI framework).
PostgreSQL and MySQL. Design and decisions: [PLAN.md](PLAN.md).

![rusdb](docs/screenshot.png)

## Quick start

```sh
make setup     # checks rustup, docker and (macOS) the Metal toolchain
make run-dev   # starts dev Postgres + MySQL with sample data and opens the app
```

`make run-dev` uses its own profile file (`target/dev-config/`) with two ready-made,
passwordless connections to the dev databases, so your real profiles stay untouched.
Use `make run` / `make run-release` for normal use.

Requirements:
- rustup (Rust 1.98.1 is installed automatically from `rust-toolchain.toml`, same as Zed).
- Docker (only for the dev databases).
- macOS: Xcode's Metal toolchain, which GPUI uses to compile shaders. Newer Xcode versions
  don't ship it by default: `xcodebuild -downloadComponent MetalToolchain`.

## Using it

- `+` in the sidebar adds a connection; the password goes to the macOS Keychain.
- Click a connection to connect and use it in the current tab; click a table to query it.
- `⌘↵` runs the selection or the whole editor, `⌘T` new tab, `⌘W` close tab.

## Development

Run `make` to list all targets. The common ones:

| Target | What it does |
|---|---|
| `make db-up` / `db-down` / `db-reset` | Dev Postgres on `127.0.0.1:55432`, MySQL on `127.0.0.1:53306` (seed: `dev/seed/`) |
| `make psql` / `make mysql` | Shell into a dev database |
| `make test` | Unit tests |
| `make test-live` | Integration tests against the dev databases |
| `make lint` | `cargo fmt --check` + clippy with warnings as errors |
| `make ci` | Everything CI runs |

The dev databases use non-default ports bound to localhost, so they don't clash with a
local Postgres/MySQL. CI (`.github/workflows/ci.yml`) runs the backend with live database
tests on Linux and builds the app on macOS.

Linux builds (dev only) need `libxkbcommon-dev libxkbcommon-x11-dev`.

## License

[Apache-2.0](LICENSE)
