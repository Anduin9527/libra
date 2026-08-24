#![allow(
    dead_code,
    reason = "M2-01 freezes I/O-free contracts before the M2-04 writer consumes them"
)]

//! Versioned Agent Memory domain contracts.
//!
//! This module intentionally exposes only validated, I/O-free domain values.
//! Storage, projection, compilation, and command adapters are implemented by
//! later plan slices and must not become alternate write seams.

mod canonical;
mod domain;
mod error;
mod fts_sql;
mod job_sql;
mod policy;
mod store;
mod tree;
mod validation;
mod writer;
