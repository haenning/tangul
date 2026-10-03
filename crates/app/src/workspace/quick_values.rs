//! The editor's quick looks at a text column's values: asked for by completion
//! (`SqlCompletion`) while a condition on the column is typed, run in the background
//! (`tangul_service::values`, at most a few hundred rows), kept in the knowledge
//! pool for the next keystroke. Only for databases with `editor_values` on.

use std::collections::HashSet;

use tangul_core::{ColumnMeta, ConnId, QueryId};
use tangul_service::knowledge::ColumnRef;
use tangul_service::values::quick_values;

use super::*;

/// A column of a connection completion wants values of.
pub type LookRequest = (ConnId, ColumnMeta);

pub(super) struct QuickLooks {
    requests: async_channel::Sender<LookRequest>,
    /// Running now, by database user and column: asked for once.
    running: HashSet<(String, ColumnRef)>,
    _task: Task<()>,
}

impl QuickLooks {
    pub(super) fn start(cx: &mut Context<Workspace>) -> Self {
        let (requests, rx) = async_channel::unbounded::<LookRequest>();
        let task = cx.spawn(async move |this, cx| {
            while let Ok((conn, column)) = rx.recv().await {
                if this
                    .update(cx, |ws, cx| ws.look_at_values(conn, column, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            requests,
            running: HashSet::new(),
            _task: task,
        }
    }

    pub(super) fn requests(&self) -> async_channel::Sender<LookRequest> {
        self.requests.clone()
    }
}

impl Workspace {
    /// Takes a quick look at `column`'s values on `conn`, unless its database has
    /// them turned off, or one was taken within the hour or is running.
    fn look_at_values(&mut self, conn: ConnId, column: ColumnMeta, cx: &mut Context<Self>) {
        let Some(user) = self.knowledge.user_of_conn(conn) else {
            return;
        };
        if !self
            .find_user(&user)
            .is_some_and(|(db, _)| db.editor_values)
        {
            return;
        }
        let key: ColumnRef = (
            column.schema.clone(),
            column.table.clone(),
            column.name.clone(),
        );
        if self.knowledge.has_quick_values(&user, &key)
            || !self.quick_looks.running.insert((user.clone(), key.clone()))
        {
            return;
        }
        let schema = self.schema_of(&user);
        let (engine, query) = (self.engine.clone(), QueryId(self.next_id()));
        let knowledge = self.knowledge.clone();
        cx.spawn(async move |this, cx| {
            // A failed look counts as one without values: not tried again within
            // the hour.
            let values = quick_values(&engine, query, conn, &schema, &column)
                .await
                .unwrap_or_default();
            knowledge.set_quick_values(&user, key.clone(), values);
            let _ = this.update(cx, |ws, _| ws.quick_looks.running.remove(&(user, key)));
        })
        .detach();
    }
}
