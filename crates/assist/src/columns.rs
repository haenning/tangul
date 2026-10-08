//! Where the columns of a result come from: the table column behind each one, found
//! with the schema loaded for completion, so the grid can mark keys and knows the
//! column types without asking the server again.
//!
//! The last statement's select list is read item by item (`*`, `t.*`, `[t.]col
//! [[AS] alias]`, or any other expression). When the items account for exactly the
//! result's columns they are matched by position, which also tells apart two `id`s
//! of a join. Otherwise a column is matched by name, if exactly one table of the
//! FROM clause has it.

use crate::analyze::{TableRef, analyze, is_reserved, statements};
use crate::lexer::{Token, TokenKind, tokenize, unquote};
use crate::related::numeric_type;
use crate::schema::SchemaIndex;

/// What the schema says about a result column that is a plain table column.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColumnFacts {
    /// Short table name.
    pub table: String,
    pub column: String,
    /// Type as the server spells it.
    pub data_type: String,
    pub numeric: bool,
    pub primary_key: bool,
    /// The column it points to, when it is part of a foreign key.
    pub references: Option<ForeignRef>,
    pub nullable: bool,
    /// A unique index or constraint (not the primary key) that includes it.
    pub unique: Unique,
    /// Name of its enum type and the number of values (never the values).
    pub enum_type: Option<(String, usize)>,
    pub default: Option<String>,
    /// A generated (computed) column.
    pub generated: bool,
    /// Identity, serial or auto-increment.
    pub identity: bool,
    /// Names of the non-unique (or partial) indexes it is part of.
    pub indexes: Vec<String>,
    pub comment: Option<String>,
}

/// How a column takes part in unique indexes.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Unique {
    #[default]
    No,
    /// A unique index over this column alone.
    Alone,
    /// Only in a unique index over several columns: the other ones.
    With(Vec<String>),
}

/// Longest text of a tag before it is cut off.
const TAG_WIDTH: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagKind {
    Type,
    Nullable,
    Unique,
    Enum,
    Default,
    Computed,
    Auto,
    Index,
}

/// One pill of the column card.
#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    pub kind: TagKind,
    pub text: String,
}

fn cut(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= TAG_WIDTH {
        return text;
    }
    let head: String = text.chars().take(TAG_WIDTH - 1).collect();
    format!("{}…", head.trim_end())
}

impl ColumnFacts {
    /// What the schema says about the column, as short tags in a fixed order.
    pub fn tags(&self) -> Vec<Tag> {
        let tag = |kind, text: String| Tag { kind, text };
        // An enum's tag names its type already, so it takes the type's place.
        let mut tags = vec![match &self.enum_type {
            Some((name, count)) => {
                let name = name.rsplit('.').next().unwrap_or(name);
                tag(TagKind::Enum, format!("{} ·{count}", cut(name)))
            }
            None => tag(TagKind::Type, cut(&self.data_type)),
        }];
        if self.nullable {
            tags.push(tag(TagKind::Nullable, "null".into()));
        }
        match &self.unique {
            Unique::No => {}
            Unique::Alone => tags.push(tag(TagKind::Unique, "unique".into())),
            Unique::With(others) => {
                let text = match others.as_slice() {
                    [one] => format!("unique +{}", cut(one)),
                    more => format!("unique +{}", more.len()),
                };
                tags.push(tag(TagKind::Unique, text));
            }
        }
        if let Some(default) = &self.default {
            tags.push(tag(TagKind::Default, cut(default)));
        }
        if self.generated {
            tags.push(tag(TagKind::Computed, "computed".into()));
        }
        if self.identity {
            tags.push(tag(TagKind::Auto, "auto".into()));
        }
        match self.indexes.as_slice() {
            [] => {}
            [one] => tags.push(tag(TagKind::Index, cut(one))),
            more => tags.push(tag(TagKind::Index, format!("{} indexes", more.len()))),
        }
        tags
    }
}

/// The target of a foreign key column.
#[derive(Debug, Clone, PartialEq)]
pub struct ForeignRef {
    pub schema: String,
    pub table: String,
    pub column: String,
}

impl std::fmt::Display for ForeignRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.table, self.column)
    }
}

/// One item of a select list.
#[derive(Debug, PartialEq)]
enum Item {
    /// `*`, or `t.*` (qualifier).
    Star(Option<String>),
    /// `[t.]col`, optionally renamed.
    Column {
        qualifier: Option<String>,
        name: String,
    },
    /// Anything else: one column of unknown origin.
    Expr,
}

