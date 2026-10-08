# What the AI sees of your rows

The AI sees the same database you see: every schema, table and column. What you can
limit is what it reads from the rows. Per database, a column can be

- **visible**: as it is,
- **redacted**: the column stays in the result, every value becomes `⟦redacted⟧`
  (`NULL` stays `NULL`),
- **hidden**: the column is left out of the result.

You see everything as always, including results the AI opens for you in a tab.

## Where to set it

Each database has its own settings file, `databases/<id>.json` next to
`settings.json` (on macOS in `~/Library/Application Support/tangul/`). Open it from
the database menu in the title bar: **Database settings file**. It is created with
the defaults the first time. Changes apply to the AI's next query as soon as you
save; there is nothing to restart.

```json
{
  "ai": {
    "type": "blacklist",
    "redact": ["*.email", "*.phone", "orders.adr*", "billing_*"],
    "hide": ["*.ssn", "audit_*"],
    "show": [],
    "block_queries": "none"
  }
}
```

## Patterns

One wildcard, `*`. Case doesn't matter.

| Pattern | Means |
|---|---|
| `users` | the table `users`: all its columns |
| `billing_*` | every table starting with `billing_` |
| `users.email` | the column `email` of `users` |
| `*.email` | the column `email` in every table |
| `orders.adr*` | `adr_street`, `adr_city`, … of `orders` |
| `public.users.email` | with the schema (MySQL: the database) |
| `public.users.*` | the table `users` in the schema `public` |

With a dot it is a column, without one a table. So `public.users` means the column
`users` of a table `public`; write `public.users.*` for the table.

## Blacklist or whitelist

- **`"type": "blacklist"`** (the default): what `redact` and `hide` match is redacted
  or hidden, everything else is visible. `show` is not used.
- **`"type": "whitelist"`**: only what `show` matches is visible, everything else is
  hidden. `redact` still applies to what is shown.

When a column matches more than one list, the strictest wins: hidden, then redacted,
then visible.

```json
{ "ai": { "type": "whitelist", "show": ["orders", "products", "users.id"], "redact": ["orders.adr*"] } }
```

## Renamed and computed columns

The AI writes the SQL, so `SELECT email AS e` or `upper(email)` must not get around a
rule. tangul traces every column of a result back to the table columns it comes from
and gives it the strictest mode among them:

- renames and expressions: `email AS e`, `upper(email)`, `count(email)`,
  `email || ''`, `CASE WHEN email …`
- `*` and `t.*`
- CTEs and subqueries in `FROM`
- `UNION` branches
- generated columns (`domain` computed from `email`)
- views (a view column counts as what its definition reads)

Where it can't tell which column a value comes from, every column of that result gets
the strictest mode the query uses anywhere. A visible column named like a redacted
one counts as redacted too: strict rather than leaky.

While any rule is set, the AI reads only the database's own tables. Catalogs such as
`pg_stats` (which lists the most common values of columns) and functions in `FROM`
(`generate_series(…)`) are refused.

## Filters: `block_queries`

A redacted column can still be used to filter: `WHERE email LIKE 'a%'` returns only
ids, but asked often enough it reveals the value bit by bit. `block_queries` decides
whether that is allowed:

| `block_queries` | Redacted columns | Hidden columns |
|---|---|---|
| `none` (default) | may be used anywhere | may be used anywhere |
| `redacted` | only selected, never filtered, joined, sorted or grouped by | anywhere |
| `redacted_and_hidden` | only selected | only selected (and left out) |

A refused query tells the AI which column and why.

## See what the AI saw

Every query the AI runs goes to your history under two lines:

```sql
-- ai view
-- remove the line above to see the full result
SELECT id, email FROM users
```

Run it from there and the result shows exactly what the AI got: redacted values as
`⟦redacted⟧`, hidden columns left out. Delete the first line to see everything.

You can put `-- ai view` on top of your own queries to check your rules before the AI
ever runs them. It only counts as the first line; case and spaces don't matter
(`-- AI View` works). If the AI would be refused, tangul says why instead of running
the query.

## Good to know

- **Your database grants are still the real boundary.** These rules are applied by
  tangul to what reaches the AI. Give the AI its own read-only database user too (the
  database form sets one up).
- **A settings file tangul can't read hides every column** from the AI until it is
  fixed. The file tab shows what is wrong when you save.
- **Value profiles skip redacted and hidden columns**, and the AI's table descriptions
  mark them, so it doesn't keep trying.
- Not yet: showing part of a value (`Max⟦…⟧ann`,
  [#196](https://github.com/haenning/tangul/issues/196)), blocking user-defined
  functions ([#194](https://github.com/haenning/tangul/issues/194)), a settings screen
  for these rules.
