# rusdb – Plan: from nothing to first query

Target: native macOS desktop DB viewer in Rust + egui. MVP adapters: PostgreSQL, MySQL.
Goal of this plan: the shortest path to *"connect → type SQL → Cmd+Enter → see rows"*,
with an architecture that already supports multiple simultaneous connections,
SSH tunnels and a schema sidebar without rewrites later.

---

## 1. Key decisions (the important part)

### D1 — UI thread never touches the network
- egui is immediate-mode: `update()` runs every frame and must never block.
- One **tokio runtime on a background thread**. UI sends `Command`s over an
  unbounded channel, backend sends `Event`s back and calls `ctx.request_repaint()`.
- UI state is plain structs owned by the UI thread. No `Arc<Mutex<_>>` shared with the
  backend → no lock contention, no frame stalls.
- egui runs in reactive mode (repaint only on input/events) → **0% CPU when idle**.

```
┌──────────── UI thread (eframe) ────────────┐        ┌──────── tokio thread ────────┐
│ Sidebar │ Tabs (editor)  │ Results grid    │ ─Cmd─▶ │ ConnectionManager            │
│         │                │                 │ ◀─Evt─ │  ├─ Profile A: SSH? + pool   │
└─────────┴────────────────┴─────────────────┘        │  └─ Profile B: pool          │
                                                      └──────────────────────────────┘
```

### D2 — Drivers: `tokio-postgres` + `mysql_async`, **not** `sqlx`
- sqlx is built for compile-time-typed app queries. A DB viewer runs arbitrary SQL with
  unknown types; sqlx's dynamic decoding is clunky and it hides protocol features we need.
- `tokio-postgres`: `simple_query_raw` (text protocol, **streamed**), built-in `CancelToken`.
- `mysql_async`: text protocol `query_iter` (streamed), built-in pool; cancel via
  `KILL QUERY <id>` on a second connection.
- **Text protocol for the MVP**: every value arrives as a string → zero type-mapping code,
  every exotic type (PostGIS, enums, arrays, JSON) renders on day one. Typed/binary
  decoding (for sorting, editing, export) is a later, additive step.

### D3 — Connection efficiency vs. session semantics (decided)
"Waste no connections" conflicts with SQL session state: `BEGIN`, `SET search_path`,
temp tables, `SET @var` only work if consecutive statements hit the **same** connection.

Recommendation:
- Per connection profile: **one lazily opened metadata connection** (sidebar, autocomplete),
  shared by everything.
- Per query tab: a connection is **leased on execute** from a small pool (default max 3–5)
  and **returned after the statement** → idle tabs cost 0 connections.
- **Tab gets pinned** to its connection when a transaction is open
  (PG: `ReadyForQuery` txn status / detect `BEGIN`; MySQL: `SERVER_STATUS_IN_TRANS` flag)
  or when the user reserves it for the tab.
- **Out of scope for MVP:** a "reserve connection for this tab" button. The lease code gets a
  `// TODO(reserve-connection): tab-level button to pin a connection permanently` marker at
  the point where pinning is decided, so the button only needs to set a flag later.
