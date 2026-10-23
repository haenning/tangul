//! SQL assistance without UI: tokenizing, statement analysis, fingerprints, editor
//! completion, usage-based ranking, query suggestions and telling questions from SQL.

pub mod ai_rules;
pub mod ai_view;
pub mod analyze;
pub mod chart;
pub mod columns;
pub mod complete;
pub mod copy;
pub mod dialect;
pub mod digest;
pub mod edit;
pub mod export;
pub mod format;
pub mod grammar;
pub mod guard;
pub mod hints;
pub mod join_graph;
pub mod json;
pub mod lexer;
pub mod mark;
pub mod nl;
pub mod profile;
pub mod redact;
pub mod refine;
pub mod related;
pub mod schema;
pub mod slice;
pub mod statement;
pub mod suggest;
pub mod usage;
pub mod value;