/// The facts per result column (`columns` are the result's column names); `None`
/// for expressions and columns that can't be told apart.
pub fn column_facts(
    sql: &str,
    columns: &[String],
    schema: &SchemaIndex,
) -> Vec<Option<ColumnFacts>> {
    let unknown = vec![None; columns.len()];
    let Some(stmt) = statements(sql)
        .into_iter()
        .map(|(s, e)| &sql[s..e])
        .rfind(|s| !s.trim().is_empty())
    else {
        return unknown;
    };
    let Some(query) = main_query(stmt) else {
        return unknown;
    };
    let refs = analyze(stmt).tables;
    let from: Vec<Option<(&TableRef, usize)>> = query
        .from
        .iter()
        .map(|&start| {
            let r = refs.iter().find(|r| r.start == start)?;
            Some((r, schema.resolve(r.schema.as_deref(), &r.name)?))
        })
        .collect();
    // A subquery, function or unknown table in FROM: unqualified names may be its.
    let complete = !query.opaque && from.iter().all(Option::is_some);
    let from: Vec<(&TableRef, usize)> = from.into_iter().flatten().collect();
    let by_qualifier = |q: &str| {
        from.iter()
            .find(|(r, _)| r.answers_to(q))
            .map(|&(_, ix)| ix)
            .or_else(|| {
                // Qualified by a table that is only in a subquery or join condition.
                refs.iter()
                    .find(|r| r.answers_to(q))
                    .and_then(|r| schema.resolve(r.schema.as_deref(), &r.name))
            })
    };
    let facts = |table: usize, name: &str| facts_for(schema, table, name);
    let unqualified = |name: &str| {
        if !complete {
            return None;
        }
        let mut found = from.iter().filter(|(_, ix)| {
            schema
                .columns(*ix)
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case(name))
        });
        match (found.next(), found.next()) {
            (Some(&(_, ix)), None) => Some(ix),
            _ => None,
        }
    };

    // Expand the items to one entry per output column, if every star can be.
    let mut expanded: Option<Vec<Option<ColumnFacts>>> = Some(Vec::new());
    for item in &query.items {
        let Some(out) = expanded.as_mut() else { break };
        match item {
            Item::Star(qualifier) => {
                let tables: Option<Vec<usize>> = match qualifier {
                    Some(q) => by_qualifier(q).map(|ix| vec![ix]),
                    None => complete.then(|| from.iter().map(|&(_, ix)| ix).collect()),
                };
                match tables {
                    Some(tables) => {
                        for ix in tables {
                            for c in schema.columns(ix) {
                                out.push(facts(ix, &c.name));
                            }
                        }
                    }
                    None => expanded = None,
                }
            }
            Item::Column { qualifier, name } => {
                let table = match qualifier {
                    Some(q) => by_qualifier(q),
                    None => unqualified(name),
                };
                out.push(table.and_then(|ix| facts(ix, name)));
            }
            Item::Expr => out.push(None),
        }
    }
    if let Some(expanded) = expanded.filter(|e| e.len() == columns.len()) {
        return expanded;
    }
    columns
        .iter()
        .map(|name| unqualified(name).and_then(|ix| facts(ix, name)))
        .collect()
}

