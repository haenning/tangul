//! What every front end of tangul shares, without UI: which connections are open
//! and what for ([`connections`]), whether and how a statement may run
//! ([`policy`]), and which scheduled bookmark is due ([`schedule`]).
//!
//! Running a statement from anywhere (a tab, a schedule, the assistant):
//! 1. [`policy::decide`] with the caller: run (read-only or not, time limit), ask
//!    the user first, or refuse. The assistant and schedules only ever read.
//! 2. [`connections::Connections::connect_user`] for a connection of that purpose.
//! 3. `EngineHandle::query` for the whole result (or `Command::Execute` to stream
//!    it into a tab).

pub mod connections;
pub mod policy;
pub mod schedule;
