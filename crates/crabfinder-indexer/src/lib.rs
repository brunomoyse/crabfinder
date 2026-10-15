//! crabfinder-indexer — BeSt XML → Tantivy pipeline.
//!
//! Two-pass indexing (see `docs/architecture.md`):
//! 1. Load Municipality / PostalInfo / StreetName / PartOfMunicipality into in-memory dictionaries.
//! 2. Stream the Address file and resolve references against the dictionaries.

pub mod bundle;
pub mod cli;
pub mod dict;
pub mod impute;
pub mod index;
pub mod localities;
pub mod meta;
pub mod mid_street;
pub mod notability;
pub mod pipeline;
pub mod places;
pub mod reproject;
pub mod xml;
pub mod zip_read;
