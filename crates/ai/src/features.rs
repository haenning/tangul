//! What tangul can do beyond the tools, for a model to suggest when it helps the
//! user. Built from what the app supports (chart styles and limits), so it can't
//! promise more.

use tangul_assist::chart::{ChartStyle, MAX_BARS, MAX_POINTS, MAX_SERIES};

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

- Charts: a result can be shown as a chart instead of the grid. The first column is \
the x axis (dates or labels), then 1–{MAX_SERIES} number columns, one series each. \
Styles: {styles}. Bars show up to {MAX_BARS} rows, lines up to {MAX_POINTS} points. \
To show one, call propose_sql with the query and \"chart\": it opens in a tab set to \
that chart, and the user runs it.
- Changes: statements that change data or schema are never run by you; propose_sql \
opens them in a tab for the user to review and run as themselves.
- Bookmarks: the user can save a query under a name, and schedule it to run now and \
then; tangul then tracks its duration and row count over time.
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
