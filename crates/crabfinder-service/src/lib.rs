//! crabfinder-service library surface.
//!
//! Everything the service does lives here: the resolver, query building,
//! dedup, hydration, the batch pipeline and the HTTP handlers + router
//! (`http`). The `crabfinder-service` binary (`src/main.rs`) is a thin
//! `Args` → `AppState::load` → `http::router` → `serve` wrapper; the
//! `crabfinder-batch` CLI and `crabfinder-eval` batch mode reuse the same
//! modules.

pub mod abbreviation;
pub mod addr_count;
pub mod batch;
pub mod dedup;
pub mod focus;
pub mod http;
pub mod hydrate;
pub mod interpolate;
pub mod localities;
pub mod mid_street;
pub mod query_builder;
pub mod resolver;
pub mod spatial;
pub mod state;
pub mod telemetry;

pub use state::{AppState, LoadOptions, chrono_lite};
