//! Postgres' performance boards (see [`crate::perf`]): its statistics views
//! (`pg_stat_user_*`, `pg_statio_*`, `pg_index`, `pg_constraint`), and for the
//! queries `pg_stat_statements`. Index and table statistics count since the last
//! reset or server start; each board says since when.

use crate::perf::{Board, Facts, Kind, Offer, Stats, Tile};

/// One row: the version, and whether `pg_stat_statements` is installed and loaded
/// (`shared_preload_libraries`, when the user may read that setting).
pub(super) const FACTS: &str = "\
SELECT current_setting('server_version_num')::int / 10000 AS major,
       current_setting('server_version_num')::int % 10000 AS minor,
       false AS mariadb,
       CASE
         WHEN NOT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_stat_statements')
           THEN 'no'
         WHEN (SELECT setting FROM pg_settings WHERE name = 'shared_preload_libraries')
              NOT LIKE '%pg_stat_statements%'
           THEN 'not loaded'
         ELSE 'yes'
       END AS statements";

const SINCE: &str = "\
SELECT pg_postmaster_start_time() AS server_started,
       stats_reset AS statistics_reset,
       date_trunc('second', now() - coalesce(stats_reset, pg_postmaster_start_time())) AS counting_for
FROM pg_stat_database
WHERE datname = current_database()";

const SINCE_TEXT: &str =
    "Index and table statistics count since then: \"unused\" means unused since then.";

pub(super) fn boards(facts: &Facts) -> Vec<Offer> {
    vec![
        Offer::ready(indexes()),
        queries(facts),
        Offer::ready(storage(facts)),
    ]
}

