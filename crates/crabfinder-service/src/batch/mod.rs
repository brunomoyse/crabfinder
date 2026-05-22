//! Batch geocoding plumbing.
//!
//! Two thin façades — `crate::batch::http` (Lot E) and the future
//! `crabfinder-batch` CLI (Lot F) — sit on top of this module. Both feed
//! `crate::resolver::resolve()` row-by-row.
//!
//! See `docs/design-batch-geocoding.md` and `docs/v1-batch-plan.md` for
//! the architectural context.
//!
//! Lot D scope: pure I/O (CSV in/out) + intra-batch dédup. No HTTP, no CLI,
//! no parallelism dispatch wired up beyond the rayon par-iter inside
//! `dedup_then_resolve()`.

pub mod dedup;
pub mod http;
pub mod io;

// Re-exports kept narrow: only what `main.rs` actually uses at the router
// level. Sub-modules import each other directly to avoid trait/symbol
// duplication.
pub use http::{batch_geocode, body_limit_layer};
