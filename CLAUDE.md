# tangul

A light, fast database viewer (Postgres, MySQL) in Rust with GPUI (`gpui-kit`).
Scope, design and planned work: [CONTRIBUTING.md](CONTRIBUTING.md), [PLAN.md](PLAN.md),
[ROADMAP.md](ROADMAP.md). No schema editing, no editing cells in the grid.

## Layout

- `crates/core`: shared types, no I/O
- `crates/drivers`: Postgres / MySQL sessions (text protocol)
- `crates/engine`: tokio backend thread; front ends talk to it only via `Command` / `Event`
  (streamed into tabs) or `EngineHandle::query` (the whole result, for everything else)
- `crates/assist`: SQL logic without UI (lexer, analysis, completion, filters); put
  testable logic here
- `crates/tunnel`: SSH tunnels
- `crates/data`: everything stored on disk, no UI (profiles and Keychain, `local.db`,
  `metrics.db`, `settings.json`)
- `crates/service`: what every front end shares, no UI: open connections by purpose,
  the policy that decides whether and how a statement runs (the only safety gate;
  schedules and the assistant only read), scheduled bookmarks
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
