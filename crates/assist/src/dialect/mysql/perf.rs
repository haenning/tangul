//! MySQL's and MariaDB's performance boards (see [`crate::perf`]):
//! `information_schema` for sizes and structure, `performance_schema` for how indexes
//! and statements are used (on by default in MySQL, often off in MariaDB; without it
//! the boards keep what works without). Statistics count since the server started.

use crate::perf::{Board, Facts, Kind, Offer, Stats, Tile};

/// One row: the version, MariaDB or not, and whether `performance_schema` is on.
pub(super) const FACTS: &str = "\
SELECT CAST(SUBSTRING_INDEX(VERSION(), '.', 1) AS UNSIGNED) AS major,
       CAST(SUBSTRING_INDEX(SUBSTRING_INDEX(VERSION(), '.', 2), '.', -1) AS UNSIGNED) AS minor,
       VERSION() LIKE '%MariaDB%' AS mariadb,
       IF(@@performance_schema, 'yes', 'no') AS statements";

const PERFORMANCE_SCHEMA: &str =
    "Needs performance_schema = ON in the server's configuration (a restart)";

fn mb(bytes: &str) -> String {
    format!("CONCAT(ROUND({bytes} / 1048576, 1), ' MB')")
}

pub(super) fn boards(facts: &Facts) -> Vec<Offer> {
    let on = facts.statements == Stats::Yes;
    vec![
        Offer::ready(indexes(facts, on)),
        if on {
            Offer::ready(queries(facts))
        } else {
            Offer::needs(queries(facts), PERFORMANCE_SCHEMA)
        },
        Offer::ready(storage(facts, on)),
    ]
}

/// Since when the server counts: its uptime, from `performance_schema` (MariaDB
/// also has it in `information_schema`).
fn since(facts: &Facts, on: bool) -> Option<Tile> {
    let table = match (on, facts.mariadb) {
        (true, _) => "performance_schema.global_status",
        (false, true) => "information_schema.GLOBAL_STATUS",
        (false, false) => return None,
    };
    Some(
        Tile::new(
            "Statistics since",
            "Usage statistics count since the server started: \"unused\" means unused since then.",
            &format!(
                "\
SELECT NOW() - INTERVAL VARIABLE_VALUE SECOND AS server_started,
       CONCAT(VARIABLE_VALUE DIV 86400, ' d ', SEC_TO_TIME(VARIABLE_VALUE % 86400)) AS counting_for
FROM {table}
WHERE VARIABLE_NAME = 'Uptime'"
            ),
        )
        .half(),
    )
}

