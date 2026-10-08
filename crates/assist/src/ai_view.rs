//! What AI tools see of a result: per database, columns can be redacted (their
//! values replaced by [`REDACTED`]) or hidden (left out), by the rules in its own
//! settings ([`tangul_core::ai_view::AiColumns`]).
//!
//! A result column counts as every table column it is computed from: renamed
//! (`email AS e`), in an expression (`upper(email)`, `count(email)`), through `*`,
//! CTEs and subqueries in `FROM`, or `UNION` branches. It gets the strictest mode
//! of those. Where that can't be told (a `*` over a CTE, a column count that
//! doesn't add up), every column gets the strictest mode used anywhere in the
//! statement. No full parser: the rules follow tokens and parentheses, and err on
//! the strict side (a visible column named like a redacted one counts as it).
//!
//! While any rule applies, a statement may only read the database's own tables
//! (not catalogs, not functions in `FROM`), and with `block_queries` it may use
//! redacted (and hidden) columns only in its select list.

use std::collections::HashMap;

use tangul_core::ai_view::{AiColumns, BlockQueries, ListKind};
use tangul_core::{ColumnInfo, ResultSet, RowBatch};

use crate::analyze::{TableRef, analyze, is_reserved, significant_with};
use crate::guard;
use crate::lexer::{Lexing, Token, TokenKind, tokenize, unquote};
use crate::schema::SchemaIndex;

/// What a redacted value becomes.
pub const REDACTED: &str = "⟦redacted⟧";

/// The comment that shows a result as AI tools see it, and the one below it.
pub const MARK: &str = "-- ai view";
pub const MARK_NOTE: &str = "-- remove the line above to see the full result";

/// What AI tools see of a column, the strictest last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mode {
    Visible,
    Redacted,
    Hidden,
}

impl Mode {
    pub fn word(self) -> &'static str {
        match self {
            Mode::Visible => "visible",
            Mode::Redacted => "redacted",
            Mode::Hidden => "hidden",
        }
    }
}

/// `schema.table.column`, `table.column` or `column`.
#[derive(Debug, Clone)]
struct ColumnPattern {
    schema: Option<String>,
    table: Option<String>,
    column: String,
}

impl ColumnPattern {
    fn new(text: &str) -> Self {
        let parts: Vec<&str> = text.trim().split('.').map(str::trim).collect();
        let n = parts.len();
        let part = |back: usize| (n > back).then(|| parts[n - 1 - back].to_lowercase());
        Self {
            schema: part(2),
            table: part(1),
            column: part(0).unwrap_or_default(),
        }
    }

    fn matches(&self, schema: &str, table: &str, column: &str) -> bool {
        glob(&self.column, column)
            && self.table.as_deref().is_none_or(|p| glob(p, table))
            && self.schema.as_deref().is_none_or(|p| glob(p, schema))
    }
}

/// `schema.table` or `table`.
#[derive(Debug, Clone)]
struct TablePattern {
    schema: Option<String>,
    table: String,
}

impl TablePattern {
    fn new(text: &str) -> Self {
        match text.trim().rsplit_once('.') {
            Some((schema, table)) => Self {
                schema: Some(schema.trim().to_lowercase()),
                table: table.trim().to_lowercase(),
            },
            None => Self {
                schema: None,
                table: text.trim().to_lowercase(),
            },
        }
    }

    fn matches(&self, schema: &str, table: &str) -> bool {
        glob(&self.table, table) && self.schema.as_deref().is_none_or(|p| glob(p, schema))
    }
}

/// `pattern` (lower case, `*` for anything) against `text`, ignoring case.
fn glob(pattern: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    let parts: Vec<&str> = pattern.split('*').collect();
    let [first, middle @ .., last] = parts.as_slice() else {
        return pattern == text;
    };
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// A database's rules, ready to match.
#[derive(Debug, Clone)]
pub struct Rules {
    kind: ListKind,
    redact_columns: Vec<ColumnPattern>,
    redact_tables: Vec<TablePattern>,
    hide_columns: Vec<ColumnPattern>,
    hide_tables: Vec<TablePattern>,
    show_columns: Vec<ColumnPattern>,
    show_tables: Vec<TablePattern>,
    block: BlockQueries,
    open: bool,
}

impl Rules {
    pub fn new(settings: &AiColumns) -> Self {
        let columns = |l: &[String]| l.iter().map(|p| ColumnPattern::new(p)).collect();
        let tables = |l: &[String]| l.iter().map(|p| TablePattern::new(p)).collect();
        Self {
            kind: settings.kind,
            redact_columns: columns(&settings.redact_columns),
            redact_tables: tables(&settings.redact_tables),
            hide_columns: columns(&settings.hide_columns),
            hide_tables: tables(&settings.hide_tables),
            show_columns: columns(&settings.show_columns),
            show_tables: tables(&settings.show_tables),
            block: settings.block_queries,
            open: settings.is_open(),
        }
    }

    /// Whether every column is visible.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// What AI tools see of `schema.table.column`.
    pub fn mode(&self, schema: &str, table: &str, column: &str) -> Mode {
        let column_hit = |l: &[ColumnPattern]| l.iter().any(|p| p.matches(schema, table, column));
        let table_hit = |l: &[TablePattern]| l.iter().any(|p| p.matches(schema, table));
        if column_hit(&self.hide_columns) || table_hit(&self.hide_tables) {
            return Mode::Hidden;
        }
        if self.kind == ListKind::Whitelist
            && !column_hit(&self.show_columns)
            && !table_hit(&self.show_tables)
        {
            return Mode::Hidden;
        }
        if column_hit(&self.redact_columns) || table_hit(&self.redact_tables) {
            return Mode::Redacted;
        }
        Mode::Visible
    }

    /// Whether `block_queries` keeps columns in `mode` out of conditions.
    fn blocks(&self, mode: Mode) -> bool {
        match self.block {
            BlockQueries::None => false,
            BlockQueries::Redacted => mode == Mode::Redacted,
            BlockQueries::RedactedAndHidden => mode >= Mode::Redacted,
        }
    }
}

/// What AI tools see of one statement's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Per result column, when the statement says it exactly.
    columns: Option<Vec<Mode>>,
    /// Otherwise for every column: the strictest mode the statement uses.
    all: Mode,
}

