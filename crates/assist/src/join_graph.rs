//! How tables connect: foreign keys (declared) and joins learned from the user's
//! queries (`Usage::joins`), in one graph. Finds the join path between two tables
//! and a table's neighbours. Used by the assistant's `find_join_path` and the
//! relation graph view.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use crate::schema::SchemaIndex;
use crate::usage::{Usage, parse_join_key};

#[derive(Debug, Clone, PartialEq)]
pub enum JoinSource {
    /// A declared foreign key (its name).
    ForeignKey(String),
    /// A condition the user's queries used, not declared.
    Learned,
}

/// A join between two tables (by index in the schema): `a.a_cols = b.b_cols`. For
/// a foreign key, `a` is the referencing table.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub a: usize,
    pub a_cols: Vec<String>,
    pub b: usize,
    pub b_cols: Vec<String>,
    pub source: JoinSource,
    /// How much the user's queries used it (decayed frecency; 0 if never).
    pub used: f64,
}

/// One step of a path, oriented along it: `from.from_cols = to.to_cols`.
#[derive(Debug, Clone, PartialEq)]
pub struct JoinStep {
    pub from: usize,
    pub from_cols: Vec<String>,
    pub to: usize,
    pub to_cols: Vec<String>,
    pub source: JoinSource,
}

pub struct JoinGraph {
    edges: Vec<Edge>,
    /// Per table, its edges.
    by_table: HashMap<usize, Vec<usize>>,
}

impl JoinGraph {
    pub fn new(schema: &SchemaIndex, usage: &Usage) -> Self {
        let mut edges: Vec<Edge> = Vec::new();
        for fk in &schema.info().foreign_keys {
            let (Some(a), Some(b)) = (
                schema.resolve(Some(&fk.schema), &fk.table),
                schema.resolve(Some(&fk.ref_schema), &fk.ref_table),
            ) else {
                continue;
            };
            edges.push(Edge {
                a,
                a_cols: fk.columns.clone(),
                b,
                b_cols: fk.ref_columns.clone(),
                source: JoinSource::ForeignKey(fk.name.clone()),
                used: 0.0,
            });
        }
        // Learned joins are sorted for a deterministic graph.
        let mut learned: Vec<_> = usage.joins().collect();
        learned.sort_by(|x, y| x.0.cmp(y.0));
        for (key, score) in learned {
            let Some([x, y]) = parse_join_key(key) else {
                continue;
            };
            let side = |(s, t, c): &(String, String, String)| {
                let ix = schema.resolve(Some(s), t)?;
                let col = schema
                    .columns(ix)
                    .iter()
                    .find(|m| m.name.to_lowercase() == *c)?;
                Some((ix, col.name.clone()))
            };
            let (Some((a, ac)), Some((b, bc))) = (side(&x), side(&y)) else {
                continue;
            };
            if a == b {
                continue;
            }
            // The same condition as a foreign key: it only counts as used.
            let same = |e: &Edge| {
                (e.a == a && e.b == b && e.a_cols == [ac.clone()] && e.b_cols == [bc.clone()])
                    || (e.a == b && e.b == a && e.a_cols == [bc.clone()] && e.b_cols == [ac.clone()])
            };
            match edges.iter_mut().find(|e| same(e)) {
                Some(e) => e.used += score,
                None => edges.push(Edge {
                    a,
                    a_cols: vec![ac],
                    b,
                    b_cols: vec![bc],
                    source: JoinSource::Learned,
                    used: score,
                }),
            }
        }
        let mut by_table: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, e) in edges.iter().enumerate() {
            by_table.entry(e.a).or_default().push(i);
            by_table.entry(e.b).or_default().push(i);
        }
        Self { edges, by_table }
    }

    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// The joins of `table`, each oriented from it.
    pub fn neighbours(&self, table: usize) -> Vec<JoinStep> {
        self.by_table
            .get(&table)
            .into_iter()
            .flatten()
            .map(|&i| orient(&self.edges[i], table))
            .collect()
    }

    /// The cheapest chain of joins from `from` to `to`: declared foreign keys cost
    /// less than learned joins, and joins the user's queries use often cost less
    /// again. `None` if they aren't connected; empty if `from == to`.
    pub fn path(&self, from: usize, to: usize) -> Option<Vec<JoinStep>> {
        #[derive(PartialEq)]
        struct Visit(f64, usize);
        impl Eq for Visit {}
        impl Ord for Visit {
            fn cmp(&self, other: &Self) -> Ordering {
                // Min-heap on cost; ties by table index, for a deterministic path.
                other
                    .0
                    .total_cmp(&self.0)
                    .then_with(|| other.1.cmp(&self.1))
            }
        }
        impl PartialOrd for Visit {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }
        let mut best: HashMap<usize, (f64, Option<usize>)> = HashMap::from([(from, (0.0, None))]);
        let mut heap = BinaryHeap::from([Visit(0.0, from)]);
        while let Some(Visit(cost, table)) = heap.pop() {
            if table == to {
                break;
            }
            if best.get(&table).is_some_and(|(c, _)| *c < cost) {
                continue;
            }
            let mut edges: Vec<usize> = self.by_table.get(&table).cloned().unwrap_or_default();
            edges.sort_unstable();
            for i in edges {
                let e = &self.edges[i];
                let next = if e.a == table { e.b } else { e.a };
                let c = cost + edge_cost(e);
                if best.get(&next).is_none_or(|(old, _)| c < *old) {
                    best.insert(next, (c, Some(i)));
                    heap.push(Visit(c, next));
                }
            }
        }
        best.get(&to)?;
        let mut steps = Vec::new();
        let mut at = to;
        while let Some(&(_, Some(i))) = best.get(&at) {
            let e = &self.edges[i];
            let prev = if e.a == at { e.b } else { e.a };
            steps.push(orient(e, prev));
            at = prev;
        }
        steps.reverse();
        Some(steps)
    }
}

