//! Editor completion: asks the grammar what the cursor position expects (a table, an
//! operand, an operator or clause keyword, …) and ranks the fitting candidates by
//! match quality and usage.

use std::collections::HashSet;
use std::ops::Range;

use tangul_core::usage::HistoryPattern;
use tangul_core::{ColumnMeta, ForeignKey, TableKind};

use crate::analyze::{TableRef, analyze, significant, statements};
use crate::dialect::Dialect;
use crate::grammar::{self, CasePart, Clause, Expect, State, Vocab};
use crate::hints::ValueHints;
use crate::lexer::{Token, TokenKind, tokenize, unquote};
use crate::profile::is_secret;
use crate::refine::ColumnKind;
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
    /// A literal or expression for a value: `'…'`, an enum's value, `CURRENT_DATE`.
    Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub label: String,
    pub insert: String,
    pub kind: SuggestionKind,
    pub detail: String,
    pub score: f64,
    /// Where the cursor goes in `insert` (a byte offset) when not at its end:
    /// between the quotes of `''`.
    pub cursor: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Completion {
    /// Byte range of `sql` the chosen item replaces (the partially typed word).
    pub replace: Range<usize>,
    pub items: Vec<Suggestion>,
    /// What comes next is clear enough to open the menu before a letter is typed
    /// (after a space, `(` or `'`): a table, a unit, a value for the column, ….
    pub eager: bool,
    /// A text column the cursor compares with whose values from the data aren't
    /// known yet: worth a quick look ([`crate::profile::quick_values_sql`]).
    pub sample: Option<ColumnMeta>,
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

/// `hints`: values for columns, from history and the data.
pub fn complete(
    sql: &str,
    offset: usize,
    schema: &SchemaIndex,
    usage: &Usage,
    hints: &ValueHints,
) -> Completion {
    let offset = offset.min(sql.len());
    let Some((start, end)) = statements(sql)
        .into_iter()
        .find(|&(s, e)| offset >= s && offset <= e)
    else {
        return Completion::default();
    };
    let stmt = &sql[start..end];
    let off = offset - start;

    let tokens = significant(stmt);
    // Inside a comment: nothing. Inside a string: units, formats, an enum's values.
    if let Some(t) = tokenize(stmt).into_iter().find(|t| inside(stmt, t, off)) {
        if t.kind == TokenKind::Comment {
            return Completion::default();
        }
        return in_string(stmt, &tokens, t, off, schema, hints)
            .map(|(range, items, sample)| Completion {
                replace: start + range.start..start + range.end,
                eager: !items.is_empty(),
                items: ranked(items),
                sample,
            })
            .unwrap_or_default();
    }

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
    let resolved = resolve_scope(&scope, schema);

    let target = target(stmt, &before, &scope, schema);
    let cx = Ctx {
        schema,
        usage,
        prefix: &prefix,
        scope: &scope,
        resolved: &resolved,
        lower_keywords: lower_keywords(stmt, &tokens, &prefix),
        hints,
    };
    let mut sample = None;
    let d = schema.dialect();
    let last_word = |words: &[&str]| {
        before
            .last()
            .is_some_and(|t| words.iter().any(|w| t.is_word(stmt, w)))
    };
    let mut eager = true;
    let items = match target {
        Target::Nothing => Vec::new(),
        Target::ColumnsOf(i) => cx.columns(Some(i)),
        Target::TablesIn(s) => cx.tables(Some(&s)),
        Target::State(st) => match st.expect {
            Expect::Statement => {
                eager = false;
                cx.keywords(d.statement_keywords())
            }
            Expect::Words(words) => cx.keywords(words),
            Expect::Vocab(v) => cx.keywords(&vocab(d, v)),
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
                eager = false;
                cx.keywords(&grammar::after_table_keywords(d.syntax(), st.clause, alias))
            }
            Expect::AfterOperand { .. } if st.between => cx.keywords(&["AND"]),
            Expect::AfterOperand { .. } if st.case.is_some() => {
                cx.keywords(case_words(st.case.unwrap_or(CasePart::Case)))
            }
            Expect::AfterOperand { star } => {
                eager = false;
                cx.keywords(&grammar::after_operand_keywords(
                    d.syntax(),
                    st.clause,
                    star,
                ))
            }
            Expect::Operand { first } => {
                let mut items = Vec::new();
                // `CASE |`, `IN (|`: the word that usually follows, first.
                let mut lead = Vec::new();
                if last_word(&["case"]) {
                    lead.push("WHEN");
                }
                if before.last().is_some_and(|t| t.is_punct(stmt, '('))
                    && before.len() >= 2
                    && ["in", "any", "some", "all"]
                        .iter()
                        .any(|w| before[before.len() - 2].is_word(stmt, w))
                {
                    lead.push("SELECT");
                }
                items.extend(cx.keywords(&lead).into_iter().map(|mut i| {
                    i.score += 3.0;
                    i
                }));
                // Values that fit the column it is compared with.
                if let Some((column, op)) = comparison(stmt, &before)
                    && let Some(meta) = column_meta(schema, &scope, &resolved, &column)
                {
                    items.extend(cx.values_for(meta, op));
                    sample = wants_sample(schema, hints, meta).then(|| meta.clone());
                }
                eager = !items.is_empty()
                    || last_word(&[
                        "where", "and", "or", "on", "when", "then", "else", "by", "having", "set",
                    ]);
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
                    items.extend(cx.functions(d.functions()));
                }
                items.extend(cx.keywords(grammar::operand_keywords(st.clause, first)));
                // `CURRENT_DATE`, `INTERVAL`, …: only once typing started.
                if !prefix.is_empty() && grammar::operand_takes_columns(st.clause) {
                    let mut words = vec!["CURRENT_DATE", "CURRENT_TIMESTAMP", "TRUE", "FALSE"];
                    if !d.interval_units().is_empty() {
                        words.insert(0, "INTERVAL");
                    }
                    items.extend(cx.keywords(&words));
                }
                items
            }
        },
    };
    let items = ranked(items);
    Completion {
        replace: start + replace_start..offset,
        eager: eager && !items.is_empty(),
        items,
        sample,
    }
}