fn indexes(facts: &Facts, on: bool) -> Board {
    let mut tiles: Vec<Tile> = since(facts, on).into_iter().collect();
    if on {
        tiles.push(
            Tile::new(
                "Buffer pool hit rate",
                "InnoDB pages read from memory rather than disk.",
                "\
SELECT ROUND(100 - 100 * r.VARIABLE_VALUE / NULLIF(q.VARIABLE_VALUE, 0), 2) AS hit_pct,
       r.VARIABLE_VALUE AS pages_from_disk
FROM performance_schema.global_status q
JOIN performance_schema.global_status r
  ON q.VARIABLE_NAME = 'Innodb_buffer_pool_read_requests'
 AND r.VARIABLE_NAME = 'Innodb_buffer_pool_reads'",
            )
            .half(),
        );
        tiles.push(Tile::new(
            "Unused indexes, on the largest tables first",
            "Never used since the server started (see above). Unique, primary key and \
             foreign key indexes are left out: they can't simply go. A replica or a rare \
             job may still use one.",
            "\
SELECT u.OBJECT_NAME AS `table`, u.INDEX_NAME AS `index`, t.TABLE_ROWS AS table_rows,
       COUNT(*) OVER () AS unused_in_all,
       CONCAT('ALTER TABLE `', u.OBJECT_NAME, '` DROP INDEX `', u.INDEX_NAME, '`') AS drop_statement
FROM performance_schema.table_io_waits_summary_by_index_usage u
JOIN information_schema.TABLES t
  ON t.TABLE_SCHEMA = u.OBJECT_SCHEMA AND t.TABLE_NAME = u.OBJECT_NAME
WHERE u.OBJECT_SCHEMA = DATABASE()
  AND u.INDEX_NAME IS NOT NULL AND u.INDEX_NAME <> 'PRIMARY'
  AND u.COUNT_STAR = 0
  AND NOT EXISTS (
    SELECT 1 FROM information_schema.STATISTICS s
    WHERE s.TABLE_SCHEMA = u.OBJECT_SCHEMA AND s.TABLE_NAME = u.OBJECT_NAME
      AND s.INDEX_NAME = u.INDEX_NAME AND s.NON_UNIQUE = 0)
  AND NOT EXISTS (
    SELECT 1 FROM information_schema.TABLE_CONSTRAINTS k
    WHERE k.TABLE_SCHEMA = u.OBJECT_SCHEMA AND k.TABLE_NAME = u.OBJECT_NAME
      AND k.CONSTRAINT_NAME = u.INDEX_NAME AND k.CONSTRAINT_TYPE = 'FOREIGN KEY')
ORDER BY t.TABLE_ROWS DESC
LIMIT 25",
        ));
    }
    tiles.push(Tile::new(
        "Duplicate and redundant indexes",
        "An index with the same columns as another (duplicate), or whose columns start \
         another one (prefix): the other serves its queries too.",
        "\
WITH idx AS (
  SELECT TABLE_NAME AS tbl, INDEX_NAME AS name, MAX(NON_UNIQUE) AS non_unique,
         MAX(INDEX_TYPE) AS type,
         GROUP_CONCAT(CONCAT(COLUMN_NAME, IFNULL(CONCAT('(', SUB_PART, ')'), ''))
                      ORDER BY SEQ_IN_INDEX SEPARATOR ',') AS cols
  FROM information_schema.STATISTICS
  WHERE TABLE_SCHEMA = DATABASE()
  GROUP BY TABLE_NAME, INDEX_NAME
  HAVING SUM(COLUMN_NAME IS NULL) = 0
)
SELECT a.tbl AS `table`, a.name AS `index`, b.name AS covered_by,
       IF(a.cols = b.cols, 'duplicate', 'prefix') AS kind, a.cols AS columns
FROM idx a
JOIN idx b ON b.tbl = a.tbl AND b.name <> a.name AND b.type = a.type
WHERE a.non_unique = 1
  AND ((a.cols = b.cols AND (b.non_unique = 0 OR b.name < a.name))
       OR b.cols LIKE CONCAT(a.cols, ',%'))
ORDER BY a.tbl, a.name
LIMIT 25",
    ));
    tiles.push(Tile::new(
        "Index space by table",
        "All indexes of a table together, next to its rows.",
        &format!(
            "\
SELECT TABLE_NAME AS `table`, {} AS indexes, {} AS `rows_size`, TABLE_ROWS AS table_rows
FROM information_schema.TABLES
WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE'
ORDER BY INDEX_LENGTH DESC
LIMIT 25",
            mb("INDEX_LENGTH"),
            mb("DATA_LENGTH")
        ),
    ));
    if on {
        tiles.push(Tile::new(
            "Tables read without an index",
            "Tables of more than 10,000 rows by rows read without an index: a missing \
             index, or one the optimizer doesn't take.",
            "\
SELECT u.OBJECT_NAME AS `table`, t.TABLE_ROWS AS table_rows,
       u.COUNT_READ AS rows_read_without_index
FROM performance_schema.table_io_waits_summary_by_index_usage u
JOIN information_schema.TABLES t
  ON t.TABLE_SCHEMA = u.OBJECT_SCHEMA AND t.TABLE_NAME = u.OBJECT_NAME
WHERE u.OBJECT_SCHEMA = DATABASE() AND u.INDEX_NAME IS NULL
  AND u.COUNT_READ > 0 AND t.TABLE_ROWS > 10000
ORDER BY u.COUNT_READ DESC
LIMIT 25",
        ));
    }
    let description = if on {
        "Indexes that cost without helping, and where one is missing. Findings, not \
         advice: check before you drop anything."
    } else {
        "Indexes that cost without helping. How indexes are used needs \
         performance_schema = ON (a restart); without it only their structure and size."
    };
    Board {
        kind: Kind::Indexes,
        description: description.into(),
        tiles,
    }
}

