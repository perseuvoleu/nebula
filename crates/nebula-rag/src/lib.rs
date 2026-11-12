//! Nebula RAG: durable, searchable knowledge for agents — notes, shared
//! know-how and every app's markdown docs — in one SQLite database on the
//! RAG host (findl). See docs/specs/rag.md.

#![allow(
    clippy::disallowed_methods,
    reason = "clippy.toml bans sleeps and wall clock in integration tests only; product code waits and times by design"
)]

pub mod apps;
pub mod chunk;
pub mod cli;
pub mod db;
pub mod embed;
pub mod ignore;
pub mod obs;
pub mod setup;
pub mod store;
pub mod sync;
pub mod ws;

pub use obs::{Category, Obs, ObsFilter, ObsStats, Status, Transition};
pub use store::{Hit, Kind, NewDoc, Note, NoteAdded, Rag};
