//! SQL highlighting: the grammar's own highlight query (tree-sitter-sequel), with the
//! names it gives that no theme color exists for renamed onto ones that do
//! (decimals → `number`, `CASE`/`WHEN` → `keyword`, columns → `property`,
//! parameters → `variable`), and its number patterns fixed: they are written for
//! Neovim (`%d`), which the Rust regex doesn't read, so numbers showed as strings.

use gpui_kit::component::highlighter::LanguageRegistry;

/// Captures the grammar uses that themes have no color for, and the one they get.
const RENAMES: [(&str, &str); 5] = [
    ("@float", "@number"),
    ("@conditional", "@keyword"),
    ("@field", "@property"),
    ("@parameter", "@variable"),
    ("@storageclass", "@keyword"),
];

/// Registers SQL with the corrected query (once, at start).
pub fn init() {
    let registry = LanguageRegistry::singleton();
    if let Some(mut sql) = registry.language("sql") {
        sql.highlights = sql_highlights(&sql.highlights).into();
        registry.register("sql", &sql);
    }
}

/// `query` with the captures renamed and the number patterns readable.
fn sql_highlights(query: &str) -> String {
    let mut out = query
        .replace("%d", "[0-9]")
        .replace("\\.", "[.]")
        // A number is a literal too: it must not also be a string.
        .replace(
            "(literal) @string",
            "((literal) @string (#not-match? @string \"^[-+]?[0-9]*[.]?[0-9]+$\"))",
        );
    for (from, to) in RENAMES {
        out = rename(&out, from, to);
    }
    out
}

/// `query` with capture `from` (not a longer one starting with it) named `to`.
fn rename(query: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(query.len());
    let mut rest = query;
    while let Some(at) = rest.find(from) {
        let after = &rest[at + from.len()..];
        let whole = !after.starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '.');
        out.push_str(&rest[..at]);
        out.push_str(if whole { to } else { from });
        rest = after;
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use gpui_kit::component::highlighter::{HighlightTheme, SyntaxHighlighter};
    use gpui_kit::component::input::Rope;
    use tangul_data::themes::{ThemeMode, default_for};

    use super::*;

    #[test]
    fn captures_are_renamed_whole() {
        assert_eq!(
            rename("@field @field.x @fields", "@field", "@property"),
            "@property @field.x @fields"
        );
        assert_eq!(
            sql_highlights(r#"((literal) @float (#match? @float "^[-+]?%d*\.%d*$"))"#),
            r#"((literal) @number (#match? @number "^[-+]?[0-9]*[.][0-9]*$"))"#
        );
    }

    /// The SQL editor, and JSON in the value viewer and the settings and theme files.
    #[test]
    fn every_sql_and_json_capture_has_a_theme_color() {
        init();
        for language in ["sql", "json"] {
            uncolored_captures(language);
        }
    }

    fn uncolored_captures(language: &str) {
        let query = LanguageRegistry::singleton()
            .language(language)
            .unwrap()
            .highlights;
        let captures: BTreeSet<&str> = query
            .split('@')
            .skip(1)
            .map(|c| {
                c.split(|ch: char| !(ch.is_alphanumeric() || ch == '.' || ch == '_'))
                    .next()
                    .unwrap()
            })
            .filter(|c| !c.is_empty())
            .collect();
        let theme = default_for(ThemeMode::Dark);
        let style = HighlightTheme {
            name: theme.name.clone(),
            appearance: gpui_kit::component::ThemeMode::Dark,
            style: crate::theme::config(&theme).highlight.unwrap(),
        };
        // `spell` marks text for spell checking, not a color.
        let uncolored: Vec<_> = captures
            .into_iter()
            .filter(|c| *c != "spell" && style.style(c).and_then(|s| s.color).is_none())
            .collect();
        assert_eq!(uncolored, Vec::<&str>::new(), "{language}");
    }

    #[test]
    fn sql_is_colored_by_the_theme() {
        init();
        let theme = default_for(ThemeMode::Dark);
        let style = HighlightTheme {
            name: theme.name.clone(),
            appearance: gpui_kit::component::ThemeMode::Dark,
            style: crate::theme::config(&theme).highlight.unwrap(),
        };
        let sql =
            "SELECT CASE WHEN o.total > 1.5 THEN 'big' END, count(*) FROM orders o WHERE o.id = 42";
        let mut highlighter = SyntaxHighlighter::new("sql");
        highlighter.update(None, &Rope::from_str(sql), None);
        let styles = highlighter.styles(&(0..sql.len()), &style);
        let color_of = |word: &str| {
            let at = sql.find(word).unwrap();
            styles
                .iter()
                .rev()
                .find(|(r, s)| r.contains(&at) && s.color.is_some())
                .and_then(|(_, s)| s.color)
        };
        let s = &theme.syntax;
        let c = crate::theme::color;
        assert_eq!(color_of("SELECT"), Some(c(s.keyword)));
        assert_eq!(color_of("CASE"), Some(c(s.keyword)), "conditional");
        assert_eq!(color_of("1.5"), Some(c(s.number)), "a decimal");
        assert_eq!(color_of("42"), Some(c(s.number)), "an integer");
        assert_eq!(color_of("'big'"), Some(c(s.string)));
        assert_eq!(color_of("count"), Some(c(s.function)));
        assert_eq!(color_of("total"), Some(c(s.property)), "a column");
    }
}
