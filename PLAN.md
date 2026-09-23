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

### D4 — SSH tunnel via `russh` (not implemented yet)
- One SSH session per profile, each DB connection = one `direct-tcpip` channel; local
  `127.0.0.1:0` listener, so drivers stay unchanged. Auth: password, key, ssh-agent.
  Verify `known_hosts`, never auto-accept. Hooks: `TODO(ssh)` in profile/form.

### D5 — Result storage
- One `String` arena + `u32` end offsets + null flags per result (no allocation per cell).
- `gpui-component` `DataTable` is virtualized: only visible cells are rendered, read
  straight from the arena. Long values are cut at 200 chars in the grid.

### D6 — Profiles & secrets
- Profiles: TOML in `~/Library/Application Support/rusdb/profiles.toml`.
- Passwords: macOS Keychain (`keyring`, service `rusdb`). On Linux (dev only) an
  in-memory mock store is used → passwords are not persisted there.
- "User management" = multiple profiles per server (different users). No role/grant UI.

### D7 — UI: GPUI + gpui-component, kept deliberately plain
- Editor: gpui-component code editor (tree-sitter SQL highlighting, line numbers).
  Its engine has multi-cursor support (`AddCursorAbove/Below`, bound to ⌘⌥↑/↓ on macOS);
  not yet verified on a Mac. ⌘↵ runs the selection or the whole buffer.
- Simple own layout (sidebar | tab bar + tab content | status bar), stock components and
  the default dark theme. No custom styling system.
- Pinned versions (`gpui-kit = "=0.6.6"`, toolchain 1.98) — GPUI API churn is the main
  risk, upgrades are deliberate.
- Zed's own editor is GPL-3.0: don't copy code from it into this Apache-2.0 project.

---

## 2. Crate layout

```
crates/
  core/     # ids, Profile, ResultSet arena, TableInfo — no I/O
  drivers/  # Driver/Session traits, postgres.rs, mysql.rs
  engine/   # tokio thread, Command/Event, pool (D3); live integration tests
  app/      # GPUI binary `rusdb`: workspace, query tab, results grid, connection form
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
| M8 | SSH tunnel | ⏳ next |

### Next candidates
1. SSH tunnel (M8).
2. Resizable editor/results split; results per statement (currently last result set).
3. Cell detail view + copy (cell / row / TSV).
4. Columns/indexes in the sidebar (lazy per table).
5. Fetch more rows via server-side cursors instead of discarding beyond the limit.
6. `.app` bundle (`cargo-bundle`), signing/notarization.

---

## 4. Dev workflow

- `docker compose up -d` → Postgres 16 + MySQL 8.4 for local testing.
- `cargo test -p rusdb-engine` with `RUSDB_TEST_PG` / `RUSDB_TEST_MYSQL` set runs the live
  tests (queries, NULLs, errors, pinning, release, cancel, row limit) against real servers.
- Deps are optimized in dev builds (`[profile.dev.package."*"] opt-level = 2`).
