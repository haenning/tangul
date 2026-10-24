# Settings files

tangul keeps its settings as JSON next to each other in the config folder (macOS:
`~/Library/Application Support/tangul/`, or `TANGUL_CONFIG_DIR`):

| File | Opened with |
|---|---|
| `settings.json` | `⌘,` |
| `databases/<id>.json`, one per database | database menu → **Database settings file** |

The trash icon next to a database in the database menu (asked first) deletes its file
with everything else tangul keeps for it: its users and their passwords, boards,
bookmarks and their runs, history, the assistant's chats about it, pinned tabs.
The audit log stays.

Saved changes apply right away. A file that doesn't parse is kept as you wrote it
but not applied; the tab says why. While you type, the tab underlines each value
that can't be applied in red, with what it accepts on hover (`"dim"`: expected
one of `auto`, `dark`, `light`), and keys it doesn't know in yellow.

**Upgrades.** Each file starts with `"version"`: the tangul version whose layout
it has; leave it as it is.
On start, tangul brings every file from an older release to its current layout:
keys that moved are carried over, missing keys added at their default, your values
and keys it doesn't know are kept. The old text stays next to it as `<name>.bak`.
A file written by a newer tangul is left alone. In a file's tab, **Upgrade** does
the same and **Revert to default** puts every key at its default; both only fill
the editor, so you look before you save.

## `settings.json`

```json
{
  "version": "0.1.2",
  "editor": { "font_size": null, "font_family": null },
  "table": { "font_size": null, "font_family": null },
  "chart": { "primary_color": null, "font_size": null, "font_family": null },
  "safety": {
    "read_only_users": [],
    "confirm_unfiltered_writes": true,
    "relock_after": null,
    "max_query_time": "1m"
  },
  "monitoring": {
    "interval": "1d",
    "schedule_new_bookmarks": false,
    "max_points": 10000,
    "max_age": "365d",
    "max_mb": 50
  },
  "theme": { "appearance": "auto", "dark": "Tangul Dark Rainbow", "light": "Tangul Light Rainbow" },
  "mcp": { "enabled": false, "port": 7433 },
  "ai": {
    "provider": null,
    "providers": {
      "anthropic": { "model": null, "base_url": null },
      "openai": { "model": null, "base_url": null },
      "gemini": { "model": null, "base_url": null },
      "ollama": { "model": null, "base_url": null }
    },
    "max_tool_rounds": 12,
    "questions_in_editor": true,
    "access": "data"
  },
  "export": { "line_endings": "lf", "bom": false },
  "updates": { "check": true }
}
```

Durations are a whole number and a unit: `"30s"`, `"5m"`, `"1h"`, `"7d"`. `null`
means unset (the app's own value) or no limit.

| Key | Meaning |
|---|---|
| `editor`, `table`, `chart`: `font_size`, `font_family` | Fonts of the SQL editor, the result grid and chart labels (sizes are kept within 8–32, 8–18 and 6–16) |
| `chart.primary_color` | First series' color, e.g. `"#2563eb"`; else the theme's |
| `safety.read_only_users` | Database users in read-only mode |
| `safety.confirm_unfiltered_writes` | Ask before `UPDATE`/`DELETE` without `WHERE` |
| `safety.relock_after` | Read-only mode back on this long after the last write |
| `safety.max_query_time` | Every statement stops after this long, for every caller |
| `monitoring.interval` | How often scheduled bookmarks run (5m to 1d) |
| `monitoring.schedule_new_bookmarks` | New bookmarks start scheduled (if they only read) |
| `monitoring.max_points`, `max_age`, `max_mb` | How many runs are kept per bookmark, for how long, how much space in all |
| `theme.appearance` | `"auto"` (as the system), `"dark"` or `"light"` |
| `theme.dark`, `theme.light` | Theme names for each appearance |
| `mcp.enabled`, `mcp.port` | The MCP server on `127.0.0.1:<port>` |
| `ai.access` | The most AI tools (the assistant, MCP clients, `-- ai view`) see of any database: `"data"` or `"schema"` |
| `ai.provider` | `"anthropic"`, `"openai"` (any OpenAI-compatible service), `"gemini"` or `"ollama"`; `null` until chosen |
| `ai.providers` | Per provider: `model`, `base_url` (`null`: its own). Keys are in the Keychain, never here |
| `ai.max_tool_rounds` | Most tool rounds per question (1–50) |
| `ai.questions_in_editor` | A question run in the editor goes to the assistant |
| `export.line_endings` | How lines end in files saved from a result: `"lf"` or `"crlf"` (Windows); values keep their own line breaks |
| `export.bom` | CSV and TSV files start with a byte order mark, so Excel reads them as UTF-8 |
| `updates.check` | Look for a newer release at start and once a day (one request to GitHub, nothing else sent); nothing installs without a click |

## `themes/<name>.json`

Color themes (the built-in ones can be overridden by a file with the same name). A theme
needs only what it changes: every color that is missing or isn't one (`"#rrggbb"` or
`"#rrggbbaa"`), an empty `charts` and an unknown key are replaced by the rainbow theme
of its `mode` (Tangul Dark Rainbow or Tangul Light Rainbow); a file without a `mode`
takes the mode it is chosen for, `theme.dark` or `theme.light`. Its tab underlines each
replacement in yellow; only a file that isn't a JSON object doesn't load.

The optional `grid` section sets how the result grid's mark looks; a key left out is
derived from `selection` and `accent`.

```json
"grid": {
  "row": "#e5484d14",
  "mark": "#e5484d40",
  "mark_border": "#e5484d",
  "mark_border_width": 2,
  "mark_radius": 4,
  "header_mark": "#e5484d33"
}
```

| Key | |
| --- | --- |
| `row` | Highlight of the rows the mark covers (default: `selection` at 35% of its alpha) |
| `mark` | Fill of marked cells (default: `selection`) |
| `mark_border` | Outline around the mark's outer edge (default: `accent`) |
| `mark_border_width` | Outline width in pixels, 0 to 8; 0 draws none (default: 2) |
| `mark_radius` | Radius of the mark's outer corners in pixels, 0 to 8; 0 is square (default: 4) |
| `header_mark` | Fill of a marked column's header cell (default: `selection`) |

## `databases/<id>.json`

```json
{
  "version": "0.1.2",
  "ai": {
    "content_blocking": {
      "redact": [],
      "hide": [],
      "block_queries": []
    }
  }
}
```

What AI tools see of this database's rows; see [What the AI sees of your
rows](ai-view.md). Keys under `ai` it doesn't know, or a newer `"version"`, make
the file fail, and then AI tools see no rows, so a typo never leaves data open.