impl Plan {
    /// Everything visible.
    pub fn open() -> Self {
        Self {
            columns: None,
            all: Mode::Visible,
        }
    }

    /// The mode of each of a result's `count` columns.
    pub fn modes(&self, count: usize) -> Vec<Mode> {
        match &self.columns {
            Some(columns) if columns.len() == count => columns.clone(),
            _ => vec![self.all; count],
        }
    }

    /// `result` as AI tools see it, and a line for the model saying what changed
    /// (`None`: nothing did).
    pub fn apply(&self, result: &ResultSet) -> (ResultSet, Option<String>) {
        let modes = self.modes(result.columns().len());
        if modes.iter().all(|m| *m == Mode::Visible) {
            return (result.clone(), None);
        }
        let mut out = ResultSet::new(columns(&modes, result.columns()));
        let mut batch = RowBatch::with_capacity(result.row_count() * modes.len());
        for row in 0..result.row_count() {
            push_row(&modes, &mut batch, |c| result.cell(row, c));
        }
        out.append(batch);
        let named = |mode: Mode| -> Vec<&str> {
            (0..modes.len())
                .filter(|&c| modes[c] == mode)
                .map(|c| result.columns()[c].name.as_str())
                .collect()
        };
        let mut note = Vec::new();
        let redacted = named(Mode::Redacted);
        if !redacted.is_empty() {
            note.push(format!(
                "Values of {} are redacted for AI tools ({REDACTED}); the user sees them.",
                redacted.join(", ")
            ));
        }
        let hidden = named(Mode::Hidden);
        if !hidden.is_empty() {
            note.push(format!(
                "Left out, hidden from AI tools: {}.",
                hidden.join(", ")
            ));
        }
        (out, Some(note.join(" ")))
    }
}

