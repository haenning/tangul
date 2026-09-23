//! Editor completion: asks the grammar what the cursor position expects (a table, an
//! operand, an operator or clause keyword, …) and ranks the fitting candidates by
//! match quality and usage.

use std::collections::HashSet;
use std::ops::Range;

use rusdb_core::{ForeignKey, TableKind};

use crate::analyze::{TableRef, analyze, significant, statements};
use crate::grammar::{self, Clause, Expect, State};
use crate::lexer::{Token, TokenKind, tokenize, unquote};
use crate::schema::SchemaIndex;
use crate::usage::{Usage, UsageKind, column_key, parse_join_key, table_key};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionKind {
    Keyword,
    Function,
    Table,
    View,
    Column,
    Join,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub label: String,
    pub insert: String,
    pub kind: SuggestionKind,
    pub detail: String,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Completion {
    /// Byte range of `sql` the chosen item replaces (the partially typed word).
    pub replace: Range<usize>,
    pub items: Vec<Suggestion>,
}

const MAX_ITEMS: usize = 80;

/// Where the cursor is, beyond what the grammar state says.
enum Target {
    /// Columns of a table in scope (`alias.|`).
    ColumnsOf(usize),
    /// Tables of a schema (`schema.|`).
    TablesIn(String),
    State(State),
    Nothing,
}

pub fn complete(sql: &str, offset: usize, schema: &SchemaIndex, usage: &Usage) -> Completion {
    let offset = offset.min(sql.len());
    let Some((start, end)) = statements(sql)
        .into_iter()
        .find(|&(s, e)| offset >= s && offset <= e)
    else {
        return Completion::default();
    };
    let stmt = &sql[start..end];
    let off = offset - start;

    // Inside a string or comment: nothing to complete.
    if tokenize(stmt).iter().any(|t| {
        matches!(t.kind, TokenKind::String | TokenKind::Comment) && t.start < off && off < t.end
    }) {
        return Completion::default();
    }

    let tokens = significant(stmt);
    let current = tokens
        .iter()
        .position(|t| t.is_ident() && t.start < off && off <= t.end);
    let (prefix, replace_start) = match current {
        Some(i) => (unquote(&stmt[tokens[i].start..off]), tokens[i].start),
        None => (String::new(), off),
    };
    let before: Vec<Token> = tokens
        .iter()
        .copied()
        .take_while(|t| t.end <= replace_start)
        .collect();

    let mut scope = analyze(stmt).tables;
    // The reference being typed (`FROM us|`, `FROM app.|`) is not in scope yet.
    let dot = before
        .last()
        .filter(|t| t.is_punct(stmt, '.'))
        .map(|t| t.start);
    scope.retain(|t| {
        let typing = current.is_some() && t.start <= replace_start && replace_start <= t.end;
        !typing && dot != Some(t.end)
    });
    let resolved: Vec<Option<usize>> = scope
        .iter()
        .map(|t| schema.resolve(t.schema.as_deref(), &t.name))
        .collect();

    let target = target(stmt, &before, &scope, schema);
    let cx = Ctx {
        schema,
        usage,
        prefix: &prefix,
        scope: &scope,
        resolved: &resolved,
        lower_keywords: lower_keywords(stmt, &tokens, &prefix),
    };
    let d = schema.driver();
    let mut items = match target {
        Target::Nothing => Vec::new(),
        Target::ColumnsOf(i) => cx.columns(Some(i)),
        Target::TablesIn(s) => cx.tables(Some(&s)),
        Target::State(st) => match st.expect {
            Expect::Statement => cx.keywords(grammar::statement_keywords(d)),
            Expect::Words(words) => cx.keywords(words),
            Expect::Name => Vec::new(),
            Expect::Table if st.clause == Clause::Join => {
                let mut items = cx.joins(None);
                items.extend(cx.tables(None));
                items
            }
            Expect::Table => cx.tables(None),
            // Right after a table the next word is often an alias: only offer
            // keywords once two letters make the intent clear.
            Expect::AfterTable { alias: false } if prefix.chars().count() < 2 => Vec::new(),
            Expect::AfterTable { alias } => {
                cx.keywords(&grammar::after_table_keywords(d, st.clause, alias))
            }
            Expect::AfterOperand { star } => {
                cx.keywords(&grammar::after_operand_keywords(d, st.clause, star))
            }
            Expect::Operand { first } => {
                let mut items = Vec::new();
                // Right after `JOIN t ON`: the join conditions first.
                if st.clause == Clause::On
                    && before.last().is_some_and(|t| t.is_word(stmt, "on"))
                    && let Some(j) = scope.iter().rposition(|t| t.end <= replace_start)
                    && resolved[j].is_some()
                {
                    items.extend(cx.joins(Some(j)));
                }
                if grammar::operand_takes_columns(st.clause) {
                    items.extend(cx.columns(None));
                    items.extend(cx.functions(grammar::functions(d)));
                }
                items.extend(cx.keywords(grammar::operand_keywords(st.clause, first)));
                items
            }
        },
    };
    items.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.label.len().cmp(&b.label.len()))
            .then(a.label.cmp(&b.label))
    });
    let mut seen = HashSet::new();
    items.retain(|i| seen.insert(i.insert.clone()));
    items.truncate(MAX_ITEMS);
    Completion {
        replace: start + replace_start..offset,
        items,
    }
}

