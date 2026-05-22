use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// `<com:type>` on a StreetName record. File 11 §9.1: Wallonia has 118 hamlets
/// alongside 61,777 streetnames; both are loaded into the same dictionary and
/// referenced identically by Address records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreetType {
    Streetname,
    Hamlet,
}

impl FromStr for StreetType {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "streetname" => Ok(StreetType::Streetname),
            "hamlet" => Ok(StreetType::Hamlet),
            other => Err(CoreError::InvalidStreetType(other.to_string())),
        }
    }
}