/// A foreign key costs 1, a learned join 1.5; use lowers either by up to 0.4.
fn edge_cost(e: &Edge) -> f64 {
    let base = match e.source {
        JoinSource::ForeignKey(_) => 1.0,
        JoinSource::Learned => 1.5,
    };
    base - 0.4 * e.used / (1.0 + e.used)
}

fn orient(e: &Edge, from: usize) -> JoinStep {
    if e.a == from {
        JoinStep {
            from: e.a,
            from_cols: e.a_cols.clone(),
            to: e.b,
            to_cols: e.b_cols.clone(),
            source: e.source.clone(),
        }
    } else {
        JoinStep {
            from: e.b,
            from_cols: e.b_cols.clone(),
            to: e.a,
            to_cols: e.a_cols.clone(),
            source: e.source.clone(),
        }
    }
}

/// `FROM a JOIN b ON b.x = a.y …` for a path starting at `from`, with the tables as
/// written in SQL on this connection.
pub fn join_sql(schema: &SchemaIndex, from: usize, steps: &[JoinStep]) -> String {
    let mut sql = format!("FROM {}", schema.sql_name(from));
    for s in steps {
        let (f, t) = (schema.sql_name(s.from), schema.sql_name(s.to));
        let on: Vec<String> = s
            .from_cols
            .iter()
            .zip(&s.to_cols)
            .map(|(a, b)| format!("{t}.{} = {f}.{}", schema.quote(b), schema.quote(a)))
            .collect();
        sql.push_str(&format!("\nJOIN {t} ON {}", on.join(" AND ")));
    }
    sql
}

#[cfg(test)]
mod tests {
    use tangul_core::{ColumnMeta, DriverKind, ForeignKey, SchemaInfo, TableInfo};

    use super::*;
    use crate::dialect::dialect_for;
    use crate::usage::{UsageKind, join_key};

    /// users ← orders ← items; users ← tickets; and `audit` with no keys.
    fn schema() -> SchemaIndex {
        let names = ["users", "orders", "items", "tickets", "audit"];
        let fk = |name: &str, table: &str, col: &str, target: &str| ForeignKey {
            name: name.into(),
            schema: "app".into(),
            table: table.into(),
            columns: vec![col.into()],
            ref_schema: "app".into(),
            ref_table: target.into(),
            ref_columns: vec!["id".into()],
        };
        let col = |table: &str, name: &str| ColumnMeta {
            schema: "app".into(),
            table: table.into(),
            name: name.into(),
            data_type: "integer".into(),
            ..Default::default()
        };
        let info = SchemaInfo {
            tables: names
                .iter()
                .map(|n| TableInfo {
                    schema: "app".into(),
                    name: n.to_string(),
                    ..Default::default()
                })
                .collect(),
            columns: vec![
                col("users", "id"),
                col("orders", "id"),
                col("orders", "user_id"),
                col("items", "id"),
                col("items", "order_id"),
                col("tickets", "id"),
                col("tickets", "user_id"),
                col("audit", "user_id"),
            ],
            foreign_keys: vec![
                fk("orders_user", "orders", "user_id", "users"),
                fk("items_order", "items", "order_id", "orders"),
                fk("tickets_user", "tickets", "user_id", "users"),
            ],
            ..Default::default()
        };
        SchemaIndex::new(info, dialect_for(DriverKind::Postgres), Some("public".into()))
    }

