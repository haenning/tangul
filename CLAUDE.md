# tangul

A light, fast database viewer (Postgres, MySQL) in Rust with GPUI (`gpui-kit`).
Scope, design and planned work: [CONTRIBUTING.md](CONTRIBUTING.md), [PLAN.md](PLAN.md),
[ROADMAP.md](ROADMAP.md). No schema editing, no editing cells in the grid.

## Layout

- `crates/core`: shared types, no I/O (incl. chart settings and learned usage as stored)
- `crates/drivers`: sessions per database (text protocol); a Cargo feature each
- `crates/engine`: tokio backend thread; front ends talk to it only via `Command` / `Event`
  (streamed into tabs) or `EngineHandle::query` (the whole result, for everything else).
  Its gate (`gate.rs`) is the only safety gate; see **Safety rules** below.
- `crates/assist`: SQL logic without UI or I/O (lexer, dialects, analysis, completion,
  filters); put testable logic here
- `crates/tunnel`: SSH tunnels
- `crates/data`: everything stored on disk, no UI (profiles and Keychain, `local.db`,
  `metrics.db`, `settings.json`)
- `crates/service`: what every front end shares, no UI: open connections by purpose
  (the assistant uses the database's optional assistant user), scheduled bookmarks,
  and `knowledge`: the one pool of what is known about the databases (schema per
  user, usage per database). The editor, schema tree, results and the assistant all
  read it; don't keep copies elsewhere
- `crates/ai`: the assistant's tools (`Tool`, `Registry`), each written once; MCP and
  provider adapters map the registry generically. `Ctx` is the one security layer
  for every caller: only exposed connections and their opt-ins; SQL only through the
  gate as `Caller::Assistant`. Tests call tools through the registry and compare
  exact output (hand-built schemas, and SQLite files with the real engine).
  `mcp.rs`: the MCP server (rmcp, Streamable HTTP on `127.0.0.1`, bearer token,
  requests with an `Origin` refused); it builds a `Ctx` per call from a `CtxSource`
  and records each call through an `AuditSink`
- `crates/app`: the GPUI app
  - `workspace/`: layout and wiring; `sidebar.rs`: the sidebars left and right of the
    content (one `Sidebar` each, hosting a list of `Panel`s) and the panels' state;
    `value_panel.rs`: the value viewer (right sidebar): the inspected cell (A), the
    compared one (B) and their diff; logic in `assist::value`; `tabs.rs`: the kinds of tab (`TabItem`: query,
    file; boards later) and the open ones; `mcp.rs`: runs the MCP server while
    `mcp.enabled` is set, offers the connected databases with `ai_tools` on (each on
    its assistant connection), its token in the Keychain, its status bar plug
  - `query_tab/`: the SQL editor (`SqlEditor`) over a `ResultView`
  - `result_view/`: a result as grid or chart, with filters, sort and paging; knows
    nothing about tabs, so boards and the assistant can reuse it
  - `query_router.rs`: engine answers to the tab waiting for them;
    `connection.rs`: open connections shared by tabs

Crates depend only as far as they must; keep it that way:

```
core     ← nothing
assist   ← core
drivers  ← core
tunnel   ← core
data     ← core
engine   ← core, assist, data, drivers, tunnel
service  ← core, assist, data, engine
ai       ← core, assist, engine, service
app      ← core, assist, data, engine, service, ai   (never drivers or tunnel)
```

## Databases

Only four places name a concrete database; everything else asks the connection's
dialect (`assist::dialect::Dialect`) or driver and never matches on `DriverKind`:

- `core::DriverKind`: the list, label, default port
- `crates/drivers/src/<db>.rs` + `driver_for`: connecting and running (I/O): read-only,
  time limit, cancel, catalog queries, error positions
- `crates/assist/src/dialect/<db>.rs` + `dialect_for`: the SQL text: quoting, literals,
  `LIMIT`, text matching, date arithmetic, keywords, functions, extra syntax, functions
  with side effects
- `crates/app/src/connection_form.rs`: which databases are offered

Adding one: a `DriverKind` variant, a driver (with its Cargo feature) and a dialect,
each with tests, plus live tests in `crates/engine/tests/live.rs`. The compiler points
to the rest. Check the **Safety rules** against it: the gate relies on a read-only
transaction and a server-side time limit where the database has them.

## Safety rules (user and AI)

Architectural constraints. Don't weaken, bypass or work around them without the owner's
explicit OK, and keep them covered by tests.

- **One gate.** Every statement to a database passes the engine's gate
  (`crates/engine/src/gate.rs`). Front ends pass only a `Caller` (User, Schedule,
  Assistant) and an `Approval` (the tab dialog's go-ahead); they never decide
  read-only mode or the time limit themselves. No command, flag or setting skips the gate. The engine reads
  `settings.json` itself.
- **User (tabs).** In write mode the user writes freely; the "UPDATE/DELETE without
  WHERE" dialog stays (`Approval::Confirmed`). Read-only mode of a database user is lifted
  only by changing the settings (the read-only dialog's "Allow writes" does that, and
  the statement then has to be run again); a go-ahead never runs a write past it. Tabs (`Command::Execute`) are the user's only.
- **Schedules.** Only statements on the read allow-list (`assist::guard::not_reads`);
  anything else is refused. Reads run like the assistant's.
- **Assistant reads.** Only allow-listed reads run, through
  `EngineHandle::query` (never on a tab's session), in `Mode::RolledBack`: session
  read-only, inside `START TRANSACTION READ ONLY`, always rolled back.
- **Assistant writes** (anything not on the allow-list) never run for the assistant,
  whatever approval comes along. The gate returns them as a `Proposal`; `propose_sql`
  opens statements in a new tab, unrun, on the user's own connection, where the user
  runs them as themselves (`Caller::User`, with the user's rules). There is no path that
  lets the assistant, the model or an MCP client execute a write.
- **Assistant database user.** A database may name its own user for the assistant
  (recommended: the SELECT-only role the database form sets up). The assistant's reads
  run on its own connection as that user. On MySQL some statements commit implicitly,
  so the role's grants are the real protection there.
- **Secrets.** Passwords in SQL (`… PASSWORD '…'`, `IDENTIFIED BY '…'`) are replaced by
  `assist::redact` before history, bookmarks and the audit log.
- **Everyone.** `safety.max_query_time` applies to every caller, set on the server and
  enforced by the engine's timer.
- **Audit.** Assistant and schedule runs (ran, proposed, refused) and every AI tool
  call (caller `mcp`) go to the `audit` table in `local.db`.
- **Tests.** Changes to the gate, guard, pool or drivers come with tests, including
  live ones (`crates/engine/tests/live.rs`, e.g. `assistant_checks`: a write hidden in
  a read must fail). Aim for ≥ 95% line coverage in core, drivers, engine, assist,
  tunnel, data, service and ai (`.github/workflows/coverage.yml`).

## Commands

- `make run-dev`: dev databases (Docker) plus the app with dev-only profiles
- `make test`, `make test-live` (against the dev databases)
- UI tests (`crates/app/src/workspace/ui_tests.rs`): the real workspace in a headless
  window, driven by clicks and keys via gpui-kit's harness (`test-support`), with
  `Data::in_memory()` and `tangul_engine::detached()`. Give elements you want to find
  an id and `.test_support()` (a no-op outside tests)
- `make lint` before committing: rustfmt, clippy with warnings as errors, cargo-deny,
  cargo-machete

## Git

- All code goes through `dev`. Branch off `dev`, and open pull requests against `dev`,
  never against `main`. Only `dev` → `main` targets `main`.
- **One branch and one pull request per feature or feature set**, not per commit.
  Commits make the branch reviewable step by step: one per finished part (an issue,
  a refactor), each passing `make test` and `make lint`, pushed as it is done. Don't
  stack pull requests on each other.
- Issues close when their commit reaches `main`, so put `Closes #N` in the commit
  message. In a pull request into `dev` it has no effect.
- CI runs only when started by hand (Actions → CI → Run workflow).