/// What AI tools may see of `sql`'s result on a database with `rules`, or why it
/// can't run for them. `lexing`: the database's lexical rules.
pub fn plan(
    sql: &str,
    schema: &SchemaIndex,
    rules: &Rules,
    lexing: Lexing,
) -> Result<Plan, String> {
    if rules.is_open() {
        return Ok(Plan::open());
    }
    // Comments blanked, as the server gets it.
    let text = guard::read_text(sql, lexing);
    let tokens = significant_with(&text, lexing);
    let s = Scan::new(&text, &tokens);

    // The relations it reads: tables of the database, or CTEs.
    let ctes = s.ctes();
    let refs = analyze(&text).tables;
    let mut handles: Vec<(String, Option<usize>)> = Vec::new();
    for r in &refs {
        let cte = r.schema.is_none() && ctes.names.iter().any(|n| n.eq_ignore_ascii_case(&r.name));
        let table = if cte {
            None
        } else {
            Some(schema.resolve(r.schema.as_deref(), &r.name).ok_or_else(|| {
                format!(
                    "{}: not a table of this database; while columns are redacted or hidden \
                     for AI tools, they only read the database's own tables",
                    qualified(r)
                )
            })?)
        };
        handles.push((r.handle().to_lowercase(), table));
    }
    if let Some(function) = s.function_in_from() {
        return Err(format!(
            "{function}(…) in FROM: while columns are redacted or hidden for AI tools, they \
             only read the database's own tables"
        ));
    }
    let tables: Vec<usize> = handles.iter().filter_map(|(_, t)| *t).collect();
    let column_mode = |ix: usize, column: &str| {
        let t = schema.table(ix);
        rules.mode(&t.schema, &t.name, column)
    };
    let table_modes = |ix: usize| -> Vec<Mode> {
        schema
            .columns(ix)
            .iter()
            .map(|c| column_mode(ix, &c.name))
            .collect()
    };
    let strictest = |modes: &[Mode]| modes.iter().copied().max().unwrap_or(Mode::Visible);
    // By column name, the strictest of the tables read.
    let mut by_name: HashMap<String, Mode> = HashMap::new();
    for &ix in &tables {
        for c in schema.columns(ix) {
            let mode = column_mode(ix, &c.name);
            let entry = by_name.entry(c.name.to_lowercase()).or_insert(mode);
            *entry = (*entry).max(mode);
        }
    }
    let every_table = tables
        .iter()
        .map(|&ix| strictest(&table_modes(ix)))
        .max()
        .unwrap_or(Mode::Visible);
    let handle = |name: &str| handles.iter().find(|(h, _)| h == name).map(|(_, t)| *t);

    // Every use of a column (or a whole row), with what it reveals.
    let in_ref = |t: &Token| refs.iter().any(|r| t.start >= r.start && t.start < r.end);
    let mut uses: Vec<(usize, String, Mode)> = Vec::new();
    let mut cte_rows: Vec<usize> = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        if !t.is_ident() || in_ref(t) || s.is(i + 1, '(') || s.is(i + 1, '.') {
            continue;
        }
        let name = unquote(t.text(&text)).to_lowercase();
        let known = by_name.get(&name).copied().unwrap_or(Mode::Visible);
        let mode = if i >= 2 && s.is(i - 1, '.') {
            let qualifier = unquote(tokens[i - 2].text(&text)).to_lowercase();
            match handle(&qualifier) {
                Some(Some(ix))
                    if schema
                        .columns(ix)
                        .iter()
                        .any(|c| c.name.eq_ignore_ascii_case(&name)) =>
                {
                    column_mode(ix, &name)
                }
                _ => known,
            }
        } else {
            // A table or alias on its own is its whole row (`row_to_json(u)`).
            let row = match handle(&name) {
                Some(Some(ix)) => strictest(&table_modes(ix)),
                // A CTE's row: what the CTE passes on, known below.
                Some(None) => {
                    cte_rows.push(uses.len());
                    Mode::Visible
                }
                None => Mode::Visible,
            };
            known.max(row)
        };
        uses.push((i, name, mode));
    }
    // `*` anywhere but as a top-level select item reveals what any table holds.
    let stars: Vec<usize> = (0..tokens.len()).filter(|&i| s.is_select_star(i)).collect();

    // What CTEs and subqueries in FROM pass on: whatever they use.
    let derived = s.derived(&ctes);
    let within = |i: usize, ranges: &[(usize, usize)]| ranges.iter().any(|&(a, b)| i > a && i < b);
    let mut passed_on = Mode::Visible;
    for (i, _, mode) in &uses {
        if within(*i, &derived) {
            passed_on = passed_on.max(*mode);
        }
    }
    if stars.iter().any(|&i| within(i, &derived)) {
        passed_on = passed_on.max(every_table);
    }
    for &u in &cte_rows {
        uses[u].2 = uses[u].2.max(passed_on);
    }

    // The top-level select lists, one per UNION branch.
    let branches = s.branches(ctes.main);
    let items: Vec<(usize, usize)> = branches.iter().flat_map(|b| b.items.clone()).collect();
    if rules.block != BlockQueries::None {
        for (i, name, mode) in &uses {
            if rules.blocks(*mode) && !within_item(*i, &items) {
                return Err(format!(
                    "{name} is {} for AI tools: it can only be selected, not used to filter, \
                     join, sort or group (block_queries in the database's settings)",
                    mode.word()
                ));
            }
        }
    }
    // Each branch's columns, where every item's width is known; and what the
    // statement reveals at most.
    let mut all = uses
        .iter()
        .map(|(_, _, m)| *m)
        .chain([passed_on])
        .max()
        .unwrap_or(Mode::Visible);
    let star_items: Vec<usize> = items
        .iter()
        .filter(|&&(a, b)| s.star_item(a, b).is_some())
        .map(|&(_, b)| b - 1)
        .collect();
    for &i in &stars {
        if !within(i, &derived) && !star_items.contains(&i) {
            all = all.max(every_table);
        }
    }
    let mut per_branch: Vec<Option<Vec<Mode>>> = Vec::new();
    for branch in &branches {
        let mut cols: Option<Vec<Mode>> = Some(Vec::new());
        for &(a, b) in &branch.items {
            let mode = if let Some(qualifier) = s.star_item(a, b) {
                // A star: one column per column of its relations, a CTE or subquery
                // passing on what it uses.
                let relations: Vec<Option<usize>> = match qualifier {
                    Some(q) => vec![handle(&q).flatten()],
                    None => branch
                        .from
                        .iter()
                        .map(|r| handle(&r.handle().to_lowercase()).flatten())
                        .collect(),
                };
                for r in &relations {
                    all = all.max(r.map_or(passed_on, |ix| strictest(&table_modes(ix))));
                }
                match (
                    &mut cols,
                    branch.opaque || relations.iter().any(Option::is_none),
                ) {
                    (Some(cols), false) => {
                        for ix in relations.into_iter().flatten() {
                            cols.extend(table_modes(ix).into_iter().map(|m| m.max(passed_on)));
                        }
                    }
                    _ => cols = None,
                }
                continue;
            } else {
                let mut mode = passed_on;
                for (i, _, m) in &uses {
                    if *i >= a && *i < b {
                        mode = mode.max(*m);
                    }
                }
                if stars.iter().any(|&i| i >= a && i < b) {
                    mode = mode.max(every_table);
                }
                mode
            };
            if let Some(cols) = cols.as_mut() {
                cols.push(mode);
            }
        }
        per_branch.push(cols);
    }
    // Branches line up column by column; the strictest wins.
    let mut columns: Option<Vec<Mode>> = None;
    for (n, cols) in per_branch.into_iter().enumerate() {
        columns = match (n, columns, cols) {
            (0, _, cols) => cols,
            (_, Some(a), Some(b)) if a.len() == b.len() => {
                Some(a.into_iter().zip(b).map(|(x, y)| x.max(y)).collect())
            }
            _ => None,
        };
        if columns.is_none() {
            break;
        }
    }
    Ok(Plan { columns, all })
}