    fn ix(s: &SchemaIndex, name: &str) -> usize {
        s.resolve(Some("app"), name).unwrap()
    }

    fn joined(usage: &mut Usage, a: (&str, &str), b: (&str, &str), times: usize) {
        for _ in 0..times {
            usage.record(UsageKind::Join, join_key(("app", a.0, a.1), ("app", b.0, b.1)));
        }
    }

    #[test]
    fn follows_foreign_keys_over_several_hops() {
        let s = schema();
        let g = JoinGraph::new(&s, &Usage::default());
        let path = g.path(ix(&s, "items"), ix(&s, "tickets")).unwrap();
        let tables: Vec<_> = path.iter().map(|p| s.table(p.to).name.as_str()).collect();
        assert_eq!(tables, ["orders", "users", "tickets"]);
        assert_eq!(
            join_sql(&s, ix(&s, "items"), &path),
            "FROM app.items\n\
             JOIN app.orders ON app.orders.id = app.items.order_id\n\
             JOIN app.users ON app.users.id = app.orders.user_id\n\
             JOIN app.tickets ON app.tickets.user_id = app.users.id"
        );
        assert_eq!(g.path(ix(&s, "users"), ix(&s, "users")), Some(Vec::new()));
        // No keys, never joined: not connected.
        assert_eq!(g.path(ix(&s, "audit"), ix(&s, "users")), None);
    }

    #[test]
    fn learned_joins_connect_what_keys_dont() {
        let s = schema();
        let mut usage = Usage::default();
        joined(&mut usage, ("audit", "user_id"), ("users", "id"), 1);
        let g = JoinGraph::new(&s, &usage);
        let path = g.path(ix(&s, "audit"), ix(&s, "orders")).unwrap();
        assert_eq!(path[0].source, JoinSource::Learned);
        assert_eq!(
            (path[0].from_cols.as_slice(), path[0].to_cols.as_slice()),
            (&["user_id".to_string()][..], &["id".to_string()][..])
        );
        assert_eq!(path[1].source, JoinSource::ForeignKey("orders_user".into()));
        assert_eq!(g.neighbours(ix(&s, "audit")).len(), 1);
    }

    #[test]
    fn a_learned_join_on_a_key_only_counts_as_use() {
        let s = schema();
        let mut usage = Usage::default();
        joined(&mut usage, ("users", "id"), ("orders", "user_id"), 3);
        let g = JoinGraph::new(&s, &usage);
        assert_eq!(g.edges().len(), 3);
        let e = &g.edges()[0];
        assert_eq!(e.source, JoinSource::ForeignKey("orders_user".into()));
        assert!(e.used > 0.0);
    }

    #[test]
    fn a_direct_key_beats_a_detour_and_use_can_tip_the_balance() {
        let s = schema();
        // tickets.user_id = orders.user_id: learned, used a lot. Against the keys
        // tickets → users → orders (cost 2), one learned hop (cost < 1.5) wins.
        let mut usage = Usage::default();
        joined(&mut usage, ("tickets", "user_id"), ("orders", "user_id"), 1);
        let g = JoinGraph::new(&s, &usage);
        let path = g.path(ix(&s, "tickets"), ix(&s, "orders")).unwrap();
        assert_eq!(path.len(), 1);
        assert_eq!(path[0].source, JoinSource::Learned);
        // Without it, the keys.
        let g = JoinGraph::new(&s, &Usage::default());
        assert_eq!(g.path(ix(&s, "tickets"), ix(&s, "orders")).unwrap().len(), 2);
    }
}
