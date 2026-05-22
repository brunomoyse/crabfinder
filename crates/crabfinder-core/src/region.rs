use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// The three Belgian regions. Codes follow ISO 3166-2:BE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Region {
    Brussels,
    Flanders,
    Wallonia,
}

impl Region {
    /// ISO 3166-2:BE code as used in API responses and Tantivy index fields.
    pub fn iso_code(self) -> &'static str {
        match self {
            Region::Brussels => "BE-BRU",
            Region::Flanders => "BE-VLG",
            Region::Wallonia => "BE-WAL",
        }
    }

    /// Derive the region from a BeSt namespace string. See file 11 §2 for the
    /// per-region namespace inventory.
    pub fn from_namespace(ns: &str) -> Result<Self, CoreError> {
        let n = ns.trim_end_matches('/');
        if n.starts_with("https://databrussels.be") {
            Ok(Region::Brussels)
        } else if n.starts_with("https://data.vlaanderen.be") {
            Ok(Region::Flanders)
        } else if n.starts_with("geodata.wallonie.be") {
            Ok(Region::Wallonia)
        } else {
            Err(CoreError::UnknownRegion(ns.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_codes() {
        assert_eq!(Region::Brussels.iso_code(), "BE-BRU");
        assert_eq!(Region::Flanders.iso_code(), "BE-VLG");
        assert_eq!(Region::Wallonia.iso_code(), "BE-WAL");
    }

    #[test]
    fn from_namespace_each_region() {
        // Brussels — no trailing slash, https
        assert_eq!(
            Region::from_namespace("https://databrussels.be/id/address").unwrap(),
            Region::Brussels
        );
        // Flanders — with trailing slash, https, NL terms
        assert_eq!(
            Region::from_namespace("https://data.vlaanderen.be/id/adres/").unwrap(),
            Region::Flanders
        );
        // Wallonia — no scheme, mixed case
        assert_eq!(
            Region::from_namespace("geodata.wallonie.be/id/Address").unwrap(),
            Region::Wallonia
        );
    }

    #[test]
    fn unknown_namespace_errors() {
        assert!(Region::from_namespace("https://example.org/x").is_err());
    }
}