fn queries(facts: &Facts) -> Board {
    let here = "SCHEMA_NAME = DATABASE()";
    let text = "DIGEST_TEXT AS query";
    let top = |order: &str, filter: &str, columns: &str| {
        format!(
            "\
SELECT {columns},
       {text}
FROM performance_schema.events_statements_summary_by_digest
WHERE {here}{filter}
ORDER BY {order} DESC
LIMIT 25"
        )
    };
    let mut tiles: Vec<Tile> = since(facts, true).into_iter().collect();
    tiles.extend([
        Tile::new(
            "In all",
            "Every statement this database ran since the server started.",
            &format!(
                "\
SELECT COUNT(*) AS statements, SUM(COUNT_STAR) AS calls,
       ROUND(SUM(SUM_TIMER_WAIT) / 1e12) AS total_seconds
FROM performance_schema.events_statements_summary_by_digest
WHERE {here}"
            ),
        )
        .half(),
        Tile::new(
            "Most time in all",
            "Where the database spends its time: calls × mean, and its share.",
            &top(
                "SUM_TIMER_WAIT",
                "",
                "ROUND(SUM_TIMER_WAIT / 1e12, 1) AS total_seconds, COUNT_STAR AS calls,
       ROUND(AVG_TIMER_WAIT / 1e9, 2) AS mean_ms,
       ROUND(100 * SUM_TIMER_WAIT / NULLIF(SUM(SUM_TIMER_WAIT) OVER (), 0), 1) AS share_pct",
            ),
        ),
        Tile::new(
            "Slowest on average",
            "Statements run at least 10 times, slowest per call first.",
            &top(
                "AVG_TIMER_WAIT",
                " AND COUNT_STAR >= 10",
                "ROUND(AVG_TIMER_WAIT / 1e9, 2) AS mean_ms, COUNT_STAR AS calls,
       ROUND(SUM_TIMER_WAIT / 1e12, 1) AS total_seconds",
            ),
        ),
        Tile::new(
            "Most called",
            "Often the cheapest to speed up in all: a little per call adds up.",
            &top(
                "COUNT_STAR",
                "",
                "COUNT_STAR AS calls, ROUND(AVG_TIMER_WAIT / 1e9, 2) AS mean_ms,
       ROUND(SUM_TIMER_WAIT / 1e12, 1) AS total_seconds",
            ),
        ),
        Tile::new(
            "Most rows examined",
            "Rows read to find the rows sent, and temporary tables written to disk.",
            &top(
                "SUM_ROWS_EXAMINED",
                "",
                "SUM_ROWS_EXAMINED AS rows_examined, SUM_ROWS_SENT AS rows_sent,
       SUM_CREATED_TMP_DISK_TABLES AS temp_tables_on_disk, COUNT_STAR AS calls",
            ),
        ),
        Tile::new(
            "Run without an index",
            "Statements that read a table without any index, most often first.",
            &top(
                "SUM_NO_INDEX_USED",
                " AND SUM_NO_INDEX_USED > 0",
                "SUM_NO_INDEX_USED AS times_without_index, COUNT_STAR AS calls,
       ROUND(AVG_TIMER_WAIT / 1e9, 2) AS mean_ms",
            ),
        ),
    ]);
    Board {
        kind: Kind::Queries,
        description: "The statements that cost this database the most, from \
                      performance_schema's statement digests."
            .into(),
        tiles,
    }
}

