//! Bracket pairs, for the editors' nesting guides: a line from each opening bracket
//! to its closing one, colored by depth. SQL pairs parentheses (subqueries, CTE
//! bodies, function arguments) by the database's lexical rules, so brackets in
//! strings, quoted names and comments don't count.
//! Unbalanced input (while typing) never fails: a bracket without its partner gets
//! no pair.

use crate::lexer::{Lexing, TokenKind, tokenize_with};

/// An opening and its closing bracket, as byte offsets, and how deeply it is
/// nested (0: outermost).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pair {
    pub open: usize,
    pub close: usize,
    pub depth: usize,
}

/// The parentheses of `sql`, in the order they open.
pub fn sql_pairs(sql: &str, lexing: Lexing) -> Vec<Pair> {
    let brackets = tokenize_with(sql, lexing)
        .into_iter()
        .filter(|t| t.kind == TokenKind::Punct)
        .filter_map(|t| match &sql[t.start..t.end] {
            "(" => Some((t.start, Some('('))),
            ")" => Some((t.start, None)),
            _ => None,
        });
    pair_up(brackets.map(|(at, open)| (at, open, ')')))
}

/// Pairs brackets given as (offset, the opening one or `None` for a closing one,
/// the closing one it takes or is). A closing bracket that doesn't close the
/// innermost open one, and opening ones never closed, get no pair.
fn pair_up(brackets: impl Iterator<Item = (usize, Option<char>, char)>) -> Vec<Pair> {
    let mut open: Vec<(usize, char)> = Vec::new();
    let mut pairs = Vec::new();
    for (at, opening, closing) in brackets {
        match opening {
            Some(_) => open.push((at, closing)),
            None => {
                if open.last().is_some_and(|&(_, c)| c == closing) {
                    let (start, _) = open.pop().expect("checked");
                    pairs.push(Pair {
                        open: start,
                        close: at,
                        depth: open.len(),
                    });
                }
            }
        }
    }
    pairs.sort_by_key(|p| p.open);
    pairs
}

/// The pairs worth a guide: those whose brackets are on different lines.
pub fn guides(text: &str, pairs: &[Pair]) -> Vec<Pair> {
    pairs
        .iter()
        .filter(|p| text[p.open..p.close].contains('\n'))
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(text: &str, pairs: &[Pair]) -> Vec<(String, usize)> {
        pairs
            .iter()
            .map(|p| (text[p.open..=p.close].to_string(), p.depth))
            .collect()
    }

    #[test]
    fn sql_parentheses_nest() {
        let sql = "select (select max(x) from t) as m, f(a, (b))";
        let pairs = sql_pairs(sql, Lexing::default());
        assert_eq!(
            spans(sql, &pairs),
            [
                ("(select max(x) from t)".to_string(), 0),
                ("(x)".to_string(), 1),
                ("(a, (b))".to_string(), 0),
                ("(b)".to_string(), 1),
            ]
        );
    }

    #[test]
    fn strings_names_and_comments_dont_count() {
        let sql = "select ')(', \"a(b\" -- (\n /* ( */ from (t)";
        let pairs = sql_pairs(sql, Lexing::default());
        assert_eq!(spans(sql, &pairs), [("(t)".to_string(), 0)]);
        // MySQL's # comments only with its rules.
        let sql = "select 1 # (\n";
        let mysql = Lexing {
            hash_comments: true,
            ..Lexing::default()
        };
        assert!(sql_pairs(sql, mysql).is_empty());
    }

    #[test]
    fn unbalanced_input_gets_no_pair_for_the_odd_bracket() {
        let sql = "select (a, (b) from t";
        assert_eq!(
            spans(sql, &sql_pairs(sql, Lexing::default())),
            [("(b)".to_string(), 1)]
        );
        let sql = "select a) from (t)";
        assert_eq!(
            spans(sql, &sql_pairs(sql, Lexing::default())),
            [("(t)".to_string(), 0)]
        );
        assert!(sql_pairs("", Lexing::default()).is_empty());
    }

    #[test]
    fn only_pairs_over_several_lines_get_a_guide() {
        let sql = "select (\n  select f(x)\n) from t";
        let pairs = sql_pairs(sql, Lexing::default());
        let guides = guides(sql, &pairs);
        assert_eq!(
            spans(sql, &guides),
            [("(\n  select f(x)\n)".to_string(), 0)]
        );
    }
}
