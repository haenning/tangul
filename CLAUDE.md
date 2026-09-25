# tangul

A light, fast database viewer (Postgres, MySQL) in Rust with GPUI (`gpui-kit`).
Scope, design and planned work: [CONTRIBUTING.md](CONTRIBUTING.md), [PLAN.md](PLAN.md),
[ROADMAP.md](ROADMAP.md). No schema editing, no editing cells in the grid.

## Layout

- `crates/core`: shared types, no I/O
- `crates/drivers`: Postgres / MySQL sessions (text protocol)
- `crates/engine`: tokio backend thread; front ends talk to it only via `Command` / `Event`
  (streamed into tabs) or `EngineHandle::query` (the whole result, for everything else).
  Its gate (`gate.rs`) is the only safety gate: every statement passes it with a `Caller`
  and an `Approval`; it reads `settings.json` itself. Users in write mode write freely;
  schedules and the assistant run allow-listed reads in a read-only transaction that
  is rolled back; an assistant write waits for a single-use pass bound to connection and
  SQL, then runs on a new session that is closed afterwards. Tabs are the user's only. Assistant and schedule runs are
  audited in `local.db`
- `crates/assist`: SQL logic without UI (lexer, analysis, completion, filters); put
  testable logic here
- `crates/tunnel`: SSH tunnels
- `crates/data`: everything stored on disk, no UI (profiles and Keychain, `local.db`,
  `metrics.db`, `settings.json`)
- `crates/service`: what every front end shares, no UI: open connections by purpose
  (the assistant uses the database's optional assistant user), scheduled bookmarks
- `crates/app`: the GPUI app

## Commands

- `make run-dev`: dev databases (Docker) plus the app with dev-only profiles
- `make test`, `make test-live` (against the dev databases)
- `make lint` before committing: rustfmt, clippy with warnings as errors, cargo-deny,
  cargo-machete

## Git

- All code goes through `dev`. Branch off `dev`, and open pull requests against `dev`
  (or a branch that targets `dev`), never against `main`. Only `dev` → `main` targets
  `main`.
- Issues close when their commit reaches `main`, so put `Closes #N` in the commit
  message. In a pull request into `dev` it has no effect.
- CI runs only when started by hand (Actions → CI → Run workflow).