/// Whether a quick look at `column`'s data would add values: a text column, not an
/// enum, not a secret, not looked at yet.
fn wants_sample(schema: &SchemaIndex, hints: &ValueHints, column: &ColumnMeta) -> bool {
    ColumnKind::from_type(&column.data_type) == ColumnKind::Text
        && schema.enum_of(column).is_none()
        && !is_secret(&column.name)
        && !hints.has_data(&column_key(&column.schema, &column.table, &column.name))
}

/// The text of a string literal (`'it''s'` → `it's`); `None` for other literals.
fn string_text(literal: &str) -> Option<String> {
    let inner = literal.strip_prefix('\'')?.strip_suffix('\'')?;
    Some(inner.replace("''", "'"))
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
    hints: &'a ValueHints,
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
    /// Values for `column` compared by `op`: the ones used before and frequent in
    /// the data, an enum's values, `''` with the cursor inside, today and the last
    /// days for dates, `TRUE`/`FALSE`. Above columns.
    fn values_for(&self, column: &ColumnMeta, op: Op) -> Vec<Suggestion> {
        let case = |v: &str| {
            if self.lower_keywords {
                v.to_lowercase()
            } else {
                v.to_string()
            }
        };
        let mut values: Vec<(String, String, Option<usize>)> = Vec::new();
        let key = column_key(&column.schema, &column.table, &column.name);
        for h in self.hints.for_column(&key) {
            if h.like == (op == Op::Like) {
                values.push((h.literal.clone(), h.literal.clone(), None));
            }
        }
        let quoted = |v: &str| self.schema.dialect().string(v);
        if let Some(e) = self.schema.enum_of(column) {
            values.extend(e.values.iter().map(|v| (quoted(v), quoted(v), None)));
        }
        match ColumnKind::from_type(&column.data_type) {
            ColumnKind::Text if op == Op::Like => {
                values.push(("'%…%'".into(), "'%%'".into(), Some(2)));
            }
            ColumnKind::Text => values.push(("'…'".into(), "''".into(), Some(1))),
            ColumnKind::Date | ColumnKind::DateTime if op != Op::Like => {
                for v in self.schema.dialect().date_values() {
                    values.push((case(v), case(v), None));
                }
                values.push(("'…'".into(), "''".into(), Some(1)));
            }
            ColumnKind::Bool if op == Op::Compare => {
                values.extend(["TRUE", "FALSE"].map(|v| (case(v), case(v), None)));
            }
            _ => {}
        }
        values
            .into_iter()
            .enumerate()
            .filter_map(|(n, (label, insert, cursor))| {
                let score = match_score(&label, self.prefix)?;
                Some(Suggestion {
                    label,
                    insert,
                    kind: SuggestionKind::Value,
                    detail: column.data_type.clone(),
                    score: score + 3.0 - n as f64 * 0.01,
                    cursor,
                })
            })
            .collect()
    }

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
                    cursor: None,
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
                    cursor: None,
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
                let kind = match t.kind.group() {
                    TableKind::View => SuggestionKind::View,
                    _ => SuggestionKind::Table,
                };
                let noun = t.kind.label();
                Some(Suggestion {
                    label: insert.clone(),
                    insert,
                    kind,
                    detail: format!("{noun} · {}", t.schema),
                    score: score + usage_boost(usage),
                    cursor: None,
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
                    cursor: None,
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
                            cursor: None,
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
                            cursor: None,
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

/// Best first, each insert once, at most [`MAX_ITEMS`].
fn ranked(mut items: Vec<Suggestion>) -> Vec<Suggestion> {
    items.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.label.len().cmp(&b.label.len()))
            .then(a.label.cmp(&b.label))
    });
    let mut seen = HashSet::new();
    items.retain(|i| seen.insert(i.insert.clone()));
    items.truncate(MAX_ITEMS);
    items
}

