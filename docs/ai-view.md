# What the AI sees of your rows

The AI sees the same schemas, tables and columns as you. Per database you decide
what it reads from the rows: a column is **visible**, **redacted** (every value
becomes `⟦redacted⟧`, `NULL` stays `NULL`) or **hidden** (left out). You always see
everything, including results the AI opens for you.

## Where

Database menu in the title bar → **Database settings file**. It opens
`databases/<id>.json` next to `settings.json` (created on first open). Changes
apply to the AI's next query once saved.

```json
{
  "ai": {
    "content_blocking": {
      "redact": ["*.email", "*.phone", "orders.adr*"],
      "hide": ["*.ssn", "audit_*"],
      "block_queries": "none"
    }
  }
}
```

## Patterns

`*` is the only wildcard; case doesn't matter. With a dot a column, without one a
table.

| Pattern | Means |
|---|---|
| `users` | the table `users` |
| `billing_*` | every table starting with `billing_` |
| `*.email` | the column `email` in every table |
| `orders.adr*` | `adr_street`, `adr_city`, … of `orders` |
| `public.users.email` | with the schema (MySQL: the database) |
| `public.users.*` | the table `users` in the schema `public` |
| `sales.*.*` | the whole schema `sales` |

**`!` makes an exception, and the last matching pattern of a list wins**, as in
`.gitignore`:

```json
"hide": ["*", "!sales.*.*", "sales.payments.card_no"]
```

hides everything except the schema `sales`, and in it `payments.card_no`. Careful:
a typo in a `!` pattern opens data, a typo elsewhere only fails to hide.

Across lists the stricter wins: hidden over redacted.

## Renamed and computed columns

A result column counts as every table column it comes from: renames
(`email AS e`), expressions (`upper(email)`, `count(email)`), `*`, CTEs,
subqueries, `UNION`, generated columns and views. Where that can't be told, the
whole result gets the strictest mode the query uses. While any rule is set, the AI
reads only the database's own tables: no catalogs (`pg_stats` lists common
values), no functions in `FROM`.

## `block_queries`

Filtering on a redacted column (`WHERE email LIKE 'a%'`) reveals it bit by bit.

| Value | Redacted and hidden columns may be |
|---|---|
| `none` (default) | used anywhere |
| `redacted` | redacted ones only selected; hidden ones anywhere |
| `redacted_and_hidden` | only selected |

A refused query tells the AI which column and why.

## See what the AI saw

The AI's queries go to your history as

```sql
-- ai view
-- remove the line above to see the full result
SELECT id, email FROM users
```

Run it and you get exactly the AI's result. Put `-- ai view` as the first line of
your own query to test your rules (case and spaces don't matter).

## Good to know

- **Database grants remain the real boundary.** Give the AI its own read-only user
  too (the database form sets one up).
- A settings file that can't be read hides everything until fixed; the file tab
  says what's wrong.
- Value profiles skip redacted and hidden columns; the AI's table descriptions mark
  them.
- Not yet: partial masking ([#196](https://github.com/haenning/tangul/issues/196)),
  blocking user-defined functions
  ([#194](https://github.com/haenning/tangul/issues/194)), a settings screen.