fn facts_for(schema: &SchemaIndex, table: usize, name: &str) -> Option<ColumnFacts> {
    let col = schema
        .columns(table)
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(name))?;
    let t = schema.table(table);
    let references = schema.foreign_keys().iter().find_map(|fk| {
        if !(fk.schema.eq_ignore_ascii_case(&t.schema) && fk.table.eq_ignore_ascii_case(&t.name)) {
            return None;
        }
        let i = fk.columns.iter().position(|c| c == &col.name)?;
        Some(ForeignRef {
            schema: fk.ref_schema.clone(),
            table: fk.ref_table.clone(),
            column: fk.ref_columns.get(i)?.clone(),
        })
    });
    let (mut unique, mut indexes) = (Unique::No, Vec::new());
    let mut composite: Option<Vec<String>> = None;
    for ix in &schema.info().indexes {
        if !(ix.schema.eq_ignore_ascii_case(&t.schema)
            && ix.table.eq_ignore_ascii_case(&t.name)
            && !ix.primary
            && ix.columns.iter().any(|c| c.eq_ignore_ascii_case(&col.name)))
        {
            continue;
        }
        // A partial index doesn't make the column unique.
        if ix.unique && ix.predicate.is_none() {
            if ix.columns.len() == 1 {
                unique = Unique::Alone;
            } else if composite
                .as_ref()
                .is_none_or(|c| ix.columns.len() - 1 < c.len())
            {
                composite = Some(
                    ix.columns
                        .iter()
                        .filter(|c| !c.eq_ignore_ascii_case(&col.name))
                        .cloned()
                        .collect(),
                );
            }
        } else {
            indexes.push(ix.name.clone());
        }
    }
    if unique == Unique::No {
        unique = composite.map_or(Unique::No, Unique::With);
    }
    Some(ColumnFacts {
        table: t.name.clone(),
        column: col.name.clone(),
        data_type: col.data_type.clone(),
        numeric: numeric_type(&col.data_type),
        primary_key: col.primary_key,
        references,
        nullable: col.nullable,
        unique,
        enum_type: schema
            .enum_of(col)
            .map(|e| (e.name.clone(), e.values.len())),
        default: col.default.clone().filter(|d| !d.trim().is_empty()),
        generated: col.generated.is_some(),
        identity: col.identity,
        indexes,
        comment: col.comment.clone().filter(|c| !c.trim().is_empty()),
    })
}

/// The outermost SELECT of a statement.
struct MainQuery {
    items: Vec<Item>,
    /// Start offsets of the table references in its FROM clause.
    from: Vec<usize>,
    /// FROM has an item that is not a plain table (subquery, function call, LATERAL).
    opaque: bool,
}

fn main_query(sql: &str) -> Option<MainQuery> {
    // Top-level tokens only; parenthesized parts (CTEs, subqueries, calls) are kept as
    // one `(` token so items and FROM entries can still be told apart.
    let mut top: Vec<Token> = Vec::new();
    let mut depth = 0usize;
    for t in tokenize(sql).into_iter().filter(|t| !t.is_trivia()) {
        if t.is_punct(sql, '(') {
            if depth == 0 {
                top.push(t);
            }
            depth += 1;
        } else if t.is_punct(sql, ')') {
            depth = depth.saturating_sub(1);
        } else if depth == 0 {
            top.push(t);
        }
    }
    let select = top.iter().position(|t| t.is_word(sql, "select"))?;
    let mut i = select + 1;
    while top
        .get(i)
        .is_some_and(|t| t.is_word(sql, "distinct") || t.is_word(sql, "all"))
    {
        i += 1;
        // Postgres `DISTINCT ON (…)`.
        if top.get(i).is_some_and(|t| t.is_word(sql, "on")) {
            i += 2;
        }
    }
    let list_end = top[i..]
        .iter()
        .position(|t| {
            t.kind == TokenKind::Word
                && [
                    "from", "into", "where", "group", "order", "limit", "union", "window",
                ]
                .iter()
                .any(|k| t.is_word(sql, k))
        })
        .map_or(top.len(), |p| i + p);
    let items = top[i..list_end]
        .split(|t| t.is_punct(sql, ','))
        .map(|item| parse_item(sql, item))
        .collect();

    let mut from = Vec::new();
    let mut opaque = false;
    if top.get(list_end).is_some_and(|t| t.is_word(sql, "from")) {
        let mut expect = true;
        for t in &top[list_end + 1..] {
            let word = |k: &str| t.is_word(sql, k);
            if [
                "where",
                "group",
                "having",
                "order",
                "limit",
                "union",
                "intersect",
                "except",
                "window",
                "offset",
                "fetch",
                "for",
            ]
            .iter()
            .any(|k| word(k))
            {
                break;
            }
            if expect {
                if word("lateral") || word("only") {
                    opaque |= word("lateral");
                    continue;
                }
                if t.is_ident() && !(t.kind == TokenKind::Word && is_reserved(t.text(sql))) {
                    from.push(t.start);
                } else {
                    opaque = true;
                }
                expect = false;
            } else if word("join") || t.is_punct(sql, ',') {
                expect = true;
            } else if t.is_punct(sql, '(') && !from.is_empty() {
                // `name(` right after an entry: a function call, not a table.
                let prev = top.iter().rposition(|p| p.end <= t.start);
                if prev.is_some_and(|p| top[p].start == *from.last().unwrap_or(&usize::MAX)) {
                    from.pop();
                    opaque = true;
                }
            }
        }
    }
    Some(MainQuery {
        items,
        from,
        opaque,
    })
}