/// Whether `off` is inside `t`, a string or comment (also one still open at the end).
fn inside(sql: &str, t: &Token, off: usize) -> bool {
    let text = t.text(sql);
    let open = match t.kind {
        TokenKind::String => text.starts_with('\'') && (text.len() == 1 || !text.ends_with('\'')),
        TokenKind::Comment => !text.starts_with("/*"),
        _ => return false,
    };
    t.start < off && (off < t.end || (off == t.end && open))
}

/// The words of a dialect's vocabulary.
fn vocab(d: &dyn Dialect, v: Vocab) -> Vec<&'static str> {
    match v {
        Vocab::IntervalUnit => d.interval_units().to_vec(),
        Vocab::DateField => d.date_fields().to_vec(),
        Vocab::Type => d.cast_types().to_vec(),
        Vocab::Negated => {
            let x = d.syntax();
            let mut words = vec!["IN", "LIKE", "BETWEEN"];
            if x.ilike {
                words.push("ILIKE");
            }
            if x.regexp {
                words.push("REGEXP");
            }
            words
        }
    }
}

/// What may follow an operand inside `CASE`, after its last word.
fn case_words(part: CasePart) -> &'static [&'static str] {
    match part {
        CasePart::Case => &["WHEN"],
        CasePart::When => &[
            "THEN",
            "AND",
            "OR",
            "IN",
            "LIKE",
            "IS NULL",
            "IS NOT NULL",
            "BETWEEN",
        ],
        CasePart::Then => &["WHEN", "ELSE", "END"],
        CasePart::Else => &["END"],
    }
}

/// How a value is compared with a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    /// `=`, `<>`, `<`, …
    Compare,
    /// `LIKE`, `ILIKE`
    Like,
    /// `IN (…`
    In,
}

/// A column named in the statement: `(qualifier, name)`.
pub(crate) type ColumnName = (Option<String>, String);

