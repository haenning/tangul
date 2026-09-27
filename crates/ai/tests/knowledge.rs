//! The knowledge tools, called through the registry as a model would, against a
//! hand-built schema: exact answers, no database.

use std::sync::Arc;

use serde_json::json;
use tangul_ai::{Ctx, Exposed, Registry, ToolError};
use tangul_assist::dialect::dialect_for;
use tangul_assist::schema::SchemaIndex;
use tangul_assist::usage::{Usage, UsageKind, join_key};
use tangul_core::{
    CheckInfo, ColumnMeta, ConnId, DriverKind, EnumInfo, ForeignKey, IndexInfo, SchemaInfo,
    TableInfo, TableKind, ViewDef,
};
use tangul_service::knowledge::Knowledge;

fn table(name: &str, kind: TableKind, comment: Option<&str>, rows: Option<u64>) -> TableInfo {
    TableInfo {
        schema: "app".into(),
        name: name.into(),
        kind,
        comment: comment.map(str::to_string),
        row_estimate: rows,
        size_bytes: None,
    }
}

fn col(table: &str, name: &str, ty: &str) -> ColumnMeta {
    ColumnMeta {
        schema: "app".into(),
        table: table.into(),
        name: name.into(),
        data_type: ty.into(),
        nullable: true,
        ..Default::default()
    }
}

/// A small shop: users, orders with an enum status, and a view.
fn shop() -> SchemaInfo {
    SchemaInfo {
        tables: vec![
            table(
                "orders",
                TableKind::Table,
                Some("Customer orders; totals in EUR"),
                Some(300_000),
            ),
            table("revenue", TableKind::View, None, None),
            table(
                "users",
                TableKind::Table,
                Some("Registered customers"),
                Some(100_000),
            ),
        ],
        columns: vec![
            ColumnMeta {
                primary_key: true,
                identity: true,
                nullable: false,
                ..col("orders", "id", "bigint")
            },
            ColumnMeta {
                nullable: false,
                ..col("orders", "user_id", "integer")
            },
            ColumnMeta {
                nullable: false,
                default: Some("'open'::app.order_status".into()),
                enum_type: Some("app.order_status".into()),
                ..col("orders", "status", "app.order_status")
            },
            ColumnMeta {
                nullable: false,
                ..col("orders", "total", "numeric(10,2)")
            },
            ColumnMeta {
                comment: Some("Day the order was placed".into()),
                ..col("orders", "ordered", "date")
            },
            col("revenue", "status", "app.order_status"),
            col("revenue", "revenue", "numeric"),
            ColumnMeta {
                primary_key: true,
                identity: true,
                nullable: false,
                ..col("users", "id", "integer")
            },
            ColumnMeta {
                nullable: false,
                comment: Some("Login; unique".into()),
                ..col("users", "email", "text")
            },
        ],
        foreign_keys: vec![ForeignKey {
            name: "orders_user_id_fkey".into(),
            schema: "app".into(),
            table: "orders".into(),
            columns: vec!["user_id".into()],
            ref_schema: "app".into(),
            ref_table: "users".into(),
            ref_columns: vec!["id".into()],
        }],
        indexes: vec![
            IndexInfo {
                schema: "app".into(),
                table: "orders".into(),
                name: "orders_pkey".into(),
                columns: vec!["id".into()],
                unique: true,
                primary: true,
                ..Default::default()
            },
            IndexInfo {
                schema: "app".into(),
                table: "orders".into(),
                name: "orders_open".into(),
                columns: vec!["user_id".into()],
                predicate: Some("status = 'open'".into()),
                ..Default::default()
            },
        ],
        checks: vec![CheckInfo {
            schema: "app".into(),
            table: "orders".into(),
            name: "orders_total_check".into(),
            expression: "total >= 0".into(),
        }],
        enums: vec![EnumInfo {
            schema: "app".into(),
            name: "order_status".into(),
            values: vec!["open".into(), "paid".into(), "cancelled".into()],
            comment: Some("Where an order is".into()),
        }],
        views: vec![ViewDef {
            schema: "app".into(),
            name: "revenue".into(),
            definition: "SELECT status, sum(total) AS revenue\nFROM app.orders\nGROUP BY status"
                .into(),
        }],
        routines: Vec::new(),
    }
}

