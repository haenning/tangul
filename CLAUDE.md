# tangul

A light, fast database viewer (Postgres, MySQL) in Rust with GPUI (`gpui-kit`).
Scope, design and planned work: [CONTRIBUTING.md](CONTRIBUTING.md), [PLAN.md](PLAN.md),
[ROADMAP.md](ROADMAP.md). No schema editing, no editing cells in the grid.

## Layout

- `crates/core`: shared types, no I/O
- `crates/drivers`: Postgres / MySQL sessions (text protocol)
- `crates/engine`: tokio backend thread; front ends talk to it only via `Command` / `Event`
  (streamed into tabs) or `EngineHandle::query` (the whole result, for everything else).
  Its gate (`gate.rs`) is the only safety gate; see **Safety rules** below.
- `crates/assist`: SQL logic without UI (lexer, analysis, completion, filters); put
  testable logic here
- `crates/tunnel`: SSH tunnels
- `crates/data`: everything stored on disk, no UI (profiles and Keychain, `local.db`,
  `metrics.db`, `settings.json`)
- `crates/service`: what every front end shares, no UI: open connections by purpose
  (the assistant uses the database's optional assistant user), scheduled bookmarks
- `crates/app`: the GPUI app

## Safety rules (user and AI)

Architectural constraints. Don't weaken, bypass or work around them without the owner's
explicit OK, and keep them covered by tests.

- **One gate.** Every statement to a database passes the engine's gate
  (`crates/engine/src/gate.rs`). Front ends pass only a `Caller` (User, Schedule,
  Assistant) and an `Approval`; they never decide read-only mode, the time limit or
  approval themselves. No command, flag or setting skips the gate. The engine reads
  `settings.json` itself.
- **User (tabs).** In write mode the user writes freely; the "UPDATE/DELETE without
  WHERE" dialog stays (`Approval::Confirmed`). Read-only mode of a database user is lifted
  only in the settings, never by a dialog. Tabs (`Command::Execute`) are the user's only.
- **Schedules.** Only statements on the read allow-list (`assist::guard::not_reads`);
  anything else is refused. Reads run like the assistant's.
- **Assistant reads.** Only allow-listed reads run without approval, through
  `EngineHandle::query` (never on a tab's session), in `Mode::RolledBack`: session
  read-only, inside `START TRANSACTION READ ONLY`, always rolled back.
- **Assistant writes** (anything not on the allow-list) are held. The engine issues a
  random single-use `Pass`, bound to the connection and the exact SQL, valid 10 minutes.
  - The pass is sent only after a person clicked approve. It is never given to the
    model or an MCP client, which could otherwise approve its own writes.
  - An approved write runs in `Mode::OwnSession`: a new session, closed afterwards. The
    pooled sessions never allow the assistant to write.
  - In read-only mode the assistant can't write at all, approved or not.
- **Assistant database user.** A database may name its own user for the assistant
  (recommended: SELECT-only role). An approved write runs as the user of the connection
  it was sent on. On MySQL some statements commit implicitly, so the role's grants are
  the real protection there.
- **Everyone.** `safety.max_query_time` applies to every caller, set on the server and
  enforced by the engine's timer.
- **Audit.** Assistant and schedule runs (ran, held, refused) go to the `audit` table in
  `local.db`.
- **Tests.** Changes to the gate, guard, pool or drivers come with tests, including
  live ones (`crates/engine/tests/live.rs`, e.g. `assistant_checks`: a write hidden in
  a read must fail). Aim for ≥ 95% line coverage in core, drivers, engine, assist,
  tunnel, data and service (`.github/workflows/coverage.yml`).

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
