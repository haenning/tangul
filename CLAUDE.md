# tangul

A light, fast database viewer (Postgres, MySQL) in Rust with GPUI (`gpui-kit`).
Scope, design and planned work: [CONTRIBUTING.md](CONTRIBUTING.md), [PLAN.md](PLAN.md),
[ROADMAP.md](ROADMAP.md). No schema editing, no editing cells in the grid.

## Layout

- `crates/core`: shared types, no I/O (incl. chart settings, boards and learned usage as stored)
- `crates/drivers`: sessions per database (text protocol); a Cargo feature each
- `crates/engine`: tokio backend thread; front ends talk to it only via `Command` / `Event`
  (streamed into tabs) or `EngineHandle::query` (the whole result, for everything else).
  Its gate (`gate.rs`) is the only safety gate; see **Safety rules** below.
- `crates/assist`: SQL logic without UI or I/O (lexer, dialects, analysis, completion,
  filters; settings files read with positions and checked value by value: `json`;
  editing content-blocking rules: `ai_rules`; the performance boards' templates:
  `perf`, their SQL per database in `dialect/<db>/perf.rs`, offered by what the server
  has); put testable logic here
- `crates/tunnel`: SSH tunnels
- `crates/data`: everything stored on disk, no UI (profiles and Keychain, `local.db`
  incl. the assistant's chats, `metrics.db`, `settings.json`, color themes: `themes.rs`,
  the built-in ones in `crates/data/themes/`, the user's in the `themes` folder next to
  `settings.json`; boards: `boards.rs`, one JSON file each in the `boards` folder there;
  each database's own settings: `database_settings.rs`, `databases/<id>.json` there;
  `upgrade.rs`: every settings file brought to the current layout at start (its
  `"version"`, a tangul version; `migrations.rs` runs the steps, JSON files in
  `crates/data/migrations/`: see `CLAUDE.md` there; `make release` joins a
  version's dev steps), the user's values kept, the old text as `.bak`; a new key
  goes into the settings' struct and `docs/settings.md`)
- `crates/service`: what every front end shares, no UI: open connections by purpose
  (the assistant uses the database's optional assistant user), scheduled bookmarks,
  `boards` (the one API that changes boards, for the UI and the AI tools, and when a
  board refreshes; every change a new revision), `values` (the editor's quick look at a column's values)
  and `knowledge`: the one pool of what is known about the databases (schema per
  user, usage and most run queries per database, value profiles and the editor's
  quick looks per user). The editor, schema tree, results and the assistant all
  read it; don't keep copies elsewhere
- `crates/llm`: model providers, no UI, nothing about databases or tangul's tools: one
  `Provider` (a streamed answer with tool calls, the models on offer) with an adapter
  per API (`anthropic.rs`, `openai.rs` for every OpenAI-compatible service, `gemini.rs`,
  `ollama.rs`). The caller passes key, messages and tools (`ToolDef`)
- `crates/update`: tangul updating itself from its GitHub releases, no UI: the check
  (`releases/latest`, nothing sent but the request), the DMG (only from this repo's
  release downloads, signed with the app's certificate as `<bundle id>.dmg`, checked
  before it is mounted), and the install: only a real app (no link) meeting the running
  app's designated requirement, which must pin a certificate (bundle id and
  certificate, `codesign --verify --deep --strict -R`, checked again on the copy that
  is installed) and of the announced version replaces it, by renaming; any failure
  leaves the running app as it was. Ad-hoc, translocated or read-only apps only get
  the notice (`workspace/update.rs`, `updates.check`). Every update that must never be
  installed has a test (`install/attacks.rs`, run by CI's macOS job)
- `crates/ai`: the assistant's tools (`Tool`, `Registry`), each written once; MCP and
  provider adapters map the registry generically. `Ctx` is the one security layer
  for every caller: only exposed connections and their opt-ins; SQL only through the
  gate as `Caller::Assistant`. Tests call tools through the registry and compare
  exact output (hand-built schemas, and SQLite files with the real engine).
  Board tools (`tools/boards.rs`) ask the app (`Ctx::with_boards`, a `BoardCall`),
  which answers in `workspace/boards.rs`.
  `mcp.rs`: the MCP server (rmcp, Streamable HTTP on `127.0.0.1`, bearer token,
  requests with an `Origin` refused); it builds a `Ctx` per call from a `CtxSource`
  and records each call through an `AuditSink`.
  `chat`: the in-app assistant's conversation (model via `llm`, re-exported as
  `tangul_ai::llm`; tools through the registry with a `Ctx` as caller `assistant`),
  prose/SQL segments of answers, what is kept of messages (redacted). `runtime`: the
  tokio runtime this work runs on (the app's executor isn't tokio)
- `crates/app`: the GPUI app
  - `workspace/`: layout and wiring; `sidebar.rs`: the sidebars left and right of the
    content (one `Sidebar` each, hosting a list of `Panel`s) and the panels' state;
    `value_panel.rs`: the value viewer (right sidebar): the inspected cell (A), the
    compared one (B) and their diff; logic in `assist::value`; `tabs.rs`: the kinds of tab (`TabItem`: query,
    file, AI settings, board) and the open ones; `database_tabs.rs`: tabs belong to
    their database (switching shows its tabs, the others wait), pinned query and
    board tabs kept per database in `local.db` and restored when it is first shown; `remove_database.rs`: removing a
    database (asked first) with everything kept for it (`Data::remove_database`; the
    audit log stays); `boards.rs`: opening boards, their
    changes (through `service::boards`), "Add to board", a tile's own query tab (from the
    tile, or for a new one; saving there writes the tile, reads only; never pinned), the
    Boards panel; `ai.rs`: what the assistant
    and MCP clients share: the connected databases with `ai_tools` on offered to AI
    tools (each on its assistant connection), a `Ctx` per caller, and what their calls
    hand back (history, proposals, shown results); `mcp.rs`: runs the MCP server while
    `mcp.enabled` is set, its token in the Keychain, its status bar plug (green while
    it runs; opens the MCP settings); `assistant.rs`: the chat's provider and model
    from the settings, its chats in `local.db`, questions run in the editor;
    `ai_settings.rs`: the AI settings tab's state and requests
  - `ai_settings/`: the AI settings tab (pages AI and MCP); knows nothing about the
    workspace: writes `settings.json` itself, asks for the rest with events
  - `assistant/`: the assistant's chat (right sidebar); knows nothing about the
    workspace: runs a conversation it is handed, asks for storage and SQL actions with
    events
  - `theme.rs`: every color of the app. A theme (semantic colors, one JSON file per
    theme, dark or light) becomes gpui-kit's theme there; chart colors (one color:
    shades of it; several: in turn) and the engraved wordmark come from it too.
    `theme.appearance` ("auto" follows the system), `theme.dark` and `theme.light` in
    `settings.json` choose. **No color literal anywhere else** (a test checks it):
    views use `cx.theme()` or `crate::theme`
  - `ui.rs`: small pieces every view uses. Warnings and errors are always an
    `alert_card` (border and icon in the color, background a tenth of it, the reason
    below). Menu options (`menu_choice`) are a short, self-explanatory title; at best
    it says enough and needs nothing more. Only where the title alone doesn't make the
    option clear, a few words below it in the muted color, e.g. "Schema only" /
    "Table contents stay hidden from the AI" (not "Your user" / "Uses your user").
    Longer explanations go behind an (i) (`menu_section`), never as paragraphs.
    Forms: `form_section` / `form_row` / `form_note`, a choice of a few as `segmented`
    (muted track, the chosen one raised); the accent only where it means something,
    switches via `ui::switch` (on: the off color tinted with the accent)
  - `board/`: a board in a tab (header, tiles as `ResultView`s, edit mode with drag and
    drop); knows nothing about the workspace: asks for changes with events, refreshes
    in one batch (`Caller::Board`) only while it is drawn
  - `query_tab/`: the SQL editor (`SqlEditor`) over a `ResultView`; an empty editor shows
    feature hints (`query_tab/hints.rs`). A larger feature or a usability win that
    isn't obvious: propose a hint for it
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
llm      ← core
update   ← nothing
engine   ← core, assist, data, drivers, tunnel
service  ← core, assist, data, engine
ai       ← core, assist, engine, service, llm
app      ← core, assist, data, engine, service, ai, update   (never drivers, tunnel or llm)
```

## Databases

Only four places name a concrete database; everything else asks the connection's
dialect (`assist::dialect::Dialect`) or driver and never matches on `DriverKind`:

- `core::DriverKind`: the list, label, default port
- `crates/drivers/src/<db>.rs` + `driver_for`: connecting and running (I/O): read-only,
  time limit, cancel, catalog queries, error positions
- `crates/assist/src/dialect/<db>.rs` + `dialect_for`: the SQL text: quoting, literals,
  `LIMIT`, text matching, date arithmetic, keywords, functions, extra syntax, functions
  with side effects, performance boards (`dialect/<db>/perf.rs`)
- `crates/app/src/connection_form.rs`: which databases are offered

Adding one: a `DriverKind` variant, a driver (with its Cargo feature) and a dialect,
each with tests, plus live tests in `crates/engine/tests/live.rs`. The compiler points
to the rest. Check the **Safety rules** against it: the gate relies on a read-only
transaction and a server-side time limit where the database has them.

## Model providers

Like databases, only a few places name a concrete provider: `core::llm::ProviderKind`
(the list, label, default address and model, whether it needs a key), its adapter in
`crates/llm/src/<provider>.rs` with its arm in `llm::provider`, and the AI settings
page. API keys live in the Keychain (`ProviderKind::key_account`), never in
`settings.json`. Adding one: a `ProviderKind` variant and an adapter with tests (the
request it builds, a recorded stream, and `tests/http.rs` over HTTP).

## Safety rules (user and AI)

Architectural constraints. Don't weaken, bypass or work around them without the owner's
explicit OK, and keep them covered by tests.

- **One gate.** Every statement to a database passes the engine's gate
  (`crates/engine/src/gate.rs`). Front ends pass only a `Caller` (User, Schedule,
  Board, Editor, Assistant) and an `Approval` (the tab dialog's go-ahead); they never decide
  read-only mode or the time limit themselves. No command, flag or setting skips the gate. The engine reads
  `settings.json` itself.
- **User (tabs).** In write mode the user writes freely; the "UPDATE/DELETE without
  WHERE" dialog stays (`Approval::Confirmed`). Read-only mode of a database user is lifted
  only by changing the settings (the read-only dialog's "Allow writes" does that, and
  the statement then has to be run again); a go-ahead never runs a write past it. Tabs (`Command::Execute`) are the user's only.
- **Schedules.** Only statements on the read allow-list (`assist::guard::not_reads`);
  anything else is refused. Reads run like the assistant's.
- **Boards.** Their tiles run as `Caller::Board`, with the schedules' rules. One refresh
  is one `Command::QueryBatch` (readers only, never `Caller::User`): each statement
  passes the gate on its own, then all run on one spare session in a read-only
  transaction that is rolled back.
- **Boards by AI.** The board tools change tangul's boards, not the database: only
  through `service::boards::{create_by_ai, apply_by_ai}`, on databases offered to AI
  tools, all edits or none and only at the revision the AI read (only changes to
  what a board shows give a new revision). Tile queries must be one statement that
  reads (the gate's checks), are saved under a comment naming the AI, and run as the
  user's board (`Caller::Board`), never for the AI: no result goes back. Before they
  are saved they are planned (`EXPLAIN` as a board read, `Dialect::plan_cost`): a
  cost over `HEAVY_COST` or no plan holds the tile (red, not run by the board) until
  the user ran it in the tile's tab. Links, refresh, removing tiles and deleting or
  archiving boards stay the user's. On a schema-only database the AI sees tile
  queries only as fingerprints.
- **Editor values.** While a condition on a text column is typed, the editor may
  look at its values (`assist::profile::quick_values_sql`: at most a few hundred
  rows) as `Caller::Editor`, with the schedules' rules; only where the database's
  `editor_values` is on (the default). The values are kept apart from the AI's
  value profiles: AI tools never read them.
- **Assistant reads.** Only allow-listed reads run, through
  `EngineHandle::query` (never on a tab's session), in `Mode::RolledBack`: session
  read-only, inside `START TRANSACTION READ ONLY`, always rolled back. One statement
  (no `;`, no `U&` escapes), checked with the database's lexical rules and sent without
  its comments, so the server runs exactly what was checked (same for schedules).
  The in-app assistant (`ai::chat`) calls the same tools through the same `Ctx` as
  MCP clients (caller `assistant`); it has no path of its own to the database.
  `run_readonly` with `show` hands that result to the app, which opens it in a tab on
  the user's connection without running it again; only what the user does there
  afterwards (filter, sort, page, run) runs, as the user.
- **Assistant writes** (anything not on the allow-list) never run for the assistant,
  whatever approval comes along. The gate returns them as a `Proposal`; `propose_sql`
  opens statements in a new tab, unrun, on the user's own connection, where the user
  runs them as themselves (`Caller::User`, with the user's rules). There is no path that
  lets the assistant, the model or an MCP client execute a write.
- **Assistant database user.** A database may name its own user for the assistant
  (recommended: the SELECT-only role the database form sets up). The assistant's reads
  run on its own connection as that user. On MySQL some statements commit implicitly,
  so the role's grants are the real protection there. Without one it uses the tab's
  user (allowed; the title bar says what that means). A chosen user that is gone or
  can't connect is shown to the user, and the AI has no connection: it never connects
  as anyone else instead.
- **What AI tools see of rows.** Per database, in its own settings file
  (`core::ai_view`, `databases/<id>.json` under `ai.content_blocking`; no UI yet):
  `redact` and `hide` lists of patterns by position (`schema.table`,
  `schema.table.column`; an empty part any name: `..email`; no dot is an error;
  `*` within a part, case ignored;
  `!` an exception, the last match in a list wins), `block_queries`. `assist::ai_view::plan` checks every AI read before it
  runs and traces each result column to the table columns it comes from (renames,
  expressions, `*`, CTEs, subqueries, `UNION`, generated columns, views); what it
  can't tell gets the strictest mode the statement uses. While rules apply, only the
  database's own tables (no catalogs, no functions in `FROM`). `Ctx` hands the model
  redacted values as `⟦redacted⟧` and leaves hidden columns out; `show` still opens
  the full result for the user. Value profiles and known values skip these columns.
  AI queries go to history under `-- ai view`; a query whose first line is that
  (case and spaces aside) never runs as the user: the app calls `run_readonly`
  through the registry with a `Ctx` (caller `ai view`)
  and shows what AI tools got, not refinable (`Workspace::run_as_ai`). A settings file that can't be read hides everything.
- **Schema only.** A database's `ai_access` (`"data"` or `"schema"`), under the
  ceiling `ai.access` in `settings.json`, one for every AI tool (the assistant, MCP
  clients, `-- ai view`); the stricter applies. Schema only: no AI connection is opened (the `Exposed` has no
  `conn`, `Ctx` no engine when nothing may read data), value profiles are off,
  bookmarks are shown only as fingerprints, and tools that read rows
  (`Tool::reads_rows`: `run_readonly`, `get_cell`, `column_profile`) aren't offered and
  are refused (audited) by the registry, not by the tools. `propose_sql` then also takes
  queries: they open unrun for the user, and their result never goes back to the AI.
- **Secrets.** Passwords in SQL (`… PASSWORD '…'`, `IDENTIFIED BY '…'`) are replaced by
  `assist::redact` before history, bookmarks, the assistant's chats
  (`ai::chat::for_storage`) and the audit log.
- **Questions in the editor.** Only while the assistant is set up (provider, model,
  key), run text goes to the assistant instead of the database, and only when
  `assist::nl::is_question` is sure (comments don't count); anything else runs as SQL,
  as before.
- **Everyone.** `safety.max_query_time` applies to every caller, set on the server and
  enforced by the engine's timer.
- **Audit.** Assistant and schedule runs (ran, proposed, refused), what boards and
  the editor had refused, and every AI tool call (caller `mcp`) go to the `audit`
  table in `local.db`.
- **Tests.** Changes to the gate, guard, pool or drivers come with tests, including
  live ones (`crates/engine/tests/live.rs`, e.g. `assistant_checks`: a write hidden in
  a read must fail). Aim for ≥ 95% line coverage in core, drivers, engine, assist,
  tunnel, data, service, llm and ai (`.github/workflows/ci.yml` measures it; see
  `docs/TESTING.md`, which names the tests behind each README promise: keep it true).

## Commands

- `make run-dev`: dev databases (Docker) plus the app with dev-only profiles
- `make test`, `make test-live` (against the dev databases); without Docker (a cloud
  session on Debian/Ubuntu): `make test-live-native`, which installs and starts the same
  databases and SSH bastion with apt (`scripts/dev-db-native.sh`)
- UI tests (`crates/app/src/workspace/ui_tests.rs`): the real workspace in a headless
  window, driven by clicks and keys via gpui-kit's harness (`test-support`), with
  `Data::in_memory()` and `tangul_engine::detached()`. Give elements you want to find
  an id and `.test_support()` (a no-op outside tests)
- `make lint` before committing: rustfmt, clippy with warnings as errors, cargo-deny,
  cargo-machete

## Git

- All code goes through `dev`. Branch off `dev` and open pull requests against `dev`,
  never against `main`. Only `dev` → `main` targets `main`, and the version bump that
  `make release` commits on `main` (it merges `main` back into `dev`).
- **One branch and one pull request per feature or feature set**, not per commit.
  Commits make the branch reviewable step by step: one per finished part (an issue,
  a refactor), each passing `make test` and `make lint`, pushed as it is done. Don't
  stack pull requests on each other.
- Issues close when their commit reaches `main`, so put `Closes #N` in the commit
  message. In a pull request into `dev` it has no effect.
- Commit messages in lower case (the subject and the text; names as they are spelled).
- CI (`.github/workflows/ci.yml`) tests on pull requests into `dev` and `main` (their
  merge into the target): every crate's tests once, on Linux, with coverage, and the
  updater on macOS. Merging and releasing test nothing again; a push to `main` only
  builds for the cache. The `dev` → `main` pull request publishes the badges.
  UI tests press `secondary-…` keys (cmd on macOS, ctrl elsewhere), never `cmd-…`.