fn target(sql: &str, before: &[Token], scope: &[TableRef], schema: &SchemaIndex) -> Target {
    // `qualifier.` → columns of that table, or tables of that schema.
    if before.last().is_some_and(|t| t.is_punct(sql, '.')) {
        let Some(q) = before
            .len()
            .checked_sub(2)
            .map(|i| before[i])
            .filter(|t| t.is_ident())
        else {
            return Target::Nothing;
        };
        let q = unquote(q.text(sql));
        if let Some(i) = scope.iter().position(|t| t.answers_to(&q)) {
            return Target::ColumnsOf(i);
        }
        if schema.has_schema(&q) {
            return Target::TablesIn(q);
        }
        return Target::Nothing;
    }
    Target::State(grammar::state_at(sql, before))
}

/// Keywords follow the user's casing: the typed prefix, else the statement's first
/// keyword; uppercase by default.
fn lower_keywords(sql: &str, tokens: &[Token], prefix: &str) -> bool {
    if prefix.chars().any(char::is_alphabetic) {
        return !prefix.chars().any(char::is_uppercase);
    }
    tokens
        .iter()
        .find(|t| t.kind == TokenKind::Word)
        .is_some_and(|t| !t.text(sql).chars().any(char::is_uppercase))
}

struct Ctx<'a> {
    schema: &'a SchemaIndex,
    usage: &'a Usage,
    prefix: &'a str,
    scope: &'a [TableRef],
    resolved: &'a [Option<usize>],
    lower_keywords: bool,
}

/// How well `candidate` matches what was typed; `None` hides it.
fn match_score(candidate: &str, prefix: &str) -> Option<f64> {
    if prefix.is_empty() {
        return Some(1.0);
    }
    let (c, p) = (candidate.to_lowercase(), prefix.to_lowercase());
    if c == p {
        Some(3.5)
    } else if c.starts_with(&p) {
        Some(3.0)
    } else if p.len() >= 2 && c.split('_').any(|part| part.starts_with(&p)) {
        Some(2.0)
    } else if p.len() >= 2 && c.contains(&p) {
        Some(1.2)
    } else {
        None
    }
}

fn usage_boost(score: f64) -> f64 {
    2.0 * score.ln_1p()
}

