# Contributing to tangul

Thanks for your interest! Design and decisions are in [PLAN.md](PLAN.md), planned work
in [ROADMAP.md](ROADMAP.md).

## Scope

tangul stays a light, fast viewer: look at data, understand it, investigate it. Some
things will not be added, so please don't open pull requests for them:

- table designers, `CREATE` / `ALTER` dialogs or any other schema editing: that belongs
  to migration tools such as [golang-migrate](https://github.com/golang-migrate/migrate);
- editing cells in the grid: changing data stays explicit SQL.

Feature requests are welcome through the issue template.

## License and CLA

tangul is licensed under [AGPL-3.0-only](LICENSE). Before your first pull request can be
merged, you agree to the [Contributor License Agreement](CLA.md) by adding this line to
the pull request description (the template has it):

> I have read the CLA (CLA.md) and agree to it for this and all my future contributions.

The CLA lets the maintainer also distribute tangul under other terms, for example in the
Mac App Store, whose terms don't fit the AGPL. You keep the copyright in your work.

## Requirements

- rustup (Rust 1.98.1 is installed automatically from `rust-toolchain.toml`, same as
  Zed).
- Docker, only for the dev databases.
- macOS: Xcode's Metal toolchain, which GPUI uses to compile shaders. Newer Xcode
  versions don't ship it by default: `xcodebuild -downloadComponent MetalToolchain`.
- Linux builds (dev only) need `libxkbcommon-dev libxkbcommon-x11-dev`.

## Quick start

```sh
make setup     # checks rustup, docker and (macOS) the Metal toolchain
make run-dev   # starts dev Postgres + MySQL with sample data and opens the app
```

`make run-dev` uses its own profile file (`target/dev-config/`) with two ready-made,
passwordless connections to the dev databases, so your real profiles stay untouched.
Use `make run` / `make run-release` for normal use.

## Make targets

Run `make` to list all of them. The common ones:

| Target | What it does |
|---|---|
| `make db-up` / `db-down` / `db-reset` | Dev Postgres on `127.0.0.1:55432`, MySQL on `127.0.0.1:53306` (seed: `dev/seed/`) |
| `make psql` / `make mysql` | Shell into a dev database |
| `make test` | Unit tests |
| `make test-live` | Integration tests against the dev databases |
| `make lint` | rustfmt check, clippy (warnings as errors), cargo-deny, cargo-machete |
| `make license` | Checks licenses (own crates AGPL-3.0-only, dependencies allowed by `deny.toml`) and regenerates `THIRD_PARTY_LICENSES.md`; commit it when it changes |
| `make tools` | Installs cargo-deny, cargo-machete and cargo-about |
| `make install` / `make dmg` | Build the macOS app (and DMG), see [INSTALL.md](INSTALL.md) |
| `make ci` | Everything CI runs |

Lint rules shared by all crates are in `[workspace.lints]` (root `Cargo.toml`);
dependency policy (advisories, licenses, sources) is in `deny.toml`.

The dev databases use non-default ports bound to localhost, so they don't clash with a
local Postgres or MySQL.

## Branches and CI

Work happens on `dev`; pull requests into `main` are opened by hand. CI
(`.github/workflows/ci.yml`) runs on every push to `dev` and `main` and on pull
requests, on Linux: format, clippy and dependency checks in one job; every crate's
tests (live database and headless UI tests included) once, with coverage, in the
other. On `dev` it publishes the report and badges to the `badges` branch; see
[docs/TESTING.md](docs/TESTING.md). Releases run it before they build. Pull requests
into `main` and releases run `make license-check` (`.github/workflows/licenses.yml`).

## Releases

Merge `dev` into `main` first (its pull request), then run `make release`. It
1. switches to `main` if you aren't on it (after asking) and pulls,
2. asks for the version: higher than the current one, with its tag and release free,
3. bumps `version` in the workspace `Cargo.toml` on `main`, commits, tags it
   `vX.Y.Z` and pushes both,
4. merges `main` into `dev`, so `dev` has the version too.

The tag starts the Release workflow: it runs CI, builds the universal DMG, signs it
with the Developer ID certificate (hardened runtime), notarizes and staples it,
verifies all of that and publishes it as a GitHub release with generated notes. It
checks that the tag matches the `Cargo.toml` version.

Signing needs these secrets in the `release` environment (Settings → Environments →
`release`, deployments only from tags `v*`): `MACOS_CERTIFICATE_P12` (the exported
`.p12`, base64), `MACOS_CERTIFICATE_PASSWORD`, `MACOS_SIGN_IDENTITY`
(`Developer ID Application: Name (TEAMID)`), `NOTARY_API_KEY_P8` (the App Store
Connect API key's `.p8`), `NOTARY_KEY_ID` and `NOTARY_ISSUER_ID`. Without them the job
fails before building, unless the repository variable `UNSIGNED_RELEASE` is `true`
(ad-hoc signed, as before). `scripts/release-keychain.sh` puts the certificate into a
temporary keychain and removes it at the end; `scripts/notarize-macos.sh` notarizes.
The bundle id `io.github.haenning.tangul` must never change: macOS ties Keychain
access to it and the Team ID. The internal crates depend on each other by path only, so the workspace
version is the one place it is set.

The app icon is `packaging/macos/tangul.icon`, an Icon Composer document (open it with
Icon Composer from Xcode 26). macOS draws the glass and the light/dark background itself.
`scripts/bundle-macos.sh` compiles it with `actool`; without Xcode 26 it falls back to
`packaging/macos/icon.png`. The icon has three flat layers, the two strands of a helix
and a bar, with no highlights of their own; their source SVGs are in
`packaging/macos/icon-src/`. `packaging/macos/tangul-mono.icon` is a grey alternative.