/// The columns of a result whose columns are in `modes`, as AI tools see them.
pub fn columns(modes: &[Mode], columns: &[ColumnInfo]) -> Vec<ColumnInfo> {
    columns
        .iter()
        .zip(modes)
        .filter(|(_, m)| **m != Mode::Hidden)
        .map(|(c, _)| c.clone())
        .collect()
}

/// A batch of a result whose columns are in `modes`, as AI tools see it.
pub fn batch(modes: &[Mode], rows: &RowBatch) -> RowBatch {
    let mut out = RowBatch::with_capacity(rows.rows() * modes.len());
    for row in 0..rows.rows() {
        push_row(modes, &mut out, |c| rows.cell(modes.len(), row, c));
    }
    out
}

fn push_row<'a>(modes: &[Mode], out: &mut RowBatch, cell: impl Fn(usize) -> Option<&'a str>) {
    for (c, mode) in modes.iter().enumerate() {
        match mode {
            Mode::Hidden => {}
            Mode::Redacted => out.push_cell(cell(c).map(|_| REDACTED)),
            Mode::Visible => out.push_cell(cell(c)),
        }
    }
    out.finish_row();
}

fn within_item(i: usize, items: &[(usize, usize)]) -> bool {
    items.iter().any(|&(a, b)| i >= a && i < b)
}

fn qualified(r: &TableRef) -> String {
    match &r.schema {
        Some(s) => format!("{s}.{}", r.name),
        None => r.name.clone(),
    }
}

/// The CTEs of a statement: their names and bodies, and where the main query starts.
struct Ctes {
    names: Vec<String>,
    bodies: Vec<(usize, usize)>,
    main: usize,
}

/// One top-level `SELECT` (a `UNION` branch).
struct Branch {
    /// Token ranges of its select items.
    items: Vec<(usize, usize)>,
    /// The relations in its own FROM list.
    from: Vec<TableRef>,
    /// Its FROM list has a subquery, so `*` can't be counted.
    opaque: bool,
}

/// The significant tokens of a statement, with matching parentheses.
struct Scan<'a> {
    text: &'a str,
    tokens: &'a [Token],
    /// Per token: its parenthesis depth (a `(` has the depth outside it).
    depth: Vec<usize>,
    /// Per `(`: the index of its `)` (or the end).
    close: HashMap<usize, usize>,
}

impl<'a> Scan<'a> {
    fn new(text: &'a str, tokens: &'a [Token]) -> Self {
        let mut depth = Vec::with_capacity(tokens.len());
        let mut close = HashMap::new();
        let mut open: Vec<usize> = Vec::new();
        for (i, t) in tokens.iter().enumerate() {
            if t.is_punct(text, ')') {
                if let Some(o) = open.pop() {
                    close.insert(o, i);
                }
                depth.push(open.len());
            } else {
                depth.push(open.len());
                if t.is_punct(text, '(') {
                    open.push(i);
                }
            }
        }
        for o in open {
            close.insert(o, tokens.len());
        }
        Self {
            text,
            tokens,
            depth,
            close,
        }
    }

    fn is(&self, i: usize, c: char) -> bool {
        self.tokens.get(i).is_some_and(|t| t.is_punct(self.text, c))
    }

    fn word(&self, i: usize, w: &str) -> bool {
        self.tokens.get(i).is_some_and(|t| t.is_word(self.text, w))
    }

    fn word_in(&self, i: usize, words: &[&str]) -> bool {
        words.iter().any(|w| self.word(i, w))
    }