fn storage(facts: &Facts, on: bool) -> Board {
    let mut tiles = vec![
        Tile::new(
            "Database size",
            "",
            &format!(
                "\
SELECT {} AS size, COUNT(*) AS tables
FROM information_schema.TABLES
WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE'",
                mb("SUM(DATA_LENGTH + INDEX_LENGTH)")
            ),
        )
        .half(),
    ];
    tiles.extend(since(facts, on));
    tiles.extend([
        Tile::new(
            "Largest tables",
            "Rows and indexes of each table (the row count is InnoDB's estimate).",
            &format!(
                "\
SELECT TABLE_NAME AS `table`, {} AS total, {} AS `rows_size`, {} AS indexes,
       TABLE_ROWS AS table_rows, ENGINE AS engine
FROM information_schema.TABLES
WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE'
ORDER BY DATA_LENGTH + INDEX_LENGTH DESC
LIMIT 25",
                mb("DATA_LENGTH + INDEX_LENGTH"),
                mb("DATA_LENGTH"),
                mb("INDEX_LENGTH")
            ),
        ),
        Tile::new(
            "Free space kept by tables",
            "Space freed by deletes that the table keeps for itself: OPTIMIZE TABLE gives \
             it back (it rebuilds the table).",
            &format!(
                "\
SELECT TABLE_NAME AS `table`, {} AS free, {} AS total
FROM information_schema.TABLES
WHERE TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE' AND DATA_FREE > 0
ORDER BY DATA_FREE DESC
LIMIT 25",
                mb("DATA_FREE"),
                mb("DATA_LENGTH + INDEX_LENGTH")
            ),
        ),
        Tile::new(
            "Tables without a primary key",
            "Rows can't be told apart: replication and many tools need a key, and InnoDB \
             adds a hidden one.",
            &format!(
                "\
SELECT t.TABLE_NAME AS `table`, {} AS size, t.TABLE_ROWS AS table_rows
FROM information_schema.TABLES t
WHERE t.TABLE_SCHEMA = DATABASE() AND t.TABLE_TYPE = 'BASE TABLE'
  AND NOT EXISTS (
    SELECT 1 FROM information_schema.TABLE_CONSTRAINTS k
    WHERE k.TABLE_SCHEMA = t.TABLE_SCHEMA AND k.TABLE_NAME = t.TABLE_NAME
      AND k.CONSTRAINT_TYPE = 'PRIMARY KEY')
ORDER BY t.DATA_LENGTH + t.INDEX_LENGTH DESC
LIMIT 25",
                mb("t.DATA_LENGTH + t.INDEX_LENGTH")
            ),
        ),
        Tile::new(
            "Auto-increment close to its limit",
            "How far each auto-increment column has come towards its type's maximum: at \
             100% inserts fail. (MySQL may show the next value up to a day late.)",
            "\
SELECT t.TABLE_NAME AS `table`, c.COLUMN_NAME AS `column`, c.COLUMN_TYPE AS type,
       t.AUTO_INCREMENT AS next_value,
       ROUND(100 * t.AUTO_INCREMENT / (
         CASE c.DATA_TYPE
           WHEN 'tinyint' THEN 255 WHEN 'smallint' THEN 65535
           WHEN 'mediumint' THEN 16777215 WHEN 'int' THEN 4294967295
           ELSE 18446744073709551615
         END / IF(c.COLUMN_TYPE LIKE '%unsigned%', 1, 2)), 4) AS used_pct
FROM information_schema.TABLES t
JOIN information_schema.COLUMNS c
  ON c.TABLE_SCHEMA = t.TABLE_SCHEMA AND c.TABLE_NAME = t.TABLE_NAME
 AND c.EXTRA LIKE '%auto_increment%'
WHERE t.TABLE_SCHEMA = DATABASE() AND t.AUTO_INCREMENT IS NOT NULL
ORDER BY used_pct DESC
LIMIT 25",
        ),
    ]);
    Board {
        kind: Kind::Storage,
        description: "Where the space goes, and keys running out.".into(),
        tiles,
    }
}
