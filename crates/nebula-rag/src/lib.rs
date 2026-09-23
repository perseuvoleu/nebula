//! Nebula RAG: durable, searchable knowledge for agents — notes, shared
//! know-how and every app's markdown docs — in one SQLite database on the
//! RAG host (findl). See docs/specs/rag.md.

pub mod chunk;
pub mod db;
pub mod embed;
pub mod store;
pub mod ws;

pub use store::{Hit, Kind, NewDoc, Note, NoteAdded, Rag};