impl Ctx<'_> {
    /// `list` is in order of relevance; that order survives ranking.
    fn keywords(&self, list: &[&str]) -> Vec<Suggestion> {
        list.iter()
            .enumerate()
            .filter_map(|(i, kw)| {
                let score = match_score(kw, self.prefix)?;
                let insert = if self.lower_keywords {
                    kw.to_lowercase()
                } else {
                    kw.to_string()
                };
                Some(Suggestion {
                    label: insert.clone(),
                    insert,
                    kind: SuggestionKind::Keyword,
                    detail: "keyword".into(),
                    // Below schema objects when mixed with them.
                    score: score - 0.5 - i as f64 * 0.01,
                })
            })
            .collect()
    }

    /// Functions, inserted with the opening parenthesis.
    fn functions(&self, list: &[&str]) -> Vec<Suggestion> {
        list.iter()
            .enumerate()
            .filter_map(|(i, f)| {
                // Only once typing started: they would drown the columns otherwise.
                if self.prefix.is_empty() {
                    return None;
                }
                let score = match_score(f, self.prefix)?;
                let name = if self.lower_keywords {
                    f.to_string()
                } else {
                    f.to_uppercase()
                };
                Some(Suggestion {
                    label: format!("{name}()"),
                    insert: format!("{name}("),
                    kind: SuggestionKind::Function,
                    detail: "function".into(),
                    score: score - 0.3 - i as f64 * 0.001,
                })
            })
            .collect()
    }

    fn tables(&self, in_schema: Option<&str>) -> Vec<Suggestion> {
        let s = self.schema;
        s.tables()
            .iter()
            .enumerate()
            .filter(|(_, t)| in_schema.is_none_or(|q| t.schema.eq_ignore_ascii_case(q)))
            .filter_map(|(ix, t)| {
                let insert = match in_schema {
                    Some(_) => s.quote(&t.name),
                    None => s.sql_name(ix),
                };
                let score = match_score(&t.name, self.prefix)
                    .or_else(|| match_score(&insert, self.prefix))?;
                let usage = self
                    .usage
                    .score(UsageKind::Table, &table_key(&t.schema, &t.name));
                let (kind, noun) = match t.kind {
                    TableKind::Table => (SuggestionKind::Table, "table"),
                    TableKind::View => (SuggestionKind::View, "view"),
                };
                Some(Suggestion {
                    label: insert.clone(),
                    insert,
                    kind,
                    detail: format!("{noun} · {}", t.schema),
                    score: score + usage_boost(usage),
                })
            })
            .collect()
    }

    /// Columns of one table in scope, or of all tables in scope (qualified when there
    /// are several), or — with nothing in scope yet — of every table.
    fn columns(&self, qualifier: Option<usize>) -> Vec<Suggestion> {
        let s = self.schema;
        let tables: Vec<(Option<&TableRef>, usize)> = match qualifier {
            Some(i) => self.resolved[i]
                .map(|ix| (Some(&self.scope[i]), ix))
                .into_iter()
                .collect(),
            None if self.scope.is_empty() => (0..s.tables().len()).map(|ix| (None, ix)).collect(),
            None => self
                .scope
                .iter()
                .zip(self.resolved)
                .filter_map(|(r, ix)| Some((Some(r), (*ix)?)))
                .collect(),
        };
        let qualify = qualifier.is_none() && tables.len() > 1;
        let mut out = Vec::new();
        for (table_ref, ix) in tables {
            let t = s.table(ix);
            for c in s.columns(ix) {
                let Some(score) = match_score(&c.name, self.prefix) else {
                    continue;
                };
                let name = s.quote(&c.name);
                let insert = match table_ref {
                    Some(r) if qualify => format!("{}.{name}", s.quote(r.handle())),
                    _ => name,
                };
                let usage = self
                    .usage
                    .score(UsageKind::Column, &column_key(&t.schema, &t.name, &c.name));
                let pk = if c.primary_key { " · pk" } else { "" };
                out.push(Suggestion {
                    label: insert.clone(),
                    insert,
                    kind: SuggestionKind::Column,
                    detail: format!("{} · {}{pk}", t.name, c.data_type),
                    // Unscoped columns (no FROM yet) rank by usage only.
                    score: score + usage_boost(usage) + if table_ref.is_some() { 0.5 } else { 0.0 },
                });
            }
        }
        out
    }

    /// Join suggestions. With `joined` (after `JOIN t ON`): conditions between that
    /// table and the others. Without (after `JOIN`): `table alias ON condition`.
    fn joins(&self, joined: Option<usize>) -> Vec<Suggestion> {
        let s = self.schema;
        let mut links: Vec<(Link, f64, String)> = Vec::new();
        for fk in s.foreign_keys() {
            if let Some(link) = Link::from_fk(s, fk) {
                let usage = self.join_usage(&link);
                links.push((link, 1.0 + usage_boost(usage), format!("fk {}", fk.name)));
            }
        }
        for (key, score) in self.usage.joins() {
            if let Some(link) = Link::from_usage(s, key)
                && !links.iter().any(|(l, ..)| l.same(&link))
            {
                links.push((link, usage_boost(score), "used before".into()));
            }
        }

        let mut out = Vec::new();
        for (link, base, detail) in links {
            for flip in [false, true] {
                let link = if flip { link.flipped() } else { link.clone() };
                // `link.left` is the table being joined, `link.right` one in scope.
                let Some(right) = self.scope_index(link.right, joined) else {
                    continue;
                };
                let right_handle = self.scope[right].handle().to_string();
                match joined {
                    Some(j) => {
                        if self.resolved[j] != Some(link.left) {
                            continue;
                        }
                        let left_handle = self.scope[j].handle().to_string();
                        let cond = link.condition(s, &left_handle, &right_handle);
                        let Some(score) = match_score(&cond, self.prefix)
                            .or_else(|| match_score(&left_handle, self.prefix))
                        else {
                            continue;
                        };
                        out.push(Suggestion {
                            label: cond.clone(),
                            insert: cond,
                            kind: SuggestionKind::Join,
                            detail: detail.clone(),
                            score: score + base + 1.0,
                        });
                    }
                    None => {
                        let table = s.table(link.left);
                        let Some(score) = match_score(&table.name, self.prefix) else {
                            continue;
                        };
                        let alias = self.new_alias(&table.name);
                        let cond = link.condition(s, &alias, &right_handle);
                        let insert = format!("{} {alias} ON {cond}", s.sql_name(link.left));
                        out.push(Suggestion {
                            label: insert.clone(),
                            insert,
                            kind: SuggestionKind::Join,
                            detail: detail.clone(),
                            score: score + base + 1.0,
                        });
                    }
                }
            }
        }
        out
    }

    /// Scope entry for a schema table, excluding the one being joined.
    fn scope_index(&self, table: usize, joined: Option<usize>) -> Option<usize> {
        self.resolved
            .iter()
            .enumerate()
            .find(|&(i, ix)| {
                *ix == Some(table) && Some(i) != joined && joined.is_none_or(|j| i < j)
            })
            .map(|(i, _)| i)
    }

    fn join_usage(&self, link: &Link) -> f64 {
        let s = self.schema;
        let (l, r) = (s.table(link.left), s.table(link.right));
        let key = crate::usage::join_key(
            (&l.schema, &l.name, &link.left_cols[0]),
            (&r.schema, &r.name, &link.right_cols[0]),
        );
        self.usage.score(UsageKind::Join, &key)
    }

    /// Initials of the table name (`order_items` → `oi`), made unique in scope.
    fn new_alias(&self, table: &str) -> String {
        let base: String = table
            .split(['_', ' '])
            .filter_map(|p| p.chars().next())
            .flat_map(char::to_lowercase)
            .filter(char::is_ascii_alphabetic)
            .collect();
        let base = if base.is_empty() {
            "t".to_string()
        } else {
            base
        };
        let taken = |a: &str| {
            self.scope
                .iter()
                .any(|t| t.handle().eq_ignore_ascii_case(a))
                || crate::analyze::is_reserved(a)
        };
        if !taken(&base) {
            return base;
        }
        (2..)
            .map(|n| format!("{base}{n}"))
            .find(|a| !taken(a))
            .unwrap_or(base)
    }
}

