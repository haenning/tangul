# rusdb – Plan

Native macOS database viewer in Rust + GPUI (Zed's UI framework) via `gpui-kit`
(`gpui-component`). MVP adapters: PostgreSQL, MySQL. Look: plain, Zed-like, dark.

---

## 1. Key decisions

### D1 — UI thread never touches the network
- One **tokio runtime on its own thread** (`rusdb-engine`). UI sends `Command`s over an
  unbounded channel; the engine sends `Event`s back over an `async_channel`, which the UI
  awaits on GPUI's executor.
- The UI drains all queued events per wake-up → a stream of row batches causes one
  re-render, not one per batch.
- No shared locks between UI and engine.

```
┌──────────── GPUI main thread ──────────────┐        ┌──────── tokio thread ────────┐
│ Sidebar │ Tabs: editor + results grid      │ ─Cmd─▶ │ Engine                       │
│         │                                  │ ◀─Evt─ │  ├─ Pool (profile A)         │
└─────────┴──────────────────────────────────┘        │  └─ Pool (profile B)         │
                                                      └──────────────────────────────┘
```

### D2 — Drivers: `tokio-postgres` + `mysql_async`, text protocol, not `sqlx`
- sqlx targets compile-time-typed app queries; a viewer runs arbitrary SQL.
- Text protocol: every value is a string → any server type renders without mapping code.
- Rows are streamed in batches of 1000; per result set max `ROW_LIMIT` (10k) rows are kept.
- Cancel: Postgres `CancelToken`; MySQL `KILL QUERY` over a short-lived second connection.
- TLS via native-tls (macOS Security framework). `prefer`/`require` encrypt without
  certificate verification (libpq semantics). Verified modes: TODO.

### D3 — Connection efficiency vs. session semantics (implemented)
- Per profile one lazy pool (max 4). Nothing opens before first use.
- A tab **leases a connection only while a statement runs** → idle tabs cost nothing.
- Schema browsing uses the same pool (no dedicated metadata connection) → a single user
  with one tab uses exactly **one** server connection.
- If a statement leaves a **transaction open, the session is pinned to its tab** until
  COMMIT/ROLLBACK or the tab closes (closing = connection closed = server rollback).
  - Postgres: tokio-postgres doesn't expose the protocol's transaction status. After
    statements containing transaction keywords (or while pinned) the driver probes
    `SELECT now() <> statement_timestamp()`; an error means "aborted txn" → still pinned.
  - MySQL: `SERVER_STATUS_IN_TRANS` flag from the last OK packet (free).
- Idle connections close after 60 s.
- `TODO(reserve-connection)`: a tab button to pin a connection permanently (out of scope).
- The status bar shows `open/max · idle · in transaction` for the active profile.

### D4 — SSH tunnel via `russh` (`crates/tunnel`)
- One SSH session per database pool, each DB connection = one `direct-tcpip` channel; local
  `127.0.0.1:0` listener, so drivers stay unchanged. Auth: ssh-agent, key file (optional
  passphrase), password; secrets in the Keychain (`ssh:<database id>`).
- Opened lazily by the pool; a dead session is re-opened on the next connection.
- Host keys are checked against `~/.ssh/known_hosts`, never accepted silently: an unknown
  key asks the user (fingerprint dialog, "Trust & connect" appends it); a changed key is
  an error. `RUSDB_KNOWN_HOSTS` overrides the file (tests).
- ring crypto backend (no aws-lc/cmake build dependency).

### D5 — Result storage
- One `String` arena + `u32` end offsets + null flags per result (no allocation per cell).
- `gpui-component` `DataTable` is virtualized: only visible cells are rendered, read
  straight from the arena. Long values are cut at 200 chars in the grid.

### D6 — Profiles & secrets
- Profiles: TOML in `~/Library/Application Support/rusdb/profiles.toml`.
- Passwords: macOS Keychain (`keyring`, service `rusdb`). On Linux (dev only) an
  in-memory mock store is used → passwords are not persisted there.
- "User management" = multiple users per saved database, switched from the title bar. No role/grant UI.

### D6b — Local data & SQL assistance
- `local.db` (SQLite via `rusqlite`, bundled) next to `profiles.toml`: bookmarks, query
  history (full SQL + fingerprint with literals replaced by `?`), learned usage and a
  per-user schema cache (JSON). Schema migrations via `PRAGMA user_version`.
- `crates/assist` is UI-free and unit-tested: forgiving lexer, clause-keyword based
  statement analysis (tables/aliases/columns/joins), completion, usage "frecency"
  (count decayed by age) and per-table query suggestions.
- Completion: tables after FROM/JOIN, columns in scope (by alias), FK- and history-based
  join conditions, keywords; ranked by match quality + usage.
- AI assistance later, via ACP (Agent Client Protocol); placeholder buttons in the
  bottom bar.
- Schema graph (planned, placeholder in the bottom bar): an Obsidian-style graph of
  tables and their foreign keys from `SchemaInfo`, with the same structure exported in a
  compact form for AI agents to build queries from.

### D7 — UI: GPUI + gpui-component, kept deliberately plain
- Editor: gpui-component code editor (tree-sitter SQL highlighting, line numbers).
  Its engine has multi-cursor support (`AddCursorAbove/Below`, bound to ⌘⌥↑/↓ on macOS);
  not yet verified on a Mac. ⌘↵ runs the selection or the whole buffer.
- Simple own layout (sidebar | tab bar + tab content | status bar), stock components and
  the default dark theme. No custom styling system.
- Pinned versions (`gpui-kit = "=0.6.6"`, Rust 1.98.1 = Zed's toolchain) — GPUI API
  churn is the main risk, upgrades are deliberate.

### D8 — Open source, Apache-2.0
- Not a product. License Apache-2.0 (same as GPUI and gpui-component). No dependency
  forces a license: all are permissive; the only copyleft ones are MPL-2.0 (file-level,
  fine to use).
- Zed's own editor/app crates are GPL-3.0: don't copy code from them.
- Distribution: universal DMG via tag-based GitHub releases (ad-hoc signed); Developer ID signing, notarization and a Homebrew cask later.

---

## 2. Crate layout

```
crates/
  core/     # ids, Database/DbUser, ResultSet arena, SchemaInfo — no I/O
  drivers/  # Driver/Session traits, postgres.rs, mysql.rs
  engine/   # tokio thread, Command/Event, pool (D3); live integration tests
  assist/   # SQL lexer, analysis, completion, usage ranking, suggestions — no UI/I/O
  tunnel/   # SSH tunnels (russh): lazy session, local listener, known_hosts checks
  app/      # GPUI binary `rusdb`: workspace, query tab, results grid, forms, local.db
```

---

## 3. Milestones

| # | Milestone | Status |
|---|-----------|--------|
| M0 | Workspace, GPUI window, layout | ✅ |
| M1 | Core types, result arena, virtualized grid | ✅ |
| M2 | Engine thread, Command/Event bridge | ✅ |
| M3 | Postgres driver (stream, cancel, errors like psql) | ✅ |
| M4 | Connection dialog, profiles TOML + Keychain, lazy pool | ✅ |
| **M5** | **Query tab: editor, ⌘↵, streamed results, errors, cancel** | ✅ **first query** |
| M6 | MySQL driver | ✅ |
| M7 | Schema sidebar (schema → tables/views, click opens `SELECT * … LIMIT 100`) | ✅ basic (no columns yet) |
| M8 | SSH tunnel; auto-connect to the last used connection at startup | ✅ |
| M9 | Bookmarks, history, completion, learned suggestions (local.db) | ✅ |

### Next candidates
2. Resizable editor/results split; results per statement (currently last result set).
3. Cell detail view (copy cell/rows as TSV is done).
4. Columns/indexes in the sidebar (columns are loaded already, see D6b).
5. Fetch more rows via server-side cursors instead of discarding beyond the limit.
6. Developer ID signing + notarization (removes the first-start Gatekeeper step);
   Homebrew cask. `.app`/DMG bundling and tag-based GitHub releases exist already.

---

## 4. Dev workflow

- `make run-dev`: dev Postgres (:55432) + MySQL (:53306) via docker compose with sample
  data (`dev/seed/`), app started with separate passwordless dev profiles.
- `make test-live`: engine integration tests (queries, NULLs, errors, pinning, release,
  cancel, row limit, binary data) against the dev databases.
- CI: Linux job = fmt, clippy, tests incl. live DB tests (service containers);
  macOS job = clippy for the whole workspace + app build.
- Deps are optimized in dev builds (`[profile.dev.package."*"] opt-level = 2`).
