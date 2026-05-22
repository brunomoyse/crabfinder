use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid BeSt ID: {0}")]
    InvalidBestId(String),

    #[error("invalid region (unknown namespace): {0}")]
    UnknownRegion(String),

    #[error("invalid status: {0}")]
    InvalidStatus(String),

    #[error("invalid street type: {0}")]
    InvalidStreetType(String),

    #[error("invalid language: {0}")]
    InvalidLanguage(String),

    #[error("coordinate out of Belgian bounds: ({lat}, {lon})")]
    CoordOutOfBounds { lat: f64, lon: f64 },
}