/// A join between two schema tables on column lists.
#[derive(Debug, Clone)]
struct Link {
    left: usize,
    left_cols: Vec<String>,
    right: usize,
    right_cols: Vec<String>,
}

impl Link {
    fn from_fk(s: &SchemaIndex, fk: &ForeignKey) -> Option<Self> {
        Some(Self {
            left: s.resolve(Some(&fk.schema), &fk.table)?,
            left_cols: fk.columns.clone(),
            right: s.resolve(Some(&fk.ref_schema), &fk.ref_table)?,
            right_cols: fk.ref_columns.clone(),
        })
    }

    fn from_usage(s: &SchemaIndex, key: &str) -> Option<Self> {
        let [a, b] = parse_join_key(key)?;
        let col = |ix: usize, name: &str| {
            s.columns(ix)
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(name))
                .map(|c| c.name.clone())
        };
        let (left, right) = (s.resolve(Some(&a.0), &a.1)?, s.resolve(Some(&b.0), &b.1)?);
        Some(Self {
            left,
            left_cols: vec![col(left, &a.2)?],
            right,
            right_cols: vec![col(right, &b.2)?],
        })
    }

    fn flipped(&self) -> Self {
        Self {
            left: self.right,
            left_cols: self.right_cols.clone(),
            right: self.left,
            right_cols: self.left_cols.clone(),
        }
    }

    fn same(&self, other: &Link) -> bool {
        let f = other.flipped();
        [other, &f].iter().any(|o| {
            o.left == self.left
                && o.right == self.right
                && o.left_cols
                    .iter()
                    .zip(&self.left_cols)
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
                && o.right_cols
                    .iter()
                    .zip(&self.right_cols)
                    .all(|(a, b)| a.eq_ignore_ascii_case(b))
        })
    }

    fn condition(&self, s: &SchemaIndex, left: &str, right: &str) -> String {
        let (l, r) = (s.quote(left), s.quote(right));
        self.left_cols
            .iter()
            .zip(&self.right_cols)
            .map(|(a, b)| format!("{l}.{} = {r}.{}", s.quote(a), s.quote(b)))
            .collect::<Vec<_>>()
            .join(" AND ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::sample;

    /// Completes at `|` in `sql`.
    fn at(sql: &str, usage: &Usage) -> (Completion, String) {
        let offset = sql.find('|').unwrap();
        let sql = sql.replace('|', "");
        (complete(&sql, offset, &sample(), usage), sql)
    }

    fn labels(sql: &str) -> Vec<String> {
        at(sql, &Usage::new(0))
            .0
            .items
            .into_iter()
            .map(|i| i.label)
            .collect()
    }

    #[test]
    fn tables_after_from() {
        let l = labels("SELECT * FROM |");
        assert!(l.contains(&"app.users".to_string()) && l.contains(&"\"Logs\"".to_string()));
        assert_eq!(labels("select * from us|")[0], "app.users");
        let mut l = labels("select * from app.|");
        l.sort();
        assert_eq!(l, ["orders", "users"]);
    }

    #[test]
    fn replace_range_covers_prefix() {
        let (c, _) = at("select * from us|", &Usage::new(0));
        assert_eq!(c.replace, 14..16);
    }

    #[test]
    fn columns_by_alias_and_scope() {
        let l = labels("select u.| from app.users u");
        assert_eq!(l.len(), 3);
        assert!(l.contains(&"email".to_string()));
        let l = labels("select * from app.users where em|");
        assert_eq!(l[0], "email");
        let l = labels("select | from app.users u join app.orders o on o.user_id = u.id");
        assert!(l.contains(&"u.email".to_string()) && l.contains(&"o.total".to_string()));
    }

    #[test]
    fn joins_from_foreign_keys() {
        let l = labels("select * from app.users u join |");
        assert_eq!(l[0], "app.orders o ON o.user_id = u.id");
        let l = labels("select * from app.orders o join |");
        assert_eq!(l[0], "app.users u ON u.id = o.user_id");
        let l = labels("select * from app.users u join app.orders o on |");
        assert_eq!(l[0], "o.user_id = u.id");
    }

    #[test]
    fn usage_ranks_first() {
        let mut usage = Usage::new(0);
        for _ in 0..5 {
            usage.record(UsageKind::Table, table_key("public", "Logs"));
        }
        let (c, _) = at("select * from |", &usage);
        assert_eq!(c.items[0].label, "\"Logs\"");
    }

    #[test]
    fn keywords_and_nothing_in_strings() {
        assert!(labels("sel|").contains(&"select".to_string()));
        // Right after a table: likely an alias, so keywords only from two letters on.
        assert!(labels("select * from app.users w|").is_empty());
        assert_eq!(labels("select * from app.users wh|"), ["where"]);
        assert_eq!(labels("SELECT * FROM app.users WH|"), ["WHERE"]);
        assert!(labels("select 'fr|om'").is_empty());
    }

    #[test]
    fn after_star_only_from() {
        assert_eq!(labels("select * f|"), ["from"]);
        assert_eq!(labels("select u.* f|"), ["from"]);
    }

    #[test]
    fn operators_only_after_an_operand() {
        // `where l` starts an operand: columns/functions, never `LIKE`.
        let l = labels("select * from app.users where l|");
        assert!(!l.iter().any(|s| s.contains("like")), "{l:?}");
        assert!(l.contains(&"lower()".to_string()));
        // After a column: operators and continuations.
        assert_eq!(labels("select * from app.users where email l|")[0], "like");
        assert_eq!(
            labels("select * from app.users where email = 'x' a|")[0],
            "and"
        );
        assert!(labels("select * from app.users order by id d|").contains(&"desc".to_string()));
        assert_eq!(
            labels("select * from app.users where name is n|"),
            ["null", "not null"]
        );
        assert_eq!(labels("select * from app.users group |"), ["by"]);
    }

    #[test]
    fn dialects() {
        let words = |driver, sql: &str| -> Vec<String> {
            let offset = sql.find('|').unwrap();
            let sql = sql.replace('|', "");
            let s = crate::schema::tests::sample_for(driver);
            complete(&sql, offset, &s, &Usage::new(0))
                .items
                .into_iter()
                .map(|i| i.label)
                .collect()
        };
        let sql = "select * from app.users where email |";
        let pg = words(rusdb_core::DriverKind::Postgres, &sql.replace('|', "i|"));
        let my = words(rusdb_core::DriverKind::Mysql, &sql.replace('|', "i|"));
        assert!(pg.contains(&"ilike".to_string()) && !my.contains(&"ilike".to_string()));
        let my = words(rusdb_core::DriverKind::Mysql, &sql.replace('|', "r|"));
        assert_eq!(my, ["regexp"]);
        let pg = words(
            rusdb_core::DriverKind::Postgres,
            "insert into app.users values (1) r|",
        );
        assert_eq!(pg, ["returning"]);
        let my = words(
            rusdb_core::DriverKind::Mysql,
            "insert into app.users values (1) o|",
        );
        assert_eq!(my, ["on duplicate key update"]);
    }
}