    /// `WITH [RECURSIVE] name [(cols)] AS [[NOT] MATERIALIZED] (body), …`.
    fn ctes(&self) -> Ctes {
        let mut ctes = Ctes {
            names: Vec::new(),
            bodies: Vec::new(),
            main: 0,
        };
        if !self.word(0, "with") {
            return ctes;
        }
        let mut i = 1;
        if self.word(i, "recursive") {
            i += 1;
        }
        while let Some(name) = self.tokens.get(i).filter(|t| t.is_ident()) {
            ctes.names.push(unquote(name.text(self.text)));
            i += 1;
            if self.is(i, '(') {
                i = self.close[&i] + 1;
            }
            if self.word(i, "as") {
                i += 1;
            }
            if self.word(i, "not") {
                i += 1;
            }
            if self.word(i, "materialized") {
                i += 1;
            }
            if !self.is(i, '(') {
                break;
            }
            let end = self.close[&i];
            ctes.bodies.push((i, end));
            i = end + 1;
            if self.is(i, ',') {
                i += 1;
            } else {
                break;
            }
        }
        ctes.main = i;
        ctes
    }

    /// Token ranges (exclusive) of CTE bodies and subqueries in FROM lists.
    fn derived(&self, ctes: &Ctes) -> Vec<(usize, usize)> {
        let mut out = ctes.bodies.clone();
        let mut in_from = false;
        let mut stack = Vec::new();
        for (i, t) in self.tokens.iter().enumerate() {
            if t.is_punct(self.text, '(') {
                let after = i > 0
                    && (self.word_in(i - 1, &["from", "join", "lateral"])
                        || (self.is(i - 1, ',') && in_from));
                if after {
                    out.push((i, self.close[&i]));
                }
                stack.push(in_from);
                in_from = false;
            } else if t.is_punct(self.text, ')') {
                in_from = stack.pop().unwrap_or(false);
            } else if self.word_in(i, &["from", "join"]) {
                in_from = true;
            } else if t.kind == TokenKind::Word && is_reserved(t.text(self.text)) {
                in_from = self.word(i, "lateral") && in_from;
            }
        }
        out
    }

    /// The first function called where a table belongs (`FROM generate_series(…)`).
    fn function_in_from(&self) -> Option<String> {
        let mut in_from = false;
        let mut stack = Vec::new();
        for (i, t) in self.tokens.iter().enumerate() {
            if t.is_punct(self.text, '(') {
                stack.push(in_from);
                in_from = false;
                continue;
            }
            if t.is_punct(self.text, ')') {
                in_from = stack.pop().unwrap_or(false);
                continue;
            }
            if self.word_in(i, &["from", "join"]) {
                in_from = true;
                continue;
            }
            let lateral = self.word(i, "lateral");
            if t.kind == TokenKind::Word && is_reserved(t.text(self.text)) && !lateral {
                in_from = false;
                continue;
            }
            // A name (maybe `schema.name`) right after FROM, JOIN, LATERAL or a comma
            // in a FROM list, called like a function.
            let starts = i > 0
                && (self.word_in(i - 1, &["from", "join", "lateral"])
                    || (self.is(i - 1, ',') && in_from));
            if starts && t.is_ident() {
                let mut j = i;
                if self.is(j + 1, '.') && self.tokens.get(j + 2).is_some_and(Token::is_ident) {
                    j += 2;
                }
                if self.is(j + 1, '(') {
                    return Some(self.text[t.start..self.tokens[j].end].to_string());
                }
            }
        }
        None
    }

    /// Whether token `i` is a `*` that selects columns (not a multiplication or
    /// `count(*)`).
    fn is_select_star(&self, i: usize) -> bool {
        if !self.is(i, '*') || i == 0 {
            return false;
        }
        let before = self.word_in(i - 1, &["select", "distinct", "all"])
            || self.is(i - 1, ',')
            || (self.is(i - 1, '.') && i >= 2 && self.tokens[i - 2].is_ident());
        let after = i + 1 >= self.tokens.len()
            || self.is(i + 1, ',')
            || self.is(i + 1, ')')
            || self.word_in(
                i + 1,
                &["from", "into", "where", "union", "except", "intersect"],
            );
        before && after
    }

    /// `*` or `q.*` as a whole select item: `Some(qualifier)`.
    fn star_item(&self, a: usize, b: usize) -> Option<Option<String>> {
        match b - a {
            1 if self.is(a, '*') => Some(None),
            3 if self.tokens[a].is_ident() && self.is(a + 1, '.') && self.is(a + 2, '*') => {
                Some(Some(unquote(self.tokens[a].text(self.text)).to_lowercase()))
            }
            _ => None,
        }
    }