fn indexes() -> Board {
    Board {
        kind: Kind::Indexes,
        description: "Indexes that cost without helping, and where one is missing. \
                      Findings, not advice: check before you drop anything."
            .into(),
        tiles: vec![
            Tile::new("Statistics since", SINCE_TEXT, SINCE).half(),
            Tile::new(
                "Index cache hit rate",
                "Index blocks read from memory rather than disk.",
                "\
SELECT round(100.0 * sum(idx_blks_hit) / nullif(sum(idx_blks_hit + idx_blks_read), 0), 2) AS hit_pct,
       sum(idx_blks_read) AS blocks_from_disk
FROM pg_statio_user_indexes",
            )
            .half(),
            Tile::new(
                "Invalid indexes",
                "Left behind by a failed CREATE INDEX CONCURRENTLY: never used, still updated.",
                "\
SELECT n.nspname AS schema, t.relname AS table, i.relname AS index,
       pg_size_pretty(pg_relation_size(i.oid)) AS size,
       format('DROP INDEX CONCURRENTLY %I.%I', n.nspname, i.relname) AS drop_statement
FROM pg_index x
JOIN pg_class i ON i.oid = x.indexrelid
JOIN pg_class t ON t.oid = x.indrelid
JOIN pg_namespace n ON n.oid = t.relnamespace
WHERE NOT x.indisvalid
ORDER BY pg_relation_size(i.oid) DESC
LIMIT 25",
            ),
            Tile::new(
                "Unused indexes, largest first",
                "Never scanned since the statistics began (see above). Unique, primary key and \
                 constraint indexes are left out: they can't simply go. A replica or a rare \
                 job may still use one.",
                "\
SELECT s.schemaname AS schema, s.relname AS table, s.indexrelname AS index,
       pg_size_pretty(pg_relation_size(s.indexrelid)) AS size,
       count(*) OVER () AS unused_in_all,
       format('DROP INDEX CONCURRENTLY %I.%I', s.schemaname, s.indexrelname) AS drop_statement
FROM pg_stat_user_indexes s
JOIN pg_index x ON x.indexrelid = s.indexrelid
WHERE s.idx_scan = 0
  AND NOT x.indisunique AND NOT x.indisprimary AND NOT x.indisexclusion
  AND NOT EXISTS (SELECT 1 FROM pg_constraint k WHERE k.conindid = s.indexrelid)
ORDER BY pg_relation_size(s.indexrelid) DESC
LIMIT 25",
            ),
            Tile::new(
                "Duplicate and redundant indexes",
                "An index with the same columns as another (duplicate), or whose columns \
                 start another one (prefix): the other serves its queries too.",
                "\
SELECT n.nspname AS schema, t.relname AS table, a.relname AS index, b.relname AS covered_by,
       CASE WHEN x.indkey::text = y.indkey::text THEN 'duplicate' ELSE 'prefix' END AS kind,
       pg_size_pretty(pg_relation_size(a.oid)) AS size
FROM pg_index x
JOIN pg_index y ON y.indrelid = x.indrelid AND y.indexrelid <> x.indexrelid
JOIN pg_class a ON a.oid = x.indexrelid
JOIN pg_class b ON b.oid = y.indexrelid
JOIN pg_class t ON t.oid = x.indrelid
JOIN pg_namespace n ON n.oid = t.relnamespace
WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
  AND a.relam = b.relam
  AND x.indpred IS NULL AND y.indpred IS NULL
  AND x.indexprs IS NULL AND y.indexprs IS NULL
  AND NOT x.indisunique AND NOT x.indisprimary
  AND (
    (y.indkey::text = x.indkey::text AND (y.indisunique OR y.indexrelid < x.indexrelid))
    OR y.indkey::text LIKE x.indkey::text || ' %'
  )
ORDER BY pg_relation_size(a.oid) DESC
LIMIT 25",
            ),
            Tile::new(
                "Foreign keys without an index",
                "Deleting or updating a referenced row scans the whole referencing table \
                 (and locks longer) when no index starts with the key's columns.",
                "\
SELECT n.nspname AS schema, t.relname AS table, k.conname AS foreign_key,
       (SELECT string_agg(a.attname, ', ' ORDER BY u.i)
          FROM unnest(k.conkey) WITH ORDINALITY u(att, i)
          JOIN pg_attribute a ON a.attrelid = k.conrelid AND a.attnum = u.att) AS columns,
       f.relname AS references_table,
       pg_size_pretty(pg_relation_size(t.oid)) AS table_size
FROM pg_constraint k
JOIN pg_class t ON t.oid = k.conrelid
JOIN pg_namespace n ON n.oid = t.relnamespace
JOIN pg_class f ON f.oid = k.confrelid
WHERE k.contype = 'f'
  AND n.nspname NOT IN ('pg_catalog', 'information_schema')
  AND NOT EXISTS (
    SELECT 1 FROM pg_index x
    WHERE x.indrelid = k.conrelid
      AND (string_to_array(x.indkey::text, ' ')::int2[])[1:cardinality(k.conkey)] @> k.conkey
      AND (string_to_array(x.indkey::text, ' ')::int2[])[1:cardinality(k.conkey)] <@ k.conkey
  )
ORDER BY pg_relation_size(t.oid) DESC
LIMIT 25",
            ),
            Tile::new(
                "Largest indexes and their use",
                "Big and rarely scanned stands out: size per scan.",
                "\
SELECT schemaname AS schema, relname AS table, indexrelname AS index,
       pg_size_pretty(pg_relation_size(indexrelid)) AS size,
       idx_scan AS scans, idx_tup_read AS rows_read,
       pg_size_pretty(pg_relation_size(indexrelid) / nullif(idx_scan, 0)) AS size_per_scan
FROM pg_stat_user_indexes
ORDER BY pg_relation_size(indexrelid) DESC
LIMIT 25",
            ),
            Tile::new(
                "Tables read by full scans",
                "Tables of more than 10,000 rows read more by sequential scans than by index: \
                 a missing index, or one the planner doesn't take.",
                "\
SELECT schemaname AS schema, relname AS table, n_live_tup AS rows,
       seq_scan AS full_scans, seq_tup_read AS rows_read_by_full_scans,
       coalesce(idx_scan, 0) AS index_scans,
       pg_size_pretty(pg_relation_size(relid)) AS size
FROM pg_stat_user_tables
WHERE n_live_tup > 10000 AND seq_scan > coalesce(idx_scan, 0)
ORDER BY seq_tup_read DESC
LIMIT 25",
            ),
        ],
    }
}

