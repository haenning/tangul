//! Telling a question typed into the editor ("which customers ordered twice last
//! month?") from SQL, so running it can go to the assistant instead of the
//! database. Only when it is sure: a wrong guess must never swallow real SQL, so
//! anything unsure is SQL (and runs, or fails, as it always did). Comments don't
//! count: a question under a `-- note` is still a question.

use crate::dialect::Dialect;
use crate::grammar::KEYWORDS;
use crate::lexer::{TokenKind, tokenize_with};

/// Fewest words of a question.
const MIN_WORDS: usize = 4;
/// Words that start real statements in any database; the dialect adds its own.
const STATEMENT_STARTS: &[&str] = &[
    "select",
    "with",
    "insert",
    "update",
    "delete",
    "merge",
    "replace",
    "upsert",
    "values",
    "table",
    "create",
    "alter",
    "drop",
    "truncate",
    "rename",
    "comment",
    "grant",
    "revoke",
    "begin",
    "start",
    "commit",
    "rollback",
    "savepoint",
    "release",
    "end",
    "abort",
    "set",
    "reset",
    "use",
    "call",
    "exec",
    "execute",
    "do",
    "prepare",
    "deallocate",
    "declare",
    "fetch",
    "close",
    "move",
    "copy",
    "load",
    "vacuum",
    "analyze",
    "analyse",
    "reindex",
    "cluster",
    "refresh",
    "lock",
    "unlock",
    "listen",
    "notify",
    "unlisten",
    "discard",
    "pragma",
    "attach",
    "detach",
    "handler",
    "optimize",
    "repair",
    "flush",
    "kill",
    "import",
    "checkpoint",
];
/// Statement starts that are ordinary words too ("show me the orders", "explain
/// this error"): they don't decide it, the rest of the text does.
const WORDS_TOO: &[&str] = &["show", "explain", "describe", "desc", "check", "list"];
/// Words a question starts with, in the languages people are likely to type.
/// They are never taken as a misspelled keyword ("which" is two edits from "with").
const QUESTION_WORDS: &[&str] = &[
    // English
    "which", "what", "who", "whom", "whose", "where", "when", "why", "how", "is", "are", "was",
    "were", "do", "does", "did", "can", "could", "would", "should", "will", "has", "have", "give",
    "find", "get", "tell", "count", "please", "i", "we", "list", "show", "explain", "help",
    "write", "compare", // German
    "welche", "welcher", "welches", "welchen", "was", "wer", "wen", "wem", "wessen", "wo", "wann",
    "warum", "wieso", "weshalb", "wie", "wieviel", "wieviele", "gibt", "zeige", "zeig", "finde",
    "liste", "gib", "hat", "haben", "ist", "sind", "kannst", "bitte", "erkläre", "ich", "wir",
    // French
    "quel", "quelle", "quels", "quelles", "qui", "que", "quoi", "où", "quand", "pourquoi",
    "combien", "comment", "montre", "donne", "trouve", "est-ce",
    // Spanish, Portuguese, Italian
    "cuál", "cuáles", "qué", "quién", "quiénes", "dónde", "cuándo", "cuántos", "cuántas", "cómo",
    "por", "muestra", "dame", "qual", "quais", "quem", "onde", "quando", "quanto", "quantos",
    "quantas", "como", "mostra", "quali", "chi", "dove", "perché", "quanti",
];

/// Whether `text` is surely a question in natural language, not SQL. `is_name`
/// tells whether a word (lower case) names a table or column of the connection:
/// such words make it less sure.
pub fn is_question(text: &str, dialect: &dyn Dialect, is_name: impl Fn(&str) -> bool) -> bool {
    let text = without_comments(text, dialect);
    let text = text.trim();
    if has_sql_structure(text) {
        return false;
    }
    let words: Vec<String> = text
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '\'' || c == '-'))
        .map(|w| w.trim_matches(|c| c == '\'' || c == '-').to_lowercase())
        .filter(|w| !w.is_empty())
        .collect();
    if words.len() < MIN_WORDS {
        return false;
    }
    let first = words[0].as_str();
    // "what's" → "what".
    let question_word = QUESTION_WORDS.contains(&first.split('\'').next().unwrap_or(first));
    if !WORDS_TOO.contains(&first)
        && (is_statement_start(first, dialect)
            || (!question_word && (KEYWORDS.contains(&first) || near_keyword(first, dialect))))
    {
        return false;
    }
    let known = |w: &str| {
        KEYWORDS.contains(&w) || is_statement_start(w, dialect) || is_name(w) || is_number(w)
    };
    let unknown = words.iter().filter(|w| !known(w)).count();
    let sentence = question_word || text.ends_with('?') || words.len() >= 6;
    // Mostly words SQL doesn't know.
    sentence && unknown * 2 >= words.len()
}

