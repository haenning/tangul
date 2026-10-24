# Contributing to tangul

Thanks for your interest! Design and decisions are in [PLAN.md](PLAN.md), planned work
in [ROADMAP.md](ROADMAP.md).

## Scope

tangul stays a light, fast viewer: look at data, understand it, investigate it. Some
things will not be added, so please don't open pull requests for them:

- table designers, `CREATE` / `ALTER` dialogs or any other schema editing: that belongs
  to migration tools such as [golang-migrate](https://github.com/golang-migrate/migrate);
- editing cells in the grid: changing data stays explicit SQL.

Feature requests are welcome through the issue template. Security problems go through
private reporting instead: see [SECURITY.md](SECURITY.md).

## License and CLA

tangul is licensed under [AGPL-3.0-only](LICENSE). Before your first pull request can be
merged, you agree to the [Contributor License Agreement](CLA.md) by commenting this line
on the pull request (a bot asks for it and records it; once is enough):

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
| `make demo` | Records `docs/demo.webp`, the README animation: the real app on the dev Postgres, driven by `crates/app/src/workspace/demo.rs` (macOS, needs `img2webp`). Run it again when the UI changes |
| `make ci` | Everything CI runs |

Lint rules shared by all crates are in `[workspace.lints]` (root `Cargo.toml`);
dependency policy (advisories, licenses, sources) is in `deny.toml`.

The dev databases use non-default ports bound to localhost, so they don't clash with a
local Postgres or MySQL.

## Branches and CI

All code goes through `dev`: branch off `dev` and open pull requests into `dev`. Only
`dev` → `main` goes into `main`, plus the version bump of a release. CI
(`.github/workflows/ci.yml`) tests every pull request into `dev` and `main` as it
would be merged, once, on Linux: format, clippy and dependency checks in one job;
every crate's tests (live database and headless UI tests included), with coverage, in
the other; the updater on macOS in a third. A passing `dev` → `main` pull request
publishes the report and badges to the `badges` branch; see
[docs/TESTING.md](docs/TESTING.md). Merging and releasing don't test again (a push to
`main` only builds, for the cache). Pull requests
into `main` and releases run `make license-check` (`.github/workflows/licenses.yml`).

## Releases

Merge `dev` into `main` (a pull request) with what goes into the release first, then
run `make release`. It
1. switches to `main` if you aren't on it (after asking) and pulls,
2. asks for the version: higher than the current one, with its tag and release free,
3. bumps `version` in the workspace `Cargo.toml` on `main`, commits, tags it
   `vX.Y.Z` and pushes both,
4. merges `main` into `dev` and pushes it, so `dev` has the new version too.

The tag starts the Release workflow: it runs CI, builds the universal DMG (with
tangul's volume icon), signs the app and the DMG with the release certificate (the
updater checks both; a Developer ID also gets the hardened runtime), notarizes and
staples it when that is a Developer ID,
verifies all of that and publishes it as a GitHub release with generated notes. It
checks that the tag matches the `Cargo.toml` version.

Signing needs these secrets in the `release` environment (Settings → Environments →
`release`, deployments only from tags `v*`): `MACOS_CERTIFICATE_P12` (the `.p12`,
base64), `MACOS_CERTIFICATE_PASSWORD` and `MACOS_SIGN_IDENTITY` (the certificate's
name). Two kinds of certificate:

- **Self-signed** (until there is an Apple Developer ID):
  `scripts/release-signing-cert.sh tangul-release.p12` creates it and prints the
  three secrets. Every release then has the same signature, so the Keychain's "Always
  Allow" survives updates; Gatekeeper still asks on each downloaded update. Keep the
  `.p12`: a new certificate makes every user answer the Keychain again.
- **Developer ID** (`Developer ID Application: Name (TEAMID)`): also needs
  `NOTARY_API_KEY_P8` (the App Store Connect API key's `.p8`), `NOTARY_KEY_ID` and
  `NOTARY_ISSUER_ID`; the release is notarized and stapled and opens without the
  Gatekeeper step. Switching to it asks every user once more (the certificate
  changes), then never again.

Without the secrets the job fails before building, unless the repository variable
`UNSIGNED_RELEASE` is `true` (ad-hoc signed, as before). `scripts/release-keychain.sh` puts the certificate into a
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