fn exposed(name: &str, user: &str) -> Exposed {
    Exposed {
        name: name.into(),
        user: user.into(),
        driver: DriverKind::Postgres,
        values_allowed: false,
    }
}

/// The shop exposed as `shop`; `other` is exposed too when `two` is set.
fn ctx(two: bool) -> Ctx {
    shop_ctx(Knowledge::default(), two)
}

/// [`ctx`] over `knowledge`, a handle to the same pool the tools read.
fn shop_ctx(knowledge: Knowledge, two: bool) -> Ctx {
    let schema = Arc::new(SchemaIndex::new(
        shop(),
        dialect_for(DriverKind::Postgres),
        Some("public".into()),
    ));
    knowledge.set_schema("u1", "db1", ConnId(1), schema.clone());
    let mut list = vec![exposed("shop", "u1")];
    if two {
        knowledge.set_schema("u2", "db2", ConnId(2), schema);
        list.push(exposed("other", "u2"));
    }
    Ctx::new(knowledge, None, list)
}

async fn text(ctx: &Ctx, tool: &str, input: serde_json::Value) -> String {
    Registry::standard()
        .call(ctx, tool, input)
        .await
        .unwrap_or_else(|e| panic!("{tool}: {e}"))
        .text
}

async fn error(ctx: &Ctx, tool: &str, input: serde_json::Value) -> ToolError {
    Registry::standard()
        .call(ctx, tool, input)
        .await
        .expect_err("an error")
}

#[tokio::test]
async fn every_tool_has_a_schema_and_a_description() {
    let specs = Registry::standard().specs();
    let names: Vec<_> = specs.iter().map(|s| s.name).collect();
    assert_eq!(
        names,
        [
            "list_connections",
            "search_schema",
            "describe_table",
            "find_join_path",
            "schema_slice",
            "column_profile",
            "list_enums",
            "view_definition",
            "run_readonly"
        ]
    );
    for s in &specs {
        assert!(s.description.len() > 40, "{}", s.name);
        assert_eq!(s.input_schema["type"], "object", "{}", s.name);
    }
    let describe = &specs[2].input_schema;
    assert_eq!(describe["required"], json!(["table"]));
    assert!(
        describe["properties"]["table"]["description"]
            .as_str()
            .unwrap()
            .contains("schema.orders")
    );
}

#[tokio::test]
async fn lists_the_exposed_connections_only() {
    let ctx = ctx(false);
    assert_eq!(
        text(&ctx, "list_connections", json!({})).await,
        "shop (PostgreSQL, 3 tables)"
    );
    let none = Ctx::new(Knowledge::default(), None, Vec::new());
    assert_eq!(
        text(&none, "list_connections", serde_json::Value::Null).await,
        "No connection is enabled for the assistant."
    );
    // Exposed but not connected yet.
    let pending = Ctx::new(Knowledge::default(), None, vec![exposed("later", "u9")]);
    assert_eq!(
        text(&pending, "list_connections", json!({})).await,
        "later (PostgreSQL, not connected)"
    );
    assert_eq!(
        error(&pending, "describe_table", json!({"table": "orders"})).await,
        ToolError::Failed("later is not connected yet".into())
    );
}