- Idle pool connections closed after ~60s; nothing opened until first use.
- Consequence: `SET search_path` in a non-sticky tab may not persist → solved by
  a per-tab "default schema/database" that is applied on lease (cheap, one round-trip, only
  when it differs from the connection's current state).

### D4 — SSH tunnel via `russh` (pure Rust, async), one SSH session per profile
- No shelling out to `ssh`, no local port juggling in the MVP UI.
- One SSH session per profile, **multiplexed**: each DB connection = one `direct-tcpip`
  channel over the same SSH session. Local `127.0.0.1:0` listener forwards into channels
  (works with every driver unchanged).
- Auth: password, private key file (+passphrase), **ssh-agent** (`SSH_AUTH_SOCK`, covers
  1Password/Secretive on macOS). `~/.ssh/config` parsing: later.
- Host key: verify against `~/.ssh/known_hosts`, prompt on unknown (never auto-accept).

### D5 — Result storage & rendering built for millions of cells
- Store a result as **one `String` arena + `Vec<u32>` offsets + null bitmap**, not
  `Vec<Vec<Option<String>>>` (millions of small allocations = slow + memory heavy).
- Grid: `egui_extras::TableBuilder` with virtualized rows → only visible rows are laid out.
  Scroll cost is independent of result size.
- Rows are **streamed** in batches (e.g. 1000) → first rows visible while the query runs.
- Hard default limit (e.g. 10k rows, configurable) with "fetch more"; truncate long cells
  in the grid (full value in a detail panel).

### D6 — Secrets in macOS Keychain
- Profiles (host, port, user, db, ssh settings) as TOML in
  `~/Library/Application Support/rusdb/`.
- Passwords / key passphrases via `keyring` crate → macOS Keychain. Never in the TOML.

### D7 — Tabs: `egui_dock`
- Gives draggable/closable tabs and later split views for free. Each tab = editor + its own
  result pane (results "below tabs" for now, as requested).
- Each tab holds a `connection_id` → multiple DBs open in parallel from day one.

---

## 2. Crate layout (cargo workspace)

```
crates/
  core/     # ids, Profile, Command/Event enums, ResultSet (arena), Driver trait, errors
  drivers/  # features: pg, mysql — impl Driver
  ssh/      # russh tunnel
  app/      # eframe UI + backend runtime/ConnectionManager (bin: rusdb)
```
Split mainly for **compile times**: UI edits don't recompile drivers/ssh.

Core trait (sketch):
```rust
#[async_trait]
trait Session: Send {
    async fn execute(&mut self, sql: &str, sink: RowSink) -> Result<ExecSummary>; // streams batches
    fn cancel_handle(&self) -> CancelHandle;
    fn in_transaction(&self) -> bool;
}
#[async_trait]
trait Driver: Send + Sync {
    async fn connect(&self, target: SocketAddr, cfg: &ConnCfg) -> Result<Box<dyn Session>>;
    async fn list_schemas(&self, s: &mut dyn Session) -> Result<Vec<SchemaNode>>;
    async fn list_children(&self, s: &mut dyn Session, node: &NodeId) -> Result<Vec<SchemaNode>>;
}
```

---

## 3. Milestones to first visible query

| # | Milestone | Done when |
|---|-----------|-----------|
| M0 | Workspace + eframe window, layout: left sidebar, center `egui_dock` tabs, bottom result pane (dummy data) | window opens, tabs add/close |
| M1 | `core` types + result arena + virtualized grid with 1M fake rows | smooth scroll with 1M rows |
| M2 | Backend thread: tokio runtime, Command/Event channels, repaint on event | round-trip "ping" visible in UI |
| M3 | PG driver: connect, streamed `simple_query_raw`, cancel | integration test vs docker PG |
| M4 | Connection dialog (host/port/user/password/db), profiles TOML + Keychain, ConnectionManager with lazy pool | connect from UI, status shown |
| **M5** | **Query tab: SQL editor, Cmd+Enter runs (selection or whole buffer), streamed rows in grid, error display, cancel button** | **🎯 first query with visible results** |
| M6 | MySQL driver (same trait) | same UI works against MySQL |
| M7 | Schema sidebar: lazy tree (db → schema → tables/views → columns), uses metadata connection | expand nodes, double-click → `SELECT * … LIMIT 100` in new tab |
| M8 | SSH tunnel (password/key/agent, known_hosts) | connect to DB behind bastion |

M0–M5 is the critical path. M6–M8 are independent and can be done in any order.

---

## 4. Being fast (dev velocity + runtime)

**Dev velocity**
- `docker-compose.yml` with Postgres, MySQL and an `openssh-server` container → every
  feature testable locally, including tunnels.
- `[profile.dev.package."*"] opt-level = 2` → deps optimized (egui/wgpu usable in debug),
  own code stays fast to compile.
- Driver integration tests run headless against docker; UI stays thin.
- Build UI against fake data first (M1), so perf of the grid is proven before networking.

**Runtime**
- eframe with `wgpu` backend (Metal on macOS).
- No work in `update()` besides layout; formatting/parsing happens in the backend.
- Release profile: `lto = "thin"`, `codegen-units = 1`.
- Packaging: `cargo-bundle` → `.app`; signing/notarization later.

---

## 5. Resolved

- User management = **connection profiles** (multiple credential sets per server). No
  role/grant management UI.
- Session model = D3 as proposed; reserve button deferred (TODO marker).
- Look & feel: Zed-like clean UI, native AppKit look is **not** a goal.

## 6. Open: UI framework (SQL editor quality)

egui's `TextEdit` is the weak spot: no multi-cursor, basic undo, syntect highlighting slows
on large buffers. Options:

| Option | Editor | Result grid | Dock/tabs | Risk |
|---|---|---|---|---|
| **egui** (current plan) | `TextEdit` / `egui_code_editor` (same limits); real editor = custom widget (rope + tree-sitter), weeks of work | `egui_extras` virtualized table, very fast | `egui_dock` | Editor stays mediocre unless we build one |
| **GPUI + gpui-component** (Zed's framework) | Built-in code editor, tree-sitter highlighting, LSP hooks, claims stable at 200K lines | Virtualized table (sorting, resizable/fixed columns, cell selection) | Dock with draggable tabs, splits, serializable | Younger ecosystem, API churn, thinner docs; multi-cursor not advertised → verify |
| **iced** | `text_editor` widget + highlighter; no multi-cursor | No built-in table (community crates) | Pane grid, tabs DIY | More DIY than both above |
| **Tauri + Svelte + CodeMirror 6** | Best editor by far (multi-cursor, autocomplete, huge ecosystem), your stack | Must be canvas/virtualized; results cross IPC (serialization cost) | Web libs | Not "native Rust UI"; webview memory/latency; hardest to hit "ultra performant" |

**Recommendation: GPUI + gpui-component.** It is literally the Zed look you want, and it
ships the three hard parts of this app (editor, virtualized table, dock) instead of us
building them. Backend (D1–D6) is unchanged — only the `app` crate differs.

Mitigation for its risk: a **1–2 day spike** before M0: window + dock with 2 tabs + editor
with SQL highlighting + table with 1M rows. If it holds up → switch; if not → egui.

## 7. Remaining limits

- **Text protocol:** no client-side typed sorting/editing yet; binary values (bytea/BLOB)
  show as hex/escaped text.