    /// The top-level `SELECT`s from token `start` on, one per set-operation branch.
    /// None when the query isn't a plain `SELECT` there (a parenthesised branch).
    fn branches(&self, start: usize) -> Vec<Branch> {
        const ENDS: &[&str] = &[
            "from",
            "into",
            "where",
            "group",
            "having",
            "order",
            "limit",
            "offset",
            "fetch",
            "window",
            "union",
            "intersect",
            "except",
            "for",
        ];
        let mut out = Vec::new();
        let mut i = start;
        let n = self.tokens.len();
        while i < n {
            if !self.word(i, "select") || self.depth[i] != 0 {
                return Vec::new();
            }
            i += 1;
            if self.word(i, "all") {
                i += 1;
            } else if self.word(i, "distinct") {
                i += 1;
                if self.word(i, "on") && self.is(i + 1, '(') {
                    i = self.close[&(i + 1)] + 1;
                }
            }
            let mut items = Vec::new();
            let mut item_start = i;
            while i < n && !(self.depth[i] == 0 && self.word_in(i, ENDS)) {
                if self.depth[i] == 0 && self.is(i, ',') {
                    items.push((item_start, i));
                    item_start = i + 1;
                }
                if self.is(i, '(') {
                    i = self.close[&i];
                }
                i += 1;
            }
            if i > item_start {
                items.push((item_start, i.min(n)));
            }
            // Its own FROM list: up to the next top-level clause or branch.
            let from_start = i;
            let mut opaque = false;
            while i < n
                && !(self.depth[i] == 0 && self.word_in(i, &["union", "intersect", "except"]))
            {
                if self.is(i, '(') {
                    if i > 0
                        && (self.word_in(i - 1, &["from", "join", "lateral"])
                            || self.is(i - 1, ','))
                    {
                        opaque = true;
                    }
                    i = self.close[&i];
                }
                i += 1;
            }
            let (a, b) = (
                self.tokens
                    .get(from_start)
                    .map_or(self.text.len(), |t| t.start),
                self.tokens.get(i).map_or(self.text.len(), |t| t.start),
            );
            let from = analyze(&self.text[..b])
                .tables
                .into_iter()
                .filter(|r| r.start >= a)
                .filter(|r| self.depth_at(r.start) == 0)
                .collect();
            out.push(Branch {
                items,
                from,
                opaque,
            });
            if i < n {
                // UNION [ALL | DISTINCT]
                i += 1;
                if self.word_in(i, &["all", "distinct"]) {
                    i += 1;
                }
            }
        }
        out
    }

    /// The depth of the token starting at byte `at`.
    fn depth_at(&self, at: usize) -> usize {
        self.tokens
            .iter()
            .position(|t| t.start == at)
            .map_or(usize::MAX, |i| self.depth[i])
    }
}

/// Whether `sql` asks to be shown as AI tools see it: a `-- ai view` comment line
/// (any case, any spacing).
pub fn is_marked(sql: &str) -> bool {
    tokenize(sql).iter().any(|t| {
        t.kind == TokenKind::Comment && {
            let text = t.text(sql).trim_start_matches('-').trim_start_matches('#');
            let words: Vec<&str> = text.split_whitespace().collect();
            words.len() == 2
                && words[0].eq_ignore_ascii_case("ai")
                && words[1].eq_ignore_ascii_case("view")
        }
    })
}

