//! What tangul can do beyond the tools, for a model to suggest when it helps the
//! user. Built from what the app supports (chart styles and limits), so it can't
//! promise more.

use tangul_assist::chart::ChartStyle;

/// The text `tangul_features` gives.
pub fn text() -> String {
    let styles: Vec<String> = ChartStyle::ALL
        .iter()
        .map(|s| format!("{} ({})", s.key(), s.label().to_lowercase()))
        .collect();
    format!(
        "\
tangul is the database app the user has open; these tools read its databases. What \
it can do for the user, to suggest when it helps:

- Showing: run_readonly with \"show\" opens its result in tangul for the user, in a \
new tab, whole (not cut to your limit) and without running it again: the grid, or a \
chart. Use it to visualize data, draw a graph, or hand over a finished read query.
- Charts: \"show\": {{\"chart\": style}}, plus \"column\" for one number column by name \
(the others are left out). The first column is the x axis (dates or labels), then one \
or more number columns, one series each. Styles: {styles}. Any number of rows; few \
rows and columns read best (GROUP BY). If the result doesn't fit the chart, tangul \
shows the grid.
- Bookmarks: the user can save a query under a name, and schedule it to run now and \
then; tangul then tracks its duration and row count over time. list_bookmarks lists \
them; run_readonly with \"bookmark\" runs one (with \"show\", for the user too).
- Boards: several queries of one database at a glance, each a tile shown as a \
table or a chart. list_boards and get_board read them; create_board and edit_board \
set them up when the user asks. The user's board runs the tiles, not you.
- Changes: statements that change data or schema are never run by you; propose_sql \
opens them in a tab for the user to review and run as themselves. It is only for \
changes, except on a schema-only database.
- Schema only: on a database marked so in list_connections you get its structure, \
never a row; propose_sql opens your queries for the user to run, and their result \
stays with them.
- Results: the user can filter, sort and page a result without writing SQL, follow \
foreign keys to related rows, and compare two values side by side.
- Safety: you only read, in a read-only transaction that is rolled back, as the \
database's AI user (ideally one that may only read).",
        styles = styles.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_every_chart_style() {
        let t = text();
        for style in ChartStyle::ALL {
            assert!(t.contains(style.key()), "{}", style.key());
        }
        assert!(t.contains("line (lines), area (areas), stacked_area (stacked areas)"));
    }
}
