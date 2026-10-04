//! Filling the knowledge pool (`tangul_service::knowledge`) from the connections and
//! `local.db`, and reading from it.

use super::*;

/// Most run queries the editor's continuations choose from.
const HISTORY_PATTERNS: usize = 200;

impl Workspace {
    /// The database's learned usage, loaded from `local.db` into the pool once
    /// (with its most run queries).
    pub(super) fn usage_for(&mut self, db_id: &str) -> Arc<Usage> {
        if let Some(u) = self.knowledge.usage(db_id) {
            return u;
        }
        let u = Arc::new(Usage::from_stats(
            now_ms() / 1000,
            self.data.local.usage(db_id),
        ));
        self.knowledge.set_usage(db_id, u.clone());
        self.load_history_patterns(db_id);
        u
    }

    /// The database's most run queries from `local.db` into the pool (the editor
    /// continues a statement as one of them was run).
    pub(super) fn load_history_patterns(&self, db_id: &str) {
        let patterns = self.data.local.patterns(db_id, HISTORY_PATTERNS);
        self.knowledge.set_history(db_id, Arc::new(patterns));
    }

    /// The schema of `user_id` from the pool; empty until it was opened.
    pub(super) fn schema_of(&self, user_id: &str) -> Arc<SchemaIndex> {
        self.knowledge
            .schema(user_id)
            .unwrap_or_else(|| self.schema_index(user_id, SchemaInfo::default()))
    }

    /// The active user's schema.
    pub(super) fn active_schema(&self) -> Option<Arc<SchemaIndex>> {
        Some(self.schema_of(self.active_user.as_deref()?))
    }

    /// Puts `user_id`'s schema into the pool, on connection `conn`.
    pub(super) fn set_schema(&mut self, user_id: &str, conn: ConnId, info: SchemaInfo) {
        let Some(db_id) = self.find_user(user_id).map(|(db, _)| db.id.clone()) else {
            return;
        };
        let schema = self.schema_index(user_id, info);
        self.knowledge.set_schema(user_id, &db_id, conn, schema);
        self.usage_for(&db_id);
        self.expose_to_ai();
    }

    pub(super) fn schema_index(&self, user_id: &str, info: SchemaInfo) -> Arc<SchemaIndex> {
        let (dialect, default) = self
            .find_user(user_id)
            .map(|(db, _)| {
                let dialect = dialect_for(db.driver);
                (dialect, dialect.default_schema(&db.database))
            })
            .unwrap_or((dialect_for(DriverKind::Postgres), None));
        Arc::new(SchemaIndex::new(info, dialect, default))
    }

    /// Suggestions for a table of the active connection: learned from history first,
    /// then generated from its keys.
    pub(super) fn table_suggestions(&self, table: &TableInfo) -> Vec<QuerySuggestion> {
        let (Some(schema), Some(db)) = (self.active_schema(), self.active_db.as_deref()) else {
            return Vec::new();
        };
        let Some(ix) = schema.resolve(Some(&table.schema), &table.name) else {
            return Vec::new();
        };
        let patterns = self.data.local.patterns(db, HISTORY_PATTERNS);
        suggest_for_table(ix, &schema, &patterns)
    }
}