fn queries(facts: &Facts) -> Offer {
    // Renamed in Postgres 13.
    let (total, mean, stddev) = if facts.version.0 >= 13 {
        ("total_exec_time", "mean_exec_time", "stddev_exec_time")
    } else {
        ("total_time", "mean_time", "stddev_time")
    };
    let here = "dbid = (SELECT oid FROM pg_database WHERE datname = current_database())";
    let text = "regexp_replace(query, '\\s+', ' ', 'g') AS query";
    let top = |order: &str, filter: &str, columns: &str| {
        format!(
            "\
SELECT {columns},
       {text}
FROM pg_stat_statements
WHERE {here}{filter}
ORDER BY {order} DESC
LIMIT 25"
        )
    };
    let since = if facts.version.0 >= 14 {
        Tile::new(
            "Statistics since",
            "Statement statistics count since then.",
            "\
SELECT stats_reset AS statistics_reset,
       date_trunc('second', now() - stats_reset) AS counting_for,
       dealloc AS statements_dropped
FROM pg_stat_statements_info",
        )
    } else {
        Tile::new("Statistics since", SINCE_TEXT, SINCE)
    }
    .half();
    let board = Board {
        kind: Kind::Queries,
        description: "The statements that cost this database the most, from \
                      pg_stat_statements. Without the pg_read_all_stats role, other users' \
                      statements show as <insufficient privilege>."
            .into(),
        tiles: vec![
            since,
            Tile::new(
                "In all",
                "Every statement this database ran since the statistics began.",
                &format!(
                    "\
SELECT count(*) AS statements, sum(calls) AS calls,
       round(sum({total})::numeric / 1000) AS total_seconds
FROM pg_stat_statements
WHERE {here}"
                ),
            )
            .half(),
            Tile::new(
                "Most time in all",
                "Where the database spends its time: calls × mean, and its share.",
                &top(
                    total,
                    "",
                    &format!(
                        "round({total}::numeric / 1000, 1) AS total_seconds, calls,
       round({mean}::numeric, 2) AS mean_ms,
       round((100 * {total} / nullif(sum({total}) OVER (), 0))::numeric, 1) AS share_pct"
                    ),
                ),
            ),
            Tile::new(
                "Slowest on average",
                "Statements run at least 10 times, slowest per call first.",
                &top(
                    mean,
                    " AND calls >= 10",
                    &format!(
                        "round({mean}::numeric, 2) AS mean_ms, calls,
       round({total}::numeric / 1000, 1) AS total_seconds"
                    ),
                ),
            ),
            Tile::new(
                "Most called",
                "Often the cheapest to speed up in all: a little per call adds up.",
                &top(
                    "calls",
                    "",
                    &format!(
                        "calls, round({mean}::numeric, 2) AS mean_ms,
       round({total}::numeric / 1000, 1) AS total_seconds"
                    ),
                ),
            ),
            Tile::new(
                "Most read from disk",
                "Blocks not found in memory, and temporary files written (work_mem too \
                 small for a sort or hash).",
                &top(
                    "shared_blks_read + temp_blks_written",
                    "",
                    "shared_blks_read AS blocks_from_disk, temp_blks_written AS temp_blocks_written, calls",
                ),
            ),
            Tile::new(
                "Least predictable",
                "Run at least 10 times and over 1 ms on average, by spread: a plan that \
                 changes, or waiting on locks.",
                &top(
                    stddev,
                    &format!(" AND calls >= 10 AND {mean} >= 1"),
                    &format!(
                        "round({stddev}::numeric, 2) AS stddev_ms, round({mean}::numeric, 2) AS mean_ms,
       calls"
                    ),
                ),
            ),
        ],
    };
    match facts.statements {
        Stats::Yes => Offer::ready(board),
        Stats::Missing => Offer::needs(
            board,
            "Needs pg_stat_statements: CREATE EXTENSION pg_stat_statements, with \
             shared_preload_libraries = 'pg_stat_statements' (a restart)",
        ),
        Stats::NotLoaded => Offer::needs(
            board,
            "pg_stat_statements is installed but not loaded: add it to \
             shared_preload_libraries and restart",
        ),
    }
}

