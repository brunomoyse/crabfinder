//! Core domain types for crabfinder.
//!
//! Everything here is BeSt-shaped: types follow the published BeSt Address
//! schema. No I/O, no Tantivy, no HTTP — just the data model and pure helpers.

pub mod address;
pub mod coordinates;
pub mod error;
pub mod house_number;
pub mod ids;
pub mod multilingual;
pub mod position;
pub mod region;
pub mod status;
pub mod street_type;

pub use address::{Address, Municipality, PartOfMunicipality, PostalInfo, Street};
pub use coordinates::{Coordinates, LambertCoord, WgsCoord};
pub use error::CoreError;
pub use ids::{
    AddressId, BestId, BuildingGroupId, MunicipalityId, PartOfMunicipalityId, PostalInfoId,
    StreetId,
};
pub use multilingual::{Language, MultilingualName};
pub use position::PositionSource;
pub use region::Region;
pub use status::Status;
pub use street_type::StreetType;
