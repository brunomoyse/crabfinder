//! HTTP error type. Every error response is JSON: `{"error": "<message>"}`.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tracing::error;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("bad query: {0}")]
    BadQuery(String),
    #[error("internal: {0}")]
    Internal(anyhow::Error),
    /// Overloaded or too slow (the request guard's deadline): 503.
    #[error("unavailable: {0}")]
    Unavailable(String),
}

impl ServiceError {
    pub fn internal(context: &str, e: impl std::fmt::Display) -> Self {
        ServiceError::Internal(anyhow::anyhow!("{context}: {e}"))
    }
}

impl IntoResponse for ServiceError {
    fn into_response(self) -> Response {
        let (code, msg) = match &self {
            ServiceError::BadQuery(s) => (StatusCode::BAD_REQUEST, s.clone()),
            ServiceError::Unavailable(s) => (StatusCode::SERVICE_UNAVAILABLE, s.clone()),
            ServiceError::Internal(e) => {
                error!(error=%e, "internal");
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        };
        (code, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

impl From<anyhow::Error> for ServiceError {
    fn from(e: anyhow::Error) -> Self {
        ServiceError::Internal(e)
    }
}

/// `Query<T>` whose rejection is a JSON `ServiceError::BadQuery` instead of
/// axum's plain-text body, so every 4xx from the API has the same shape.
pub struct ApiQuery<T>(pub T);

impl<T, S> axum::extract::FromRequestParts<S> for ApiQuery<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ServiceError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        axum::extract::Query::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Query(v)| ApiQuery(v))
            .map_err(|e| ServiceError::BadQuery(e.body_text()))
    }
}