fn storage(facts: &Facts) -> Board {
    let mut tiles = vec![
        Tile::new(
            "Database size",
            "",
            "\
SELECT pg_size_pretty(pg_database_size(current_database())) AS size,
       (SELECT count(*) FROM pg_stat_user_tables) AS tables",
        )
        .half(),
        Tile::new("Statistics since", SINCE_TEXT, SINCE).half(),
        Tile::new(
            "Largest tables",
            "All of a table: its rows, its indexes and TOAST (long values stored apart).",
            "\
SELECT schemaname AS schema, relname AS table,
       pg_size_pretty(pg_total_relation_size(relid)) AS total,
       pg_size_pretty(pg_relation_size(relid)) AS rows_size,
       pg_size_pretty(pg_indexes_size(relid)) AS indexes,
       pg_size_pretty(pg_total_relation_size(relid) - pg_relation_size(relid) - pg_indexes_size(relid)) AS toast,
       n_live_tup AS rows
FROM pg_stat_user_tables
ORDER BY pg_total_relation_size(relid) DESC
LIMIT 25",
        ),
        Tile::new(
            "Dead rows and vacuum",
            "Rows deleted or updated but not yet reclaimed. Many dead rows and an old \
             vacuum: the table grows and scans slow down.",
            "\
SELECT schemaname AS schema, relname AS table, n_dead_tup AS dead_rows, n_live_tup AS rows,
       round(100.0 * n_dead_tup / nullif(n_live_tup + n_dead_tup, 0), 1) AS dead_pct,
       greatest(last_vacuum, last_autovacuum) AS last_vacuum
FROM pg_stat_user_tables
WHERE n_dead_tup > 0
ORDER BY n_dead_tup DESC
LIMIT 25",
        ),
        Tile::new(
            "Stale planner statistics",
            "Rows changed since the last analyze: the planner guesses from old numbers, \
             the most common cause of a bad plan. Never analyzed first.",
            "\
SELECT schemaname AS schema, relname AS table, n_mod_since_analyze AS changed_since_analyze,
       n_live_tup AS rows, greatest(last_analyze, last_autoanalyze) AS last_analyze
FROM pg_stat_user_tables
WHERE n_mod_since_analyze > 0 OR (last_analyze IS NULL AND last_autoanalyze IS NULL)
ORDER BY (last_analyze IS NULL AND last_autoanalyze IS NULL) DESC, n_mod_since_analyze DESC
LIMIT 25",
        ),
        Tile::new(
            "Tables without a primary key",
            "Rows can't be told apart: logical replication and many tools need a key.",
            "\
SELECT n.nspname AS schema, c.relname AS table,
       pg_size_pretty(pg_total_relation_size(c.oid)) AS size
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
WHERE c.relkind IN ('r', 'p') AND NOT c.relispartition
  AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
  AND NOT EXISTS (SELECT 1 FROM pg_constraint k WHERE k.conrelid = c.oid AND k.contype = 'p')
ORDER BY pg_total_relation_size(c.oid) DESC
LIMIT 25",
        ),
    ];
    if facts.version.0 >= 10 {
        tiles.push(Tile::new(
            "Sequences close to their limit",
            "How far a sequence (serial, identity) has come towards its type's maximum: at \
             100% inserts fail.",
            "\
SELECT schemaname AS schema, sequencename AS sequence, data_type, last_value, max_value,
       round(100.0 * last_value / max_value, 2) AS used_pct
FROM pg_sequences
WHERE last_value IS NOT NULL AND increment_by > 0
ORDER BY last_value::numeric / max_value DESC
LIMIT 25",
        ));
    }
    Board {
        kind: Kind::Storage,
        description: "Where the space goes and what keeps the planner and vacuum \
                      from doing their job."
            .into(),
        tiles,
    }
}
