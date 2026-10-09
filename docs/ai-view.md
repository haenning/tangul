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
      "redact": ["..email", "..phone", ".orders.adr*"],
      "hide": ["..ssn", ".audit_*"],
      "block_queries": []
    }
  }
}
```

## Patterns

Every pattern goes by position: `schema.table` for tables (all their columns),
`schema.table.column` for columns. On MySQL the schema is the database. An empty
part matches any name, `*` any text within a part, and case doesn't matter. A
pattern without a dot is an error (the file then hides everything until it's
fixed).

| Pattern | Means |
|---|---|
| `.users` | the table `users` in every schema |
| `public.users` | the table `users` in the schema `public` |
| `.billing_*` | every table starting with `billing_` |
| `..email` | the column `email` in every table |
| `.orders.adr*` | `adr_street`, `adr_city`, … of `orders` |
| `public.users.email` | one column |
| `sales.` | the whole schema `sales` |
| `orders.adr_*` | the tables starting with `adr_` in the schema `orders` |
| `.` | everything |

**In the file's tab:** a quote in `redact` or `hide` gets its pair, and the parts
still to type show as a hint (`schema.table.column`, then `.table.column` once a
letter is in, …); after a letter, the database's names for that part are offered.
Hovering a pattern says what it means and which columns it matches; hovering
`redact` or `hide` lists every column the file leaves that way. Patterns that
can't be read are underlined in red, ones that match nothing (as tangul last read
the database) or an exception that lifts everything before it in yellow.

**`!` makes an exception, and the last matching pattern of a list wins**, as in
`.gitignore`:

```json
"hide": [".", "!sales.", "sales.payments.card_no"]
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
Columns of the modes listed (`"redacted"`, `"hidden"`) may only be selected, never
used to filter, join, sort or group. Empty (the default): used anywhere.

```json
"block_queries": ["redacted", "hidden"]
```

A refused query tells the AI which column and why.

## See what the AI saw

The AI's queries go to your history as

```sql
-- ai view
-- remove the line above to see the full result
SELECT id, email FROM users
```

Run it and you get exactly the AI's result. Put `-- ai view` as the first line of
your own query to test your rules (case and spaces don't matter). Sorting,
filtering and loading more rows of that result stay as the AI sees it. The
comment must be directly above the statement you run: with a blank line between,
⌘↵ runs the statement without it.

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