/// The column a value at the end of `before` is compared with: `col = |`,
/// `t.col LIKE |`, `col IN ('a', |`.
pub(crate) fn comparison(sql: &str, before: &[Token]) -> Option<(ColumnName, Op)> {
    let at = |i: usize| before.get(i).copied();
    let word = |i: usize, w: &str| at(i).is_some_and(|t| t.is_word(sql, w));
    let punct = |i: usize, c: char| at(i).is_some_and(|t| t.is_punct(sql, c));
    // Skip the values of an IN list typed so far.
    let mut j = before.len();
    while j > 0
        && at(j - 1).is_some_and(|t| {
            matches!(t.kind, TokenKind::String | TokenKind::Number) || t.is_punct(sql, ',')
        })
    {
        j -= 1;
    }
    let mut k = before.len();
    let op = if j >= 2 && punct(j - 1, '(') && word(j - 2, "in") {
        k = j - 2;
        Op::In
    } else if k >= 1 && (word(k - 1, "like") || word(k - 1, "ilike")) {
        k -= 1;
        Op::Like
    } else {
        let mut n = 0;
        while n < 2 && k > n && ['=', '<', '>', '!'].iter().any(|&c| punct(k - 1 - n, c)) {
            n += 1;
        }
        if n == 0 {
            return None;
        }
        k -= n;
        Op::Compare
    };
    if k >= 1 && word(k - 1, "not") {
        k -= 1;
    }
    let name = at(k.checked_sub(1)?).filter(|t| t.is_ident())?;
    let qualifier = (k >= 3 && punct(k - 2, '.'))
        .then(|| at(k - 3).filter(|t| t.is_ident()))
        .flatten()
        .map(|t| unquote(t.text(sql)));
    Some(((qualifier, unquote(name.text(sql))), op))
}

/// The function call whose argument `tokens[i]` is: (name in lower case, argument
/// index from 0).
fn call_of(sql: &str, tokens: &[Token], i: usize) -> Option<(String, usize)> {
    let (mut depth, mut arg) = (0usize, 0usize);
    for j in (0..i).rev() {
        let t = tokens[j];
        if t.is_punct(sql, ')') {
            depth += 1;
        } else if t.is_punct(sql, '(') {
            if depth == 0 {
                let f = tokens.get(j.checked_sub(1)?).filter(|t| t.is_ident())?;
                return Some((f.text(sql).to_ascii_lowercase(), arg));
            }
            depth -= 1;
        } else if depth == 0 && t.is_punct(sql, ',') {
            arg += 1;
        }
    }
    None
}

/// A value inside a string literal, inserted as it is.
fn value_item(label: &str, prefix: &str, n: usize, detail: &str) -> Option<Suggestion> {
    let score = match_score(label, prefix)?;
    Some(Suggestion {
        label: label.to_string(),
        insert: label.replace('\'', "''"),
        kind: SuggestionKind::Value,
        detail: detail.to_string(),
        score: score - n as f64 * 0.01,
        cursor: None,
    })
}

