//! Pinned tabs as kept between runs, per database: only query and board tabs
//! belong to a database; the others are never kept.

use serde::{Deserialize, Serialize};

use crate::board::{view_in, view_out};
use crate::chart::ChartView;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PinnedTab {
    /// A query tab: its text (passwords replaced), unrun when it comes back.
    Query {
        title: String,
        sql: String,
        /// The database user it ran as; `None` takes the active one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bookmark_id: Option<String>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "view_out",
            deserialize_with = "view_in"
        )]
        view: Option<ChartView>,
    },
    Board {
        board_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_form() {
        let tabs = vec![
            PinnedTab::Query {
                title: "Signups".into(),
                sql: "select 1".into(),
                user_id: Some("u1".into()),
                bookmark_id: None,
                view: ChartView::from_key("bars"),
            },
            PinnedTab::Board {
                board_id: "b1".into(),
            },
        ];
        let json = serde_json::to_string(&tabs).unwrap();
        assert_eq!(
            json,
            r#"[{"kind":"query","title":"Signups","sql":"select 1","user_id":"u1","view":"bars"},{"kind":"board","board_id":"b1"}]"#
        );
        assert_eq!(serde_json::from_str::<Vec<PinnedTab>>(&json).unwrap(), tabs);
        // A chart a newer version knows comes back as the table.
        let newer = r#"[{"kind":"query","title":"t","sql":"s","view":"pie"}]"#;
        let PinnedTab::Query { view, .. } =
            &serde_json::from_str::<Vec<PinnedTab>>(newer).unwrap()[0]
        else {
            panic!()
        };
        assert_eq!(*view, None);
    }
}
