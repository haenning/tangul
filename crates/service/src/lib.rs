//! What every front end of tangul shares, without UI: which connections are open
//! and what for ([`connections`]), what is known about them ([`knowledge`]), which
//! scheduled bookmark is due ([`schedule`]), changing and refreshing boards
//! ([`boards`]), and the editor's quick look at a column's values ([`values`]).
//!
//! Running a statement from anywhere (a tab, a schedule, the assistant):
//! 1. [`connections::Connections`] for a connection of that purpose (the assistant
//!    uses the database's assistant user, if one is set).
//! 2. `EngineHandle::query` (whole result) or `Command::Execute` (streamed into a
//!    tab), saying who the caller is. The engine's gate decides from its own copy
//!    of `settings.json` whether and how it runs; the assistant's writes wait for a
//!    person's approval (`tangul_engine::gate`). A front end can ask the same
//!    `tangul_engine::decide` beforehand to show a dialog.

pub mod boards;
pub mod connections;
pub mod knowledge;
pub mod schedule;
pub mod values;
