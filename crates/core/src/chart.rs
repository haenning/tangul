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

    /// Styles for a single value column.
    pub const SINGLE: [ChartStyle; 3] = [ChartStyle::Line, ChartStyle::Area, ChartStyle::Bars];

    /// Name for all value columns in one chart.
    pub fn label(self) -> &'static str {
        match self {
            ChartStyle::Line => "Lines",
            ChartStyle::Area => "Areas",
            ChartStyle::StackedArea => "Stacked areas",
            ChartStyle::Bars => "Bars side by side",
            ChartStyle::StackedBars => "Stacked bars",
        }
    }

    /// Name for a single value column.
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

/// What a chart shows: all value columns in one style, or a single one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChartView {
    pub style: ChartStyle,
    /// Index of the value column (the chart's series); `None` shows all of them.
    pub series: Option<usize>,
}

impl ChartView {
    /// Stored form: `"line"` for all value columns, `"bars:1"` for one of them.
    pub fn to_key(self) -> String {
        match self.series {
            None => self.style.key().to_string(),
            Some(i) => format!("{}:{i}", self.style.key()),
        }
    }

    /// Reads [`ChartView::to_key`]'s form; `None` for anything else.
    pub fn from_key(key: &str) -> Option<Self> {
        let (style, series) = match key.split_once(':') {
            Some((style, i)) => (style, Some(i.parse().ok()?)),
            None => (key, None),
        };
        let style = ChartStyle::ALL.into_iter().find(|s| s.key() == style)?;
        Some(Self { style, series })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_keys_round_trip() {
        for style in ChartStyle::ALL {
            for series in [None, Some(0), Some(3)] {
                let view = ChartView { style, series };
                assert_eq!(ChartView::from_key(&view.to_key()), Some(view));
            }
        }
        assert_eq!(ChartView::from_key("bars:x"), None);
        assert_eq!(ChartView::from_key("pie"), None);
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
