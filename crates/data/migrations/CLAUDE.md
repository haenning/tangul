# Migrations of the settings files

Each settings file (`settings.json`, `databases/<id>.json`) carries `"version"`: the
tangul version whose layout it has. One migrator (`src/migrations.rs`) runs the
steps here on older files, at start and on "Upgrade"; then missing keys get their
default. Steps are data, not code.

- **Versions follow releases.** A step is `<file>/<version>.json`, named after the
  release whose layout it leads to. While a version is developed, steps are
  `<version>-dev.N.json` (`0.1.3-dev.1`, `0.1.3-dev.2`, …), so dev and test setups
  are migrated too. `make release` joins them into one `<version>.json` and refuses
  a dev step of another version.
- **Only when keys move or change meaning.** A new key with a default needs no step.
  Treat released key names like an API: rename only when it is worth a step.
- **A step is a list of operations** on dotted paths; each does nothing where its key
  is missing, and must be harmless if run twice (a tester's file runs a dev step
  again when the release's joined step comes):
  - `{ "move": "ai.redact", "to": "ai.content_blocking.redact" }`: kept at `to` if
    that is set already
  - `{ "remove": "ai.show" }`
  - `{ "map": "ai.block_queries", "values": { "none": [], "redacted": ["redacted"] } }`:
    a text value that is a key of `values` becomes its value
  - `{ "merge": "mcp.access", "into": "ai.access", "strictest": ["data", "schema"] }`:
    `merge` goes away, `into` keeps the stricter of both (the later in `strictest`)
  Anything else needs a new operation in the migrator, with tests, not code per step.
- **Never change or drop a released step.** Users skip releases.
- **Fixtures**: `make release` keeps the layout each release writes, when it
  changed, as `fixtures/<file>/<version>.json`; a test upgrades every fixture and
  checks it loads. Don't edit them.
- Rules that protect data (`ai.content_blocking`) fail closed: a step that can't
  carry a rule over must leave the file failing (hiding everything), never drop it.