/// Completion inside the string literal `t` at `off`: the range to replace and the
/// items.
fn in_string(
    sql: &str,
    tokens: &[Token],
    t: Token,
    off: usize,
    schema: &SchemaIndex,
    hints: &ValueHints,
) -> Option<(Range<usize>, Vec<Suggestion>, Option<ColumnMeta>)> {
    let d = schema.dialect();
    let i = tokens.iter().position(|x| x.start == t.start)?;
    let content = t.start + 1;
    let typed = &sql[content..off];

    // `INTERVAL '7 d|'`: the unit after the number.
    if i > 0 && tokens[i - 1].is_word(sql, "interval") {
        let word_start = typed.trim_end_matches(char::is_alphabetic).len();
        let number = typed[..word_start].trim();
        let units = d.interval_text_units();
        if units.is_empty() || !number.ends_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        let plural = number.rsplit(' ').next() != Some("1");
        let prefix = &typed[word_start..];
        let items = units
            .iter()
            .enumerate()
            .filter_map(|(n, u)| {
                let unit = if plural {
                    format!("{u}s")
                } else {
                    u.to_string()
                };
                value_item(&unit, prefix, n, "unit")
            })
            .collect();
        return Some((content + word_start..off, items, None));
    }

    // An argument with fixed values: `DATE_TRUNC('|'`, `TO_CHAR(d, '|'`.
    if let Some((function, arg)) = call_of(sql, tokens, i) {
        let choices = d.string_choices(&function, arg);
        if !choices.is_empty() {
            let items = choices
                .iter()
                .enumerate()
                .filter_map(|(n, c)| value_item(c, typed, n, &function))
                .collect();
            return Some((content..off, items, None));
        }
    }

    // `status = '|`: the column's values used before, frequent in the data, of its
    // enum.
    let (column, op) = comparison(sql, &tokens[..i])?;
    let scope = analyze(sql).tables;
    let resolved = resolve_scope(&scope, schema);
    let meta = column_meta(schema, &scope, &resolved, &column)?;
    let key = column_key(&meta.schema, &meta.table, &meta.name);
    let mut values: Vec<String> = hints
        .for_column(&key)
        .iter()
        .filter(|h| h.like == (op == Op::Like))
        .filter_map(|h| string_text(&h.literal))
        .collect();
    if let Some(e) = schema.enum_of(meta) {
        values.extend(e.values.iter().cloned());
    }
    let items = values
        .iter()
        .enumerate()
        .filter_map(|(n, v)| value_item(v, typed, n, "value"))
        .collect();
    let sample = wants_sample(schema, hints, meta).then(|| meta.clone());
    Some((content..off, items, sample))
}

pub(crate) fn resolve_scope(scope: &[TableRef], schema: &SchemaIndex) -> Vec<Option<usize>> {
    scope
        .iter()
        .map(|t| schema.resolve(t.schema.as_deref(), &t.name))
        .collect()
}

/// The column `name` refers to among the tables in scope.
pub(crate) fn column_meta<'a>(
    schema: &'a SchemaIndex,
    scope: &[TableRef],
    resolved: &[Option<usize>],
    (qualifier, name): &ColumnName,
) -> Option<&'a ColumnMeta> {
    let tables: Vec<usize> = match qualifier {
        Some(q) => {
            let i = scope.iter().position(|t| t.answers_to(q))?;
            resolved[i].into_iter().collect()
        }
        None => resolved.iter().flatten().copied().collect(),
    };
    tables.into_iter().find_map(|ix| {
        schema
            .columns(ix)
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    })
}

/// The rest of the statement at `offset` as run before: of the queries in
/// `history` (most run first), the first that starts with what was typed (apart
/// from case and spacing), from the cursor on. Only at the end of a statement.
pub fn continuation(sql: &str, offset: usize, history: &[HistoryPattern]) -> Option<String> {
    let offset = offset.min(sql.len());
    if !sql[offset..].trim().is_empty() && !sql[offset..].trim_start().starts_with(';') {
        return None;
    }
    let start = statements(sql)
        .into_iter()
        .find(|&(s, e)| offset >= s && offset <= e)
        .map_or(offset, |(s, _)| s);
    let typed = sql[start..offset].trim_start();
    if typed.is_empty() {
        return None;
    }
    history.iter().find_map(|p| {
        let rest = p.sql[continues(&p.sql, typed)?..]
            .trim_end()
            .trim_end_matches(';')
            .trim_end();
        (!rest.trim().is_empty()).then(|| rest.to_string())
    })
}