/// `text` with its comments (as the database reads them) replaced by a space.
/// A comment the lexer can't see (after an apostrophe it takes for a string)
/// stays, and makes the text SQL.
fn without_comments(text: &str, dialect: &dyn Dialect) -> String {
    tokenize_with(text, dialect.lexing())
        .iter()
        .map(|t| match t.kind {
            TokenKind::Comment => " ",
            _ => t.text(text),
        })
        .collect()
}

/// Characters and shapes of SQL that a question hardly has: statement ends,
/// comparisons, `*`, comments, quoted names and strings, `schema.table` and calls
/// like `count(`.
fn has_sql_structure(text: &str) -> bool {
    if ["--", "/*", "::", "||"].iter().any(|s| text.contains(s))
        || text.chars().any(|c| {
            matches!(
                c,
                ';' | '=' | '<' | '>' | '*' | '`' | '"' | '(' | ')' | '$' | '@'
            )
        })
    {
        return true;
    }
    let chars: Vec<char> = text.chars().collect();
    chars.iter().enumerate().any(|(i, &c)| {
        let before = i.checked_sub(1).map(|j| chars[j]);
        let after = chars.get(i + 1).copied();
        match c {
            // Only an apostrophe inside a word ("what's", "don't") is prose.
            '\'' => {
                !(before.is_some_and(char::is_alphabetic) && after.is_some_and(char::is_alphabetic))
            }
            // `app.orders`, `o.id`: a dot between names (not "e.g." or a sentence end).
            '.' => {
                let name = |c: &&char| c.is_alphanumeric() || **c == '_';
                let before = chars[..i].iter().rev().take_while(name).count();
                let after_len = chars[i + 1..].iter().take_while(name).count();
                let abbreviation =
                    before == 1 && after_len == 1 && chars.get(i + 2).is_none_or(|c| *c == '.');
                before > 0
                    && !chars[i - before..i].iter().all(char::is_ascii_digit)
                    && after.is_some_and(|c| c.is_alphabetic() || c == '_')
                    && !abbreviation
            }
            _ => false,
        }
    })
}

fn is_statement_start(word: &str, dialect: &dyn Dialect) -> bool {
    STATEMENT_STARTS.contains(&word)
        || dialect
            .statement_keywords()
            .iter()
            .filter_map(|k| k.split_whitespace().next())
            .any(|k| k.eq_ignore_ascii_case(word) && !WORDS_TOO.contains(&word))
}

/// A statement keyword with a typo (`SELCT`, `UDPATE`): at most two edits from one.
fn near_keyword(word: &str, dialect: &dyn Dialect) -> bool {
    let long = |k: &&str| k.len() >= 4;
    let dialect_starts = dialect
        .statement_keywords()
        .iter()
        .filter_map(|k| k.split_whitespace().next());
    STATEMENT_STARTS
        .iter()
        .copied()
        .chain(dialect_starts)
        .filter(long)
        .any(|k| edit_distance(word, &k.to_lowercase()) <= 2)
}

fn is_number(word: &str) -> bool {
    word.chars().all(|c| c.is_ascii_digit())
}

