//! How a result is drawn as a chart: the settings, never the data. The chart
//! itself (which results fit, the numbers to draw) is `tangul-assist`'s.

/// How the series are drawn, all in one chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartStyle {
    Line,
    /// Lines with the surface below them filled.
    Area,
    /// Areas on top of each other: the top line is the total.
    StackedArea,
    /// One bar per series, next to each other.
    Bars,
    /// Bars on top of each other: the top is the total.
    StackedBars,
}

impl ChartStyle {
    pub const ALL: [ChartStyle; 5] = [
        ChartStyle::Line,
        ChartStyle::Area,
        ChartStyle::StackedArea,
        ChartStyle::Bars,
        ChartStyle::StackedBars,
    ];

    /// Styles for a single series (stacking needs several).
    pub const SINGLE: [ChartStyle; 3] = [ChartStyle::Line, ChartStyle::Area, ChartStyle::Bars];

    /// Name with several series.
    pub fn label(self) -> &'static str {
        match self {
            ChartStyle::Line => "Lines",
            ChartStyle::Area => "Areas",
            ChartStyle::StackedArea => "Stacked areas",
            ChartStyle::Bars => "Bars side by side",
            ChartStyle::StackedBars => "Stacked bars",
        }
    }

    /// Name with a single series.
    pub fn single_label(self) -> &'static str {
        match self {
            ChartStyle::Line => "Line",
            ChartStyle::Area | ChartStyle::StackedArea => "Area",
            ChartStyle::Bars | ChartStyle::StackedBars => "Bars",
        }
    }

    pub fn is_bars(self) -> bool {
        matches!(self, ChartStyle::Bars | ChartStyle::StackedBars)
    }

    pub fn is_stacked(self) -> bool {
        matches!(self, ChartStyle::StackedArea | ChartStyle::StackedBars)
    }

    /// The same without stacking: stacked areas are areas, stacked bars are bars.
    pub fn unstacked(self) -> Self {
        match self {
            ChartStyle::StackedArea => ChartStyle::Area,
            ChartStyle::StackedBars => ChartStyle::Bars,
            style => style,
        }
    }

    /// Stable name for storing a view, and for the AI tools: `line`, `stacked_bars`, …
    pub fn key(self) -> &'static str {
        match self {
            ChartStyle::Line => "line",
            ChartStyle::Area => "area",
            ChartStyle::StackedArea => "stacked_area",
            ChartStyle::Bars => "bars",
            ChartStyle::StackedBars => "stacked_bars",
        }
    }
}

/// What a chart shows: its style. The result's layout decides the rest (label
/// columns first, one value column last: `tangul-assist`'s `chart`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChartView {
    pub style: ChartStyle,
    /// Stored before charts took one value column (several value columns, or one
    /// of them picked): the result may not fit any more, and the query may need
    /// recreating. Kept as stored until the view or the query is changed.
    pub old_layout: bool,
}

/// Marks a stored view of the current layout (one value column last).
const STAMP: &str = "@2";

impl ChartView {
    pub fn new(style: ChartStyle) -> Self {
        Self {
            style,
            old_layout: false,
        }
    }

    /// Stored form: `"line@2"`; a view from the old layout keeps its unstamped
    /// `"line"`, so saving a board doesn't hide that it needs recreating.
    pub fn to_key(self) -> String {
        if self.old_layout {
            self.style.key().to_string()
        } else {
            format!("{}{STAMP}", self.style.key())
        }
    }

    /// Reads [`ChartView::to_key`]'s form, and the old layout's `"line"` and
    /// `"bars:1"` (one value column of several) as [`Self::old_layout`]; `None`
    /// for anything else.
    pub fn from_key(key: &str) -> Option<Self> {
        let (style, old_layout) = match key.strip_suffix(STAMP) {
            Some(style) => (style, false),
            None => match key.split_once(':') {
                Some((style, i)) => {
                    i.parse::<usize>().ok()?;
                    (style, true)
                }
                None => (key, true),
            },
        };
        let style = ChartStyle::from_key(style)?;
        Some(Self { style, old_layout })
    }
}

impl ChartStyle {
    /// The style named by [`ChartStyle::key`].
    pub fn from_key(key: &str) -> Option<Self> {
        ChartStyle::ALL.into_iter().find(|s| s.key() == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_keys_round_trip() {
        for style in ChartStyle::ALL {
            for old_layout in [false, true] {
                let view = ChartView { style, old_layout };
                assert_eq!(ChartView::from_key(&view.to_key()), Some(view));
            }
        }
        assert_eq!(ChartView::new(ChartStyle::Bars).to_key(), "bars@2");
        // The old layout: all value columns, or one of them.
        let old = |style| {
            Some(ChartView {
                style,
                old_layout: true,
            })
        };
        assert_eq!(
            ChartView::from_key("stacked_bars"),
            old(ChartStyle::StackedBars)
        );
        assert_eq!(ChartView::from_key("line:1"), old(ChartStyle::Line));
        assert_eq!(ChartView::from_key("bars:x"), None);
        assert_eq!(ChartView::from_key("pie"), None);
        assert_eq!(ChartView::from_key("pie@2"), None);
        assert_eq!(ChartView::from_key(""), None);
    }

    #[test]
    fn unstacked_keeps_the_kind() {
        assert_eq!(ChartStyle::StackedArea.unstacked(), ChartStyle::Area);
        assert_eq!(ChartStyle::StackedBars.unstacked(), ChartStyle::Bars);
        for style in ChartStyle::SINGLE {
            assert_eq!(style.unstacked(), style);
        }
    }
}
