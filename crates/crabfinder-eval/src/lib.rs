//! crabfinder-eval library surface.
//!
//! Modules are added incrementally per file 14 §4 implementation steps.
//! Step 1 ships empty module stubs; later steps fill them in.

pub mod adapters;
pub mod batch_mode;
pub mod benchmark;
pub mod cli;
pub mod eval_set;
pub mod generator;
pub mod metrics;
pub mod report;
pub mod runner;
