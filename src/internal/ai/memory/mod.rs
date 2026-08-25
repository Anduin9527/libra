#![allow(
    dead_code,
    reason = "M2-01 freezes I/O-free contracts before the M2-04 writer consumes them"
)]

//! Versioned Agent Memory domain contracts.
//!
//! This module intentionally exposes only validated, I/O-free domain values.
//! Storage, projection, compilation, and command adapters are implemented by
//! later plan slices and must not become alternate write seams.

mod admission;
mod applicability;
mod canonical;
mod compiler;
mod domain;
mod error;
mod evidence;
mod fts_sql;
mod job;
mod job_sql;
mod job_state;
mod limits;
mod observer;
mod policy;
mod projection;
mod query;
mod reader;
mod replay;
mod runner;
mod selector;
mod source;
mod store;
mod tree;
mod validation;
mod view;
mod writer;

pub(crate) use job::schedule_observer_repair;