#[tokio::test]
async fn describes_a_table_with_everything_known() {
    let ctx = ctx(false);
    let expected = "\
app.orders (table, ~300000 rows) — Customer orders; totals in EUR
columns:
  id bigint pk identity
  user_id integer not null → app.users.id
  status app.order_status not null default 'open'::app.order_status {open, paid, cancelled}
  total numeric(10,2) not null
  ordered date — Day the order was placed
indexes:
  orders_open (user_id) where status = 'open'
checks:
  total >= 0";
    assert_eq!(
        text(&ctx, "describe_table", json!({"table": "orders"})).await,
        expected
    );
    // The same with its schema, and a table others refer to.
    assert_eq!(
        text(&ctx, "describe_table", json!({"table": "app.orders"})).await,
        expected
    );
    assert_eq!(
        text(&ctx, "describe_table", json!({"table": "users"})).await,
        "\
app.users (table, ~100000 rows) — Registered customers
columns:
  id integer pk identity
  email text not null — Login; unique
referenced by:
  app.orders (user_id) → (id)"
    );
    // A view shows its query.
    assert!(
        text(&ctx, "describe_table", json!({"table": "revenue"}))
            .await
            .ends_with("definition:\n  SELECT status, sum(total) AS revenue\n  FROM app.orders\n  GROUP BY status")
    );
}

#[tokio::test]
async fn structured_data_matches_the_text() {
    let out = Registry::standard()
        .call(&ctx(false), "describe_table", json!({"table": "orders"}))
        .await
        .unwrap();
    assert_eq!(out.data["name"], "app.orders");
    assert_eq!(out.data["kind"], "Table");
    assert_eq!(
        out.data["columns"][2]["enum_values"],
        json!(["open", "paid", "cancelled"])
    );
    assert_eq!(out.data["columns"][1]["references"], "app.users.id");
    assert_eq!(out.data["checks"][0]["expression"], "total >= 0");
}

#[tokio::test]
async fn searches_names_comments_and_enum_values() {
    let ctx = ctx(false);
    assert_eq!(
        text(&ctx, "search_schema", json!({"query": "cancelled"})).await,
        "app.orders.status — value 'cancelled'"
    );
    assert_eq!(
        text(&ctx, "search_schema", json!({"query": "customers"})).await,
        // The exact word first, then the singular ("Customer orders").
        "app.users — comment: \"Registered customers\"\n\
         app.orders — comment: \"Customer orders; totals in EUR\""
    );
    assert_eq!(
        text(&ctx, "search_schema", json!({"query": "order", "limit": 2})).await,
        "app.orders — name\napp.orders.ordered — name"
    );
    assert_eq!(
        text(&ctx, "search_schema", json!({"query": "invoice"})).await,
        "Nothing matches \"invoice\"."
    );
}

#[tokio::test]
async fn enums_and_views() {
    let ctx = ctx(false);
    assert_eq!(
        text(&ctx, "list_enums", json!({})).await,
        "app.order_status: open, paid, cancelled — Where an order is"
    );
    assert_eq!(
        text(&ctx, "view_definition", json!({"view": "app.revenue"})).await,
        "SELECT status, sum(total) AS revenue\nFROM app.orders\nGROUP BY status"
    );
    assert_eq!(
        error(&ctx, "view_definition", json!({"view": "orders"})).await,
        ToolError::NotFound("orders is a table, not a view".into())
    );
}

#[tokio::test]
async fn bad_input_and_unknown_names_say_what_to_do() {
    let one = ctx(false);
    assert_eq!(
        error(&one, "describe_table", json!({"table": "invoices"})).await,
        ToolError::NotFound("table invoices; search_schema finds tables by name or comment".into())
    );
    assert!(matches!(
        error(&one, "describe_table", json!({})).await,
        ToolError::BadInput(m) if m.contains("table")
    ));
    assert!(matches!(
        error(&one, "describe_table", json!({"table": "orders", "tabel": "x"})).await,
        ToolError::BadInput(m) if m.contains("tabel")
    ));
    assert_eq!(
        error(&one, "drop_table", json!({})).await,
        ToolError::NotFound("no tool named drop_table".into())
    );
    // Several connections: which one must be said.
    let two = ctx(true);
    assert_eq!(
        error(&two, "list_enums", json!({})).await,
        ToolError::BadInput("several connections; name one of: shop, other".into())
    );
    assert!(
        text(&two, "list_enums", json!({"connection": "Other"}))
            .await
            .starts_with("app.order_status")
    );
    assert_eq!(
        error(&two, "list_enums", json!({"connection": "prod"})).await,
        ToolError::NotFound("connection prod; available: shop, other".into())
    );
    // Queries need the engine.
    assert_eq!(
        error(&one, "run_readonly", json!({"sql": "SELECT 1"})).await,
        ToolError::NotAllowed("queries are not available here".into())
    );
}

