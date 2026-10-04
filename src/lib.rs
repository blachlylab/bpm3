//! BPM catalog library. The `bpm` binary is a thin caller of these modules.
//!
//! SQL stays in [`catalog`] and [`migrate`]. The entity rules that do not need
//! a database live in [`model`]. Walking directories and reading bytes is
//! [`ingest`].

pub mod catalog;
pub mod cli;
pub mod error;
pub mod ingest;
pub mod migrate;
pub mod model;
pub mod perms;
pub mod query;

pub use error::Error;