fn parse_item(sql: &str, tokens: &[Token]) -> Item {
    let ident =
        |t: &Token| t.is_ident() && !(t.kind == TokenKind::Word && is_reserved(t.text(sql)));
    let dot = |t: &Token| t.is_punct(sql, '.');
    let star = |t: &Token| t.is_punct(sql, '*');
    // Leave off an alias: `x AS a` or `x a`.
    let body = match tokens {
        [rest @ .., as_kw, alias] if as_kw.is_word(sql, "as") && alias.is_ident() => rest,
        [rest @ .., last, alias] if ident(alias) && !dot(last) => &tokens[..rest.len() + 1],
        _ => tokens,
    };
    match body {
        [s] if star(s) => Item::Star(None),
        [q, d, s] if ident(q) && dot(d) && star(s) => Item::Star(Some(unquote(q.text(sql)))),
        [c] if ident(c) => column(None, c, sql),
        [q, d, c] if ident(q) && dot(d) && ident(c) => column(Some(q), c, sql),
        // `schema.table.column`
        [_, d1, q, d2, c] if dot(d1) && ident(q) && dot(d2) && ident(c) => column(Some(q), c, sql),
        _ => Item::Expr,
    }
}

fn column(qualifier: Option<&Token>, name: &Token, sql: &str) -> Item {
    Item::Column {
        qualifier: qualifier.map(|q| unquote(q.text(sql))),
        name: unquote(name.text(sql)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::sample;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// `table.column` per result column, `-` for unknown.
    fn origins(sql: &str, cols: &[&str]) -> Vec<String> {
        column_facts(sql, &names(cols), &sample())
            .into_iter()
            .map(|f| f.map_or("-".into(), |f| format!("{}.{}", f.table, f.column)))
            .collect()
    }

    #[test]
    fn single_table() {
        assert_eq!(
            origins(
                "select id, total, 1 as one from orders",
                &["id", "total", "one"]
            ),
            ["orders.id", "orders.total", "-"]
        );
        let f = column_facts(
            "select * from orders",
            &names(&["id", "user_id", "total"]),
            &sample(),
        );
        let f: Vec<_> = f.into_iter().map(Option::unwrap).collect();
        assert!(f[0].primary_key && f[0].numeric && f[0].references.is_none());
        assert_eq!(
            f[1].references.as_ref().map(ToString::to_string).as_deref(),
            Some("users.id")
        );
        assert_eq!(f[1].references.as_ref().unwrap().schema, "app");
        assert!(!f[2].primary_key && !f[2].numeric);
    }

    fn tags_of(schema: &SchemaIndex, table: &str, column: &str) -> Vec<(TagKind, String)> {
        column_facts(
            &format!("select {column} from {table}"),
            &names(&[column]),
            schema,
        )
        .remove(0)
        .unwrap()
        .tags()
        .into_iter()
        .map(|t| (t.kind, t.text))
        .collect()
    }

    fn rich() -> SchemaIndex {
        use tangul_core::{EnumInfo, IndexInfo};
        let mut info = sample().info().clone();
        let at = |info: &tangul_core::SchemaInfo, t: &str, n: &str| {
            info.columns
                .iter()
                .position(|c| c.table == t && c.name == n)
                .unwrap()
        };
        let email = at(&info, "users", "email");
        let name = at(&info, "users", "name");
        let total = at(&info, "orders", "total");
        let uid = at(&info, "orders", "user_id");
        info.columns[email].default = Some("''::text".into());
        info.columns[name].enum_type = Some("app.status".into());
        info.columns[total].generated = Some("a * b".into());
        info.columns[total].comment = Some("with tax".into());
        info.columns[uid].identity = true;
        info.enums.push(EnumInfo {
            schema: "app".into(),
            name: "status".into(),
            values: vec!["a".into(), "b".into(), "c".into()],
            comment: None,
        });
        let index = |t: &str, name: &str, cols: &[&str], unique: bool| IndexInfo {
            schema: "app".into(),
            table: t.into(),
            name: name.into(),
            columns: cols.iter().map(|c| c.to_string()).collect(),
            unique,
            ..Default::default()
        };
        info.indexes = vec![
            IndexInfo {
                primary: true,
                ..index("users", "users_pk", &["id"], true)
            },
            index("users", "users_email_key", &["email"], true),
            index("orders", "orders_user_total", &["user_id", "total"], true),
            index("orders", "orders_total_idx", &["total"], false),
            index("orders", "orders_total_2", &["total", "id"], false),
            IndexInfo {
                predicate: Some("total > 0".into()),
                ..index("orders", "orders_total_3", &["total"], true)
            },
            index("orders", "wide", &["id", "user_id", "total"], true),
        ];
        SchemaIndex::new(info, sample().dialect(), Some("public".into()))
    }

    #[test]
    fn tags_follow_the_schema() {
        use TagKind::*;
        let s = rich();
        // Primary keys are marked elsewhere, not tagged unique.
        assert_eq!(tags_of(&s, "users", "id"), [(Type, "integer".into())]);
        assert_eq!(
            tags_of(&s, "users", "email"),
            [
                (Type, "text".into()),
                (Nullable, "null".into()),
                (Unique, "unique".into()),
                (Default, "''::text".into())
            ]
        );
        assert_eq!(
            tags_of(&s, "users", "name"),
            // In the type's place.
            [(Enum, "status ·3".into()), (Nullable, "null".into())]
        );
        // Composite unique: the smallest one names its partner, a larger one counts.
        assert_eq!(
            tags_of(&s, "orders", "user_id"),
            [
                (Type, "integer".into()),
                (Nullable, "null".into()),
                (Unique, "unique +total".into()),
                (Auto, "auto".into())
            ]
        );
        assert_eq!(
            tags_of(&s, "orders", "total"),
            [
                (Type, "text".into()),
                (Nullable, "null".into()),
                (Unique, "unique +user_id".into()),
                (Computed, "computed".into()),
                (Index, "3 indexes".into())
            ]
        );
        let f = column_facts("select total from orders", &names(&["total"]), &s);
        assert_eq!(f[0].as_ref().unwrap().comment.as_deref(), Some("with tax"));
    }

    #[test]
    fn tags_count_and_cut() {
        let mut f = column_facts("select id from orders", &names(&["id"]), &sample())
            .remove(0)
            .unwrap();
        f.data_type = "character varying(255) with a very long tail".into();
        f.default = Some("nextval('orders_id_seq'::regclass)".into());
        f.unique = Unique::With(vec!["a".into(), "b".into()]);
        f.indexes = vec!["only_one".into()];
        let t: Vec<_> = f.tags().into_iter().map(|t| t.text).collect();
        assert_eq!(
            t,
            [
                "character varying(255)…",
                "unique +2",
                "nextval('orders_id_seq'…",
                "only_one"
            ]
        );
    }

    #[test]
    fn joins_by_position() {
        let sql = "select * from orders o join users u on u.id = o.user_id";
        assert_eq!(
            origins(sql, &["id", "user_id", "total", "id", "email", "name"]),
            [
                "orders.id",
                "orders.user_id",
                "orders.total",
                "users.id",
                "users.email",
                "users.name"
            ]
        );
        assert_eq!(
            origins(
                "select o.id, u.id as uid, name, count(*) from orders o, users u group by 1",
                &["id", "uid", "name", "count"]
            ),
            ["orders.id", "users.id", "users.name", "-"]
        );
        assert_eq!(
            origins(
                "select u.*, o.total from app.users u join orders o on true",
                &["id", "email", "name", "total"]
            ),
            ["users.id", "users.email", "users.name", "orders.total"]
        );
        // `id` is in both tables.
        assert_eq!(origins("select id from orders, users", &["id"]), ["-"]);
    }

    #[test]
    fn unknown_sources() {
        // A subquery in FROM could have any column.
        assert_eq!(
            origins("select id from (select 1 as id) x, orders", &["id"]),
            ["-"]
        );
        assert_eq!(
            origins(
                "with t as (select 1) select total, email from orders, t",
                &["total", "email"]
            ),
            ["-", "-"]
        );
        assert_eq!(
            origins("select * from generate_series(1, 3)", &["generate_series"]),
            ["-"]
        );
        // A mismatch in count (the schema is stale) falls back to names.
        assert_eq!(
            origins("select * from orders", &["id", "total"]),
            ["orders.id", "orders.total"]
        );
        assert_eq!(origins("", &["x"]), ["-"]);
    }
}