#[tokio::test]
async fn joins_along_foreign_keys_and_learned_joins() {
    let knowledge = Knowledge::default();
    let ctx = shop_ctx(knowledge.clone(), false);
    assert_eq!(
        text(
            &ctx,
            "find_join_path",
            json!({"from": "orders", "to": "users"})
        )
        .await,
        "app.orders.user_id = app.users.id (foreign key orders_user_id_fkey)\n\
         FROM app.orders\n\
         JOIN app.users ON app.users.id = app.orders.user_id"
    );
    // Backwards, the same condition.
    assert!(
        text(
            &ctx,
            "find_join_path",
            json!({"from": "users", "to": "orders"})
        )
        .await
        .starts_with("app.users.id = app.orders.user_id (foreign key orders_user_id_fkey)")
    );
    // The view isn't connected to anything …
    assert_eq!(
        text(
            &ctx,
            "find_join_path",
            json!({"from": "revenue", "to": "users"})
        )
        .await,
        "No join path between app.revenue and app.users: no foreign keys or joins from \
         your queries connect them."
    );
    // … until the user's queries joined it.
    let mut usage = Usage::default();
    usage.record(
        UsageKind::Join,
        join_key(("app", "revenue", "status"), ("app", "orders", "status")),
    );
    knowledge.set_usage("db1", Arc::new(usage));
    assert_eq!(
        text(
            &ctx,
            "find_join_path",
            json!({"from": "revenue", "to": "users"})
        )
        .await,
        "app.revenue.status = app.orders.status (learned from your queries)\n\
         app.orders.user_id = app.users.id (foreign key orders_user_id_fkey)\n\
         FROM app.revenue\n\
         JOIN app.orders ON app.orders.status = app.revenue.status\n\
         JOIN app.users ON app.users.id = app.orders.user_id"
    );
    assert!(
        text(&ctx, "describe_table", json!({"table": "revenue"}))
            .await
            .contains("joined in your queries:\n  app.revenue.status = app.orders.status\n")
    );
}

#[tokio::test]
async fn a_question_gets_its_slice_of_the_schema() {
    let ctx = ctx(false);
    assert_eq!(
        text(
            &ctx,
            "schema_slice",
            json!({"question": "Which customers cancelled orders?"})
        )
        .await,
        "\
app.orders (~300000 rows) — Customer orders; totals in EUR
  id bigint pk, user_id integer → app.users.id, status app.order_status {open, paid, cancelled}, \
total numeric(10,2), ordered date \"Day the order was placed\"
  check total >= 0
  why: comment: \"Customer orders; totals in EUR\", name; status: value 'cancelled'; ordered: name

app.users (~100000 rows) — Registered customers
  id integer pk, email text \"Login; unique\"
  why: comment: \"Registered customers\""
    );
    assert_eq!(
        text(
            &ctx,
            "schema_slice",
            json!({"question": "weather in Berlin"})
        )
        .await,
        "No table matches \"weather in Berlin\"; try other words with search_schema, or list \
         the tables with a query."
    );
}

#[tokio::test]
async fn column_values_only_with_the_users_consent() {
    let ctx = ctx(false);
    assert_eq!(
        error(
            &ctx,
            "column_profile",
            json!({"table": "orders", "column": "status"})
        )
        .await,
        ToolError::NotAllowed(
            "reading column values is off for this database; the user can allow it in the \
             AI menu of the title bar"
                .into()
        )
    );
}
