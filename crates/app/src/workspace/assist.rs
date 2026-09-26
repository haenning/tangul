//! What completion and suggestions know: schema and learned usage per connection.

use super::*;

impl Workspace {
    pub(super) fn usage_for(&mut self, db_id: &str) -> Arc<Usage> {
        if let Some(u) = self.usage.get(db_id) {
            return u.clone();
        }
        let u = Arc::new(self.data.local.usage(db_id, now_ms() / 1000));
        self.usage.insert(db_id.to_string(), u.clone());
        u
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

    /// Publishes a connection's schema and usage to the completion providers.
    pub(super) fn sync_assist(&mut self, user_id: &str, cx: &App) {
        let Some(db_id) = self.find_user(user_id).map(|(db, _)| db.id.clone()) else {
            return;
        };
        let usage = self.usage_for(&db_id);
        if let Some(state) = self.conns.get(user_id).map(|c| c.read(cx)) {
            self.assist.borrow_mut().insert(
                state.id,
                AssistData {
                    schema: state.schema.clone(),
                    usage,
                },
            );
        }
    }

    pub(super) fn sync_assist_for_db(&mut self, db_id: &str, cx: &App) {
        let users: Vec<String> = self
            .data
            .profiles
            .database(db_id)
            .into_iter()
            .flat_map(|db| &db.users)
            .filter(|u| self.conns.contains_key(&u.id))
            .map(|u| u.id.clone())
            .collect();
        for u in users {
            self.sync_assist(&u, cx);
        }
    }

    /// Suggestions for a table of the active connection: learned from history first,
    /// then generated from its keys.
    pub(super) fn table_suggestions(&self, table: &TableInfo, cx: &App) -> Vec<QuerySuggestion> {
        let (Some(state), Some(db)) = (self.active_conn(cx), self.active_db.as_deref()) else {
            return Vec::new();
        };
        let Some(ix) = state.schema.resolve(Some(&table.schema), &table.name) else {
            return Vec::new();
        };
        let patterns = self.data.local.patterns(db, 200);
        suggest_for_table(ix, &state.schema, &patterns)
    }
}