/// Levenshtein distance, by characters.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            let next = (row[j] + 1).min(row[j + 1] + 1).min(prev + cost);
            prev = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use tangul_core::DriverKind;

    use super::*;
    use crate::dialect::{all, dialect_for};

    const TABLES: &[&str] = &[
        "customers",
        "orders",
        "users",
        "status",
        "id",
        "email",
        "total",
    ];

    fn question(text: &str) -> bool {
        all().all(|d| is_question(text, d, |w| TABLES.contains(&w)))
            && is_question(text, dialect_for(DriverKind::Postgres), |_| false)
    }

    fn sql_everywhere(text: &str) -> bool {
        all().all(|d| !is_question(text, d, |w| TABLES.contains(&w)))
    }

    /// Real SQL, half-typed statements, typos and bits that must always run as SQL.
    const SQL: &[&str] = &[
        "SELECT * FROM orders",
        "select id, email from users where active",
        "SELECT count(id) FROM orders GROUP BY status",
        "WITH recent AS (SELECT * FROM orders) SELECT * FROM recent",
        "with open orders as select",
        "INSERT INTO users (email) VALUES ('a@b.c')",
        "UPDATE orders SET total = 0 WHERE id = 1",
        "update orders set status to paid for everyone",
        "DELETE FROM orders WHERE status = 'cancelled'",
        "delete all cancelled orders from last year",
        "EXPLAIN SELECT * FROM orders",
        "EXPLAIN ANALYZE SELECT id FROM orders o JOIN users u ON u.id = o.user_id",
        "explain analyze verbose select",
        "SHOW TABLES",
        "SHOW CREATE TABLE users",
        "show full processlist",
        "DESCRIBE users",
        "desc orders",
        "TABLE users",
        "VALUES (1), (2)",
        "(SELECT 1) UNION (SELECT 2)",
        "-- which customers ordered twice?",
        "/* which customers ordered twice */",
        "-- which customers ordered twice?\nselect * from orders",
        "/* which customers ordered twice? */ select id from orders",
        "which customers ordered twice /* note */ ; select 1",
        "SELCT * FROM x",
        "SELEC id FROM users WHERE id IN (1, 2)",
        "UDPATE orders SET x = 1",
        "slect email from users order by email",
        "selet all users from the users table",
        "creat table orders from other tables now",
        "BEGIN",
        "begin transaction isolation level serializable",
        "commit",
        "set search_path to app, public",
        "SET statement_timeout TO 1000",
        "grant select on all tables in schema app to reporting",
        "VACUUM ANALYZE orders",
        "CREATE INDEX ON orders (user_id)",
        "ALTER TABLE users ADD COLUMN name text",
        "DROP TABLE orders",
        "TRUNCATE orders",
        "copy orders to stdout with csv header",
        "call refresh_totals()",
        "PRAGMA table_info(users)",
        "select",
        "sel",
        "",
        "   ",
        "orders",
        "users where id = 1",
        "from orders select id",
        "o.id, o.total from orders o",
        "app.orders join app.users",
        "status from orders group by status order by status desc",
        "id email total status",
        "\"Orders\" order by id",
        "`orders` limit 10",
        "limit 10 offset 20 for the next page",
        "where status in ('open', 'paid')",
        "which customers ordered = twice",
        "which customers ordered twice; drop table users",
    ];

    /// Questions in several languages.
    const QUESTIONS: &[&str] = &[
        "which customers ordered twice last month?",
        "Which customers ordered twice last month",
        "what's the average order value per customer?",
        "how many users signed up this week",
        "show me the customers who haven't ordered yet",
        "explain why the totals don't add up",
        "list the biggest customers by revenue this year",
        "why is this query so slow?",
        "can you find duplicate email addresses?",
        "who bought the most in March",
        "give me the top ten products by revenue",
        "welche Kunden haben letzten Monat zweimal bestellt?",
        "wie viele Bestellungen gab es gestern",
        "zeige mir alle Kunden ohne Bestellung",
        "quels clients ont commandé deux fois le mois dernier ?",
        "combien de commandes par jour en moyenne",
        "¿cuántos pedidos hubo ayer por cliente?",
        "quali clienti hanno ordinato due volte",
        "the customers that ordered twice last month, e.g. for a newsletter",
        "revenue per country for the last quarter please",
        // Comments don't count.
        "-- asked by Ann\nwhich customers ordered twice last month?",
        "/* for the newsletter */ which customers ordered twice last month?",
        "which customers ordered twice last month? -- not the cancelled ones",
    ];

    #[test]
    fn real_sql_is_never_a_question() {
        for sql in SQL {
            assert!(sql_everywhere(sql), "taken as a question: {sql}");
        }
    }

    #[test]
    fn questions_are_recognized() {
        for q in QUESTIONS {
            assert!(question(q), "not taken as a question: {q}");
        }
    }

    #[test]
    fn words_of_the_schema_make_it_less_sure() {
        let d = dialect_for(DriverKind::Postgres);
        let text = "customers orders users status total";
        assert!(!is_question(text, d, |w| TABLES.contains(&w)));
        // Sentence-like but mostly names and keywords.
        assert!(!is_question(
            "orders by status and total for users",
            d,
            |w| TABLES.contains(&w)
        ));
        assert!(is_question("which orders were paid late?", d, |_| false));
    }

    #[test]
    fn edit_distances() {
        assert_eq!(edit_distance("select", "select"), 0);
        assert_eq!(edit_distance("selct", "select"), 1);
        assert_eq!(edit_distance("which", "with"), 2);
        assert_eq!(edit_distance("", "abc"), 3);
    }
}
