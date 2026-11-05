# tangul roadmap

Planned work lives as GitHub issues (label `roadmap`) in a GitHub Project. This file is
the source for setting that project up and the reference for its structure.
Design decisions: [PLAN.md](PLAN.md).

## Scope

tangul is a fast, focused database **viewer**: explore, query, understand.

- **In scope:** querying, navigating schemas and relations, understanding data (charts,
  graphs, profiles), safe access (read-only), AI help that works from local context.
- **Out of scope:** editing data in the grid, table designers, schema diff and migrations.
  Data changes are always SQL the user reviews and runs; schema changes belong in
  migration tools (golang-migrate etc.).

## Project structure

- **Project:** `tangul roadmap` (owner `haenning`), linked to `haenning/rusdb`.
- **Items:** one issue per feature, labels `enhancement`, `roadmap` and one `area:*`.
- **Fields**
  - `Status` (built-in): Todo · In Progress · Done
  - `Horizon` (single select): Quick win · Short term · Northstar · Backlog
- **Views** (create in the UI; the API can't create views)
  - *Roadmap* — table, filter `label:roadmap`, group by `Horizon`, sort by issue number
  - *Board* — board, columns by `Status`
- **Labels:** `area:editor`, `area:results`, `area:schema`, `area:graph`, `area:ai`,
  `area:connections`, `area:observability`, `area:drivers`, `area:distribution`, `roadmap`
- **New ideas:** the feature request template (`.github/ISSUE_TEMPLATE`); triage by adding
  `roadmap`, an area label, the project and a Horizon.

## Items

Issues #15–#42 exist already.

| # | Horizon | Area | Title | Depends on |
|---|---|---|---|---|
| 15 | Quick win | editor | Underline SQL errors at the position the server reports | |
| 16 | Quick win | results | Right-click → UPDATE / DELETE statement for selected rows or a value | |
| 17 | Quick win | connections | Read-only mode per database / user | |
| 18 | Quick win | schema | Columns with primary/foreign key markers in the schema tree | |
| 19 | Quick win | editor | Run the statement under the cursor when nothing is selected | |
| 20 | Quick win | results | Copy results as CSV, JSON, INSERT statements or Markdown | |
| 21 | Quick win | editor | Confirm UPDATE / DELETE without WHERE | |
| 22 | Quick win | results | Show count / sum / avg / min / max of selected cells | |
| 23 | Short term | results | Show a result as a chart | |
| 24 | Short term | graph | Database graph (2D, Obsidian-style) | #26 |
| 25 | Short term | ai | AI context engine: compact schema digest with row estimates | |
| 26 | Short term | ai | AI context engine: join graph from foreign keys and learned joins | |
| 27 | Short term | ai | AI context engine: relevant schema slice for a question | #25, #26 |
| 28 | Short term | ai | AI context engine: example queries from history and bookmarks | |
| 29 | Short term | ai | AI context engine: value profiles (opt-in) | |
| 30 | Short term | ai | MCP tool server with read-only guardrails | #17, #25, #26 |
| 31 | Short term | editor | Live warnings for unknown tables and columns | |
| 32 | Short term | editor | Background validation via EXPLAIN (Postgres) | |
| 33 | Short term | results | Filter and sort bar on results, paging beyond the row limit | |
| 34 | Short term | results | Value viewer for JSON and long text | |
| 35 | Short term | editor | Format SQL | |
| 36 | Short term | editor | EXPLAIN plan visualization | |
| 37 | Northstar | ai | ACP agent panel | #30 |
| 38 | Northstar | graph | 3D database graph | #24 |
| 39 | Northstar | drivers | More databases: SQL Server, ClickHouse (SQLite files: read-only) | |
| 40 | Northstar | distribution | Signed and notarized releases | |
| 41 | Backlog | observability | Log exploration | |
| 42 | Backlog | observability | Observability dashboard | |

Note: #31 and #32 are largely covered by live validation (statements are prepared on the
server while typing, which reports unknown tables/columns and syntax errors without
executing). Keep them open for client-side hints (suggestions on hover) and EXPLAIN-based
checks, or close them once live validation is merged.

## Creating the project (for a local agent or by hand)

Needs the `project` scope once: `gh auth refresh -s project`.

```sh
OWNER=haenning
# 1. Project, linked to the repo
gh project create --owner $OWNER --title "tangul roadmap" --format json   # note "number" and "id"
gh project link <number> --owner $OWNER --repo haenning/rusdb

# 2. Horizon field
gh project field-create <number> --owner $OWNER --name Horizon \
  --data-type SINGLE_SELECT --single-select-options "Quick win,Short term,Northstar,Backlog"
gh project field-list <number> --owner $OWNER --format json               # Horizon field id + option ids

# 3. Add each issue and set its Horizon (table above)
item=$(gh project item-add <number> --owner $OWNER \
  --url https://github.com/haenning/rusdb/issues/15 --format json --jq .id)
gh project item-edit --id "$item" --project-id <project id> \
  --field-id <Horizon field id> --single-select-option-id <option id for "Quick win">
# … repeat for #16–#42
```

Then create the two views in the UI (see Project structure).