/// `sql` under the comment that shows it as AI tools see it, and the note below.
pub fn mark(sql: &str) -> String {
    if is_marked(sql) {
        return sql.to_string();
    }
    format!("{MARK}\n{MARK_NOTE}\n{sql}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::dialect_for;
    use tangul_core::{ColumnMeta, DriverKind, SchemaInfo, TableInfo, TableKind};

    fn shop() -> SchemaIndex {
        let table = |name: &str| TableInfo {
            schema: "public".into(),
            name: name.into(),
            kind: TableKind::Table,
            comment: None,
            row_estimate: None,
            size_bytes: None,
        };
        let col = |table: &str, name: &str| ColumnMeta {
            schema: "public".into(),
            table: table.into(),
            name: name.into(),
            data_type: "text".into(),
            nullable: true,
            ..Default::default()
        };
        SchemaIndex::new(
            SchemaInfo {
                tables: vec![table("users"), table("orders"), table("audit_log")],
                columns: vec![
                    col("users", "id"),
                    col("users", "name"),
                    col("users", "email"),
                    col("users", "ssn"),
                    col("orders", "id"),
                    col("orders", "user_id"),
                    col("orders", "adr_street"),
                    col("orders", "adr_city"),
                    col("orders", "total"),
                    col("audit_log", "id"),
                    col("audit_log", "what"),
                ],
                ..Default::default()
            },
            dialect_for(DriverKind::Postgres),
            Some("public".into()),
        )
    }

    fn rules(json: &str) -> Rules {
        Rules::new(&serde_json::from_str(json).unwrap())
    }

    /// The usual rules: email and the address redacted, ssn and the audit log hidden.
    fn usual() -> Rules {
        rules(
            r#"{"redact_columns": ["email", "orders.adr*"], "hide_columns": ["ssn"],
                "hide_tables": ["audit_*"]}"#,
        )
    }

    fn modes(sql: &str, rules: &Rules, count: usize) -> Vec<Mode> {
        plan(sql, &shop(), rules, Lexing::default())
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .modes(count)
    }

    use Mode::{Hidden as H, Redacted as R, Visible as V};

    #[test]
    fn patterns_ignore_case_and_know_one_wildcard() {
        assert!(glob("adr*", "ADR_street"));
        assert!(glob("*token*", "api_token_hash"));
        assert!(glob("a*c*e", "abcde"));
        assert!(!glob("a*c*e", "abdx"));
        assert!(!glob("name", "username"));
        assert!(glob("*name", "username"));
        let r = usual();
        assert_eq!(r.mode("public", "users", "Email"), R);
        assert_eq!(r.mode("public", "orders", "adr_city"), R);
        assert_eq!(r.mode("public", "users", "adr_city"), V, "orders only");
        assert_eq!(r.mode("public", "users", "ssn"), H);
        assert_eq!(r.mode("public", "audit_log", "id"), H);
        assert_eq!(r.mode("public", "users", "id"), V);
        let r = rules(r#"{"redact_columns": ["public.users.name", "other.*.id"]}"#);
        assert_eq!(r.mode("public", "users", "name"), R);
        assert_eq!(r.mode("public", "orders", "id"), V);
    }

    #[test]
    fn a_whitelist_hides_what_it_doesnt_show_and_hidden_beats_redacted() {
        let r = rules(
            r#"{"type": "whitelist", "show_tables": ["orders"], "show_columns": ["users.id"],
                "redact_columns": ["adr*", "id"], "hide_columns": ["orders.total"]}"#,
        );
        assert_eq!(r.mode("public", "orders", "user_id"), V);
        assert_eq!(r.mode("public", "orders", "adr_city"), R);
        assert_eq!(r.mode("public", "orders", "total"), H);
        assert_eq!(r.mode("public", "users", "id"), R);
        assert_eq!(r.mode("public", "users", "name"), H);
        let both = rules(r#"{"redact_columns": ["ssn"], "hide_columns": ["ssn"]}"#);
        assert_eq!(both.mode("public", "users", "ssn"), H);
    }

    #[test]
    fn plain_columns_and_stars_get_their_own_mode() {
        let r = usual();
        assert_eq!(modes("select id, email, ssn from users", &r, 3), [V, R, H]);
        assert_eq!(modes("select * from users", &r, 4), [V, V, R, H]);
        assert_eq!(
            modes(
                "select u.*, o.total from users u join orders o on o.user_id = u.id",
                &r,
                5
            ),
            [V, V, R, H, V]
        );
        assert_eq!(
            modes("select o.id, u.email from orders o, users u", &r, 2),
            [V, R]
        );
        assert_eq!(modes("select count(*) from users", &r, 1), [V]);
        assert_eq!(modes("select 1", &r, 1), [V]);
        // Everything open: nothing to do.
        let open = rules("{}");
        assert_eq!(
            plan("select * from pg_stats", &shop(), &open, Lexing::default()),
            Ok(Plan::open())
        );
    }

    #[test]
    fn renamed_and_computed_columns_count_as_their_sources() {
        let r = usual();
        for sql in [
            "select email as e from users",
            "select email e from users",
            "select upper(email) from users",
            "select email || '' from users",
            "select substr(u.email, 1, 3) from users u",
            "select count(distinct email) from users",
            "select case when email like 'a%' then 1 end from users",
            "select (select email from users limit 1)",
            "select row_to_json(u) from users u",
        ] {
            let got = modes(sql, &r, 1);
            assert!(got[0] >= R, "{sql}: {got:?}");
        }
        assert_eq!(modes("select length(ssn) from users", &r, 1), [H]);
    }

    #[test]
    fn ctes_subqueries_and_unions_pass_on_what_they_use() {
        let r = usual();
        assert_eq!(
            modes(
                "with x as (select email as id from users) select id from x",
                &r,
                1
            ),
            [R]
        );
        assert_eq!(
            modes("with x as (select * from users) select * from x", &r, 4),
            [H; 4]
        );
        assert_eq!(
            modes("select t.e from (select email as e from users) t", &r, 1),
            [R]
        );
        assert_eq!(
            modes("select id from users union select email from users", &r, 1),
            [R]
        );
        assert_eq!(
            modes(
                "select id, name from users union all select id, ssn from users",
                &r,
                2
            ),
            [V, H]
        );
        // A CTE of visible columns passes on nothing.
        assert_eq!(
            modes(
                "with x as (select id, name from users) select * from x",
                &r,
                2
            ),
            [V, V]
        );
        // Columns that don't add up: the strictest for all.
        assert_eq!(modes("select id, email from users", &r, 3), [R, R, R]);
    }

    #[test]
    fn quoting_casts_whole_rows_and_more() {
        let r = usual();
        assert_eq!(
            modes(r#"select "Email", u."email" from users u"#, &r, 2),
            [R, R]
        );
        assert_eq!(modes("select `email` from users", &r, 1), [R]);
        assert_eq!(modes("select ssn::text from users", &r, 1), [H]);
        assert_eq!(modes("select json_agg(u) from users u", &r, 1), [H]);
        assert_eq!(modes("select to_jsonb(users) from users", &r, 1), [H]);
        assert_eq!(
            modes("select * from users, orders", &r, 9),
            [V, V, R, H, V, V, R, R, V]
        );
        assert_eq!(
            modes(
                "select id, (select string_agg(email, ',') from users) from orders",
                &r,
                2
            ),
            [V, R]
        );
        // Filters reveal nothing in the result itself (block_queries is for them).
        assert_eq!(modes("select id from users where email = 'x'", &r, 1), [V]);
        // The hidden audit log: even its ids.
        assert_eq!(modes("select id from audit_log", &r, 1), [H]);
    }

    #[test]
    fn only_the_databases_own_tables_while_rules_apply() {
        let r = usual();
        for (sql, what) in [
            ("select * from pg_stats", "pg_stats"),
            (
                "select * from information_schema.columns",
                "information_schema.columns",
            ),
            ("select * from generate_series(1, 3)", "generate_series"),
            (
                "select * from users u, lateral unnest(array[u.email]) e",
                "unnest",
            ),
        ] {
            let err = plan(sql, &shop(), &r, Lexing::default()).unwrap_err();
            assert!(err.starts_with(what), "{sql}: {err}");
        }
    }

    #[test]
    fn block_queries_keeps_redacted_columns_out_of_conditions() {
        let block = |level: &str| {
            rules(&format!(
                r#"{{"redact_columns": ["email"], "hide_columns": ["ssn"], "block_queries": "{level}"}}"#
            ))
        };
        let run = |sql: &str, r: &Rules| plan(sql, &shop(), r, Lexing::default()).map(|_| ());
        let filters = [
            "select id from users where email like 'a%'",
            "select id from users order by email",
            "select count(*) from users group by email",
            "select o.id from orders o join users u on u.email = o.adr_city",
            "select id from users where id in (select id from users where email = 'x')",
            "select id from users where length(email) = 12",
        ];
        for sql in filters {
            assert!(run(sql, &block("none")).is_ok(), "{sql}");
            let err = run(sql, &block("redacted")).unwrap_err();
            assert!(
                err.starts_with("email is redacted for AI tools"),
                "{sql}: {err}"
            );
        }
        assert!(run("select email from users", &block("redacted")).is_ok());
        let hidden = "select id from users where ssn = '1'";
        assert!(run(hidden, &block("redacted")).is_ok());
        let err = run(hidden, &block("redacted_and_hidden")).unwrap_err();
        assert!(err.starts_with("ssn is hidden"), "{err}");
    }

    #[test]
    fn a_result_as_ai_tools_see_it() {
        let mut result = ResultSet::new(
            ["id", "email", "ssn"]
                .map(|n| ColumnInfo { name: n.into() })
                .to_vec(),
        );
        let mut batch = RowBatch::default();
        for (id, email) in [("1", Some("ann@x.io")), ("2", None)] {
            batch.push_cell(Some(id));
            batch.push_cell(email);
            batch.push_cell(Some("123"));
            batch.finish_row();
        }
        result.append(batch);
        let plan = plan(
            "select id, email, ssn from users",
            &shop(),
            &usual(),
            Lexing::default(),
        )
        .unwrap();
        let (seen, note) = plan.apply(&result);
        let names: Vec<&str> = seen.columns().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["id", "email"]);
        assert_eq!(seen.cell(0, 1), Some(REDACTED));
        assert_eq!(seen.cell(1, 1), None, "NULL stays NULL");
        assert_eq!(seen.cell(1, 0), Some("2"));
        assert_eq!(
            note.unwrap(),
            "Values of email are redacted for AI tools (⟦redacted⟧); the user sees them. \
             Left out, hidden from AI tools: ssn."
        );
        let (same, none) = Plan::open().apply(&result);
        assert_eq!(same.columns().len(), 3);
        assert_eq!(none, None);
    }

    #[test]
    fn the_ai_view_comment() {
        let sql = mark("select 1");
        assert_eq!(
            sql,
            "-- ai view\n-- remove the line above to see the full result\nselect 1"
        );
        assert_eq!(mark(&sql), sql);
        for marked in [
            "-- AI View\nselect 1",
            "--ai   view\nselect 1",
            "select 1 -- Ai VIEW",
        ] {
            assert!(is_marked(marked), "{marked}");
        }
        for not in [
            "select 1",
            "-- ai views\nselect 1",
            "select '-- ai view'",
            "-- the ai view of it",
        ] {
            assert!(!is_marked(not), "{not}");
        }
    }
}
