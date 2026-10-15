//! HTTP surface of the service: handlers, response shapes and the router.
//!
//! Lives in the library (not `main.rs`) so integration tests drive the real
//! handlers through `router()` with `tower::ServiceExt::oneshot`, and the
//! binary stays a thin `Args` → `AppState::load` → `serve` wrapper.

pub mod building;
pub mod error;
pub mod feature;
pub mod geocode;
pub mod health;
pub mod reverse;

use axum::{Router, routing::get};
use tower_http::cors::{Any, CorsLayer};

use crate::AppState;
use crate::batch;

pub use error::ServiceError;

/// Build the production router over `state`.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health::health))
        .route("/geocode", get(geocode::geocode))
        // `building_group_id` is path-percent-encoded by the client because
        // the canonical form contains slashes and pipes.
        .route(
            "/building/{building_group_id}/units",
            get(building::building_units),
        )
        .route("/reverse", get(reverse::reverse))
        // Lot E (v1 batch): body cap dialled up to 50 MiB on this route only.
        .route(
            "/batch/geocode",
            axum::routing::post(batch::batch_geocode).route_layer(batch::body_limit_layer()),
        )
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state)
}