/// Where `pattern` goes on after `typed`, a beginning of it apart from case and the
/// length of spaces.
fn continues(pattern: &str, typed: &str) -> Option<usize> {
    let mut p = pattern.char_indices().peekable();
    let mut t = typed.chars().peekable();
    while let Some(c) = t.next() {
        if c.is_whitespace() {
            while t.next_if(|c| c.is_whitespace()).is_some() {}
            p.next_if(|(_, c)| c.is_whitespace())?;
            while p.next_if(|(_, c)| c.is_whitespace()).is_some() {}
            continue;
        }
        let (_, pc) = p.next()?;
        if !pc.to_lowercase().eq(c.to_lowercase()) {
            return None;
        }
    }
    Some(p.peek().map_or(pattern.len(), |&(i, _)| i))
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
        (
            complete(&sql, offset, &sample(), usage, &ValueHints::default()),
            sql,
        )
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
            complete(&sql, offset, &s, &Usage::new(0), &ValueHints::default())
                .items
                .into_iter()
                .map(|i| i.label)
                .collect()
        };
        let sql = "select * from app.users where email |";
        let pg = words(tangul_core::DriverKind::Postgres, &sql.replace('|', "i|"));
        let my = words(tangul_core::DriverKind::Mysql, &sql.replace('|', "i|"));
        assert!(pg.contains(&"ilike".to_string()) && !my.contains(&"ilike".to_string()));
        let my = words(tangul_core::DriverKind::Mysql, &sql.replace('|', "r|"));
        assert_eq!(my, ["regexp"]);
        let pg = words(
            tangul_core::DriverKind::Postgres,
            "insert into app.users values (1) r|",
        );
        assert_eq!(pg, ["returning"]);
        let my = words(
            tangul_core::DriverKind::Mysql,
            "insert into app.users values (1) o|",
        );
        assert_eq!(my, ["on duplicate key update"]);
    }

    /// `sample()` plus `app.users.created_at` (timestamp), `active` (boolean) and
    /// `status` (enum `app.status`), in `driver`'s dialect.
    fn rich(driver: tangul_core::DriverKind) -> SchemaIndex {
        let mut info = crate::schema::tests::sample_for(driver).info().clone();
        let col = |name: &str, ty: &str, enum_type: Option<&str>| ColumnMeta {
            schema: "app".into(),
            table: "users".into(),
            name: name.into(),
            data_type: ty.into(),
            nullable: true,
            enum_type: enum_type.map(Into::into),
            ..Default::default()
        };
        let after_users = info
            .columns
            .iter()
            .rposition(|c| c.table == "users")
            .unwrap()
            + 1;
        info.columns.splice(
            after_users..after_users,
            [
                col("created_at", "timestamp with time zone", None),
                col("active", "boolean", None),
                col("status", "status", Some("app.status")),
            ],
        );
        info.enums.push(tangul_core::EnumInfo {
            schema: "app".into(),
            name: "status".into(),
            values: vec!["active".into(), "invited".into(), "it's off".into()],
            comment: None,
        });
        SchemaIndex::new(
            info,
            crate::dialect::dialect_for(driver),
            Some("public".into()),
        )
    }

    fn on(driver: tangul_core::DriverKind, sql: &str) -> Completion {
        let offset = sql.find('|').unwrap();
        complete(
            &sql.replace('|', ""),
            offset,
            &rich(driver),
            &Usage::new(0),
            &ValueHints::default(),
        )
    }

    fn pg(sql: &str) -> Vec<String> {
        on(tangul_core::DriverKind::Postgres, sql)
            .items
            .into_iter()
            .map(|i| i.label)
            .collect()
    }

    fn my(sql: &str) -> Vec<String> {
        on(tangul_core::DriverKind::Mysql, sql)
            .items
            .into_iter()
            .map(|i| i.label)
            .collect()
    }

    #[test]
    fn intervals_and_their_units() {
        let q = "select * from app.users where created_at > now() - ";
        assert!(pg(&format!("{q}in|")).contains(&"interval".to_string()));
        assert_eq!(
            pg(&format!("{q}interval 1 |"))[..3],
            ["day", "hour", "minute"]
        );
        assert_eq!(pg(&format!("{q}interval 1 h|")), ["hour"]);
        assert_eq!(my(&format!("{q}interval 7 w|")), ["week"]);
        // Inside the text: singular after 1, plural otherwise.
        assert_eq!(pg(&format!("{q}interval '7 d|'")), ["days"]);
        assert_eq!(pg(&format!("{q}interval '1 d|")), ["day"]);
        let c = on(
            tangul_core::DriverKind::Postgres,
            &format!("{q}interval '7 d|'"),
        );
        assert!(c.eager);
        assert_eq!(c.replace.len(), 1);
        assert!(!pg(&format!("{q}interval '7 days' |")).contains(&"day".to_string()));
        // SQLite has no INTERVAL.
        let sqlite = on(
            tangul_core::DriverKind::Sqlite,
            "select * from t where a > in|",
        );
        assert!(!sqlite.items.iter().any(|i| i.label == "interval"));
    }

    #[test]
    fn fields_types_and_fixed_arguments() {
        assert_eq!(pg("select extract(|")[..3], ["year", "month", "day"]);
        assert_eq!(pg("select extract(y|"), ["year"]);
        assert_eq!(pg("select extract(year |"), ["from"]);
        assert!(pg("select cast(id as |").contains(&"timestamptz".to_string()));
        assert!(pg("select id::|").contains(&"bigint".to_string()));
        assert!(my("select cast(id as |").contains(&"signed".to_string()));
        assert_eq!(
            pg("select date_trunc('|', created_at) from app.users")[0],
            "day"
        );
        assert_eq!(
            pg("select date_trunc('mo|', created_at) from app.users"),
            ["month"]
        );
        assert_eq!(
            my("select date_format(created_at, '|') from app.users")[0],
            "%Y-%m-%d"
        );
        // Other strings and comments: nothing.
        assert!(pg("select 'mo|'").is_empty());
        assert!(pg("select 1 -- fr|").is_empty());
    }

    #[test]
    fn values_that_fit_the_column() {
        let c = on(
            tangul_core::DriverKind::Postgres,
            "select * from app.users where email = |",
        );
        assert!(c.eager, "{:?}", c.items);
        assert_eq!(
            (c.items[0].insert.as_str(), c.items[0].cursor),
            ("''", Some(1))
        );
        let c = on(
            tangul_core::DriverKind::Postgres,
            "select * from app.users u where u.email like |",
        );
        assert_eq!(
            (c.items[0].insert.as_str(), c.items[0].cursor),
            ("'%%'", Some(2))
        );
        assert_eq!(
            pg("select * from app.users where created_at >= |")[..2],
            ["current_date", "now() - interval '7 days'"]
        );
        assert_eq!(
            my("select * from app.users where created_at >= |")[1],
            "now() - interval 7 day"
        );
        assert_eq!(
            pg("select * from app.users where active = |")[..2],
            ["true", "false"]
        );
        assert_eq!(pg("select * from app.users where active <> f|")[0], "false");
        // An enum: its values, also inside the quotes and in IN lists.
        assert_eq!(
            pg("select * from app.users where status = |")[..3],
            ["'active'", "'invited'", "'it''s off'"]
        );
        assert_eq!(
            pg("select * from app.users where status = 'in|'"),
            ["invited"]
        );
        let c = on(
            tangul_core::DriverKind::Postgres,
            "select * from app.users where status in ('active', 'it|",
        );
        assert_eq!(c.items[0].insert, "it''s off");
        assert!(
            pg("select * from app.users where status not in (|").contains(&"'invited'".to_string())
        );
        // Numbers: no value to offer, the columns as before.
        let c = on(
            tangul_core::DriverKind::Postgres,
            "select * from app.users where id = |",
        );
        assert!(!c.eager && c.items.iter().all(|i| i.kind != SuggestionKind::Value));
    }

    #[test]
    fn case_between_and_subqueries() {
        assert_eq!(
            pg("select case when id = 1 |"),
            [
                "then",
                "and",
                "or",
                "in",
                "like",
                "is null",
                "is not null",
                "between"
            ]
        );
        assert_eq!(
            pg("select case when id = 1 then 2 |"),
            ["when", "else", "end"]
        );
        assert_eq!(pg("select case when id = 1 then 2 e|"), ["else", "end"]);
        assert_eq!(pg("select case |")[0], "when");
        assert_eq!(pg("select * from app.users where id between 1 |"), ["and"]);
        assert_eq!(pg("select * from app.users where id in (sel|"), ["select"]);
        assert_eq!(pg("select * from app.users where exists (|"), ["select"]);
        assert_eq!(pg("select count(*) over (|"), ["partition by", "order by"]);
        assert_eq!(
            pg("select * from app.users where email not |"),
            ["in", "like", "between", "ilike"]
        );
        assert_eq!(
            pg("select * from app.users where cur|")[..2],
            ["current_date", "current_timestamp"]
        );
    }

    #[test]
    fn eager_only_where_the_next_word_is_clear() {
        let eager = |sql: &str| on(tangul_core::DriverKind::Postgres, sql).eager;
        for sql in [
            "select * from |",
            "select * from app.users where |",
            "select * from app.users where id = 1 and |",
            "select * from app.users order by |",
            "select * from app.users u join |",
            "select now() - interval 1 |",
            "select case when id = 1 |",
        ] {
            assert!(eager(sql), "{sql}");
        }
        for sql in [
            "|",
            "select |",
            "select id |",
            "select * from app.users |",
            "select * from app.users where id |",
            "select * from app.users where id = 1 |",
        ] {
            assert!(!eager(sql), "{sql}");
        }
    }

    #[test]
    fn continuation_from_history() {
        let history = [
            HistoryPattern {
                sql: "SELECT *\nFROM app.users\nWHERE created_at > now() - interval '7 days';"
                    .into(),
                runs: 9,
            },
            HistoryPattern {
                sql: "select * from app.orders".into(),
                runs: 3,
            },
        ];
        let at = |sql: &str| continuation(sql, sql.len(), &history);
        assert_eq!(
            at("select * from app.users where "),
            Some("created_at > now() - interval '7 days'".into())
        );
        assert_eq!(at("select * from app.o"), Some("rders".into()));
        assert_eq!(
            at("SELECT  *  FROM"),
            Some(" app.users\nWHERE created_at > now() - interval '7 days'".into())
        );
        assert_eq!(at("select * from app.orders"), None);
        assert_eq!(at("select count(*)"), None);
        assert_eq!(at(""), None);
        // Only at the end of the statement; a later statement starts over.
        assert_eq!(continuation("select * from app.o x", 17, &history), None);
        assert_eq!(at("select 1; select * from app.o"), Some("rders".into()));
    }

    #[test]
    fn values_used_before_and_in_the_data() {
        let schema = rich(tangul_core::DriverKind::Postgres);
        let history = [HistoryPattern {
            sql: "select * from app.users where name = 'bob' or email like '%@x.io'".into(),
            runs: 3,
        }];
        let mut hints = ValueHints::from_history(&history, &schema);
        let at = |sql: &str, hints: &ValueHints| {
            let offset = sql.find('|').unwrap();
            complete(
                &sql.replace('|', ""),
                offset,
                &schema,
                &Usage::new(0),
                hints,
            )
        };
        let labels = |c: &Completion| c.items.iter().map(|i| i.label.clone()).collect::<Vec<_>>();

        // From history; the data not looked at yet: asked for.
        let c = at("select * from app.users where name = |", &hints);
        assert_eq!(labels(&c)[..2], ["'bob'", "'…'"]);
        assert_eq!(c.sample.as_ref().map(|m| m.name.as_str()), Some("name"));
        let c = at("select * from app.users where email like |", &hints);
        assert_eq!(labels(&c)[0], "'%@x.io'");

        // Then the data's, after history's; nothing more to ask for.
        let key = column_key("app", "users", "name");
        hints.add_data(key, &["ann".into(), "bob".into()], schema.dialect());
        let c = at("select * from app.users where name = |", &hints);
        assert_eq!(labels(&c)[..3], ["'bob'", "'ann'", "'…'"]);
        assert!(c.sample.is_none());
        let c = at("select * from app.users where name in ('bob', 'a|", &hints);
        assert_eq!(labels(&c), ["ann"]);
        assert!(c.sample.is_none());

        // Not worth a look: enums, secrets, numbers.
        for sql in [
            "select * from app.users where status = |",
            "select * from app.users where id = |",
        ] {
            assert!(at(sql, &hints).sample.is_none(), "{sql}");
        }
    }
}
