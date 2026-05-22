use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// BeSt address/street status. All four observed values from the schema.
/// See file 11 §5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Current,
    Proposed,
    Reserved,
    Retired,
}

impl Status {
    /// Default indexing filter: index `Current` and `Proposed` records.
    /// Rationale: Brussels marks 14% of addresses as `proposed`; excluding them
    /// drops too much real data. See file 11 §9.3.
    pub fn should_index_by_default(self) -> bool {
        matches!(self, Status::Current | Status::Proposed)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Current => "current",
            Status::Proposed => "proposed",
            Status::Reserved => "reserved",
            Status::Retired => "retired",
        }
    }
}

impl FromStr for Status {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "current" => Ok(Status::Current),
            "proposed" => Ok(Status::Proposed),
            "reserved" => Ok(Status::Reserved),
            "retired" => Ok(Status::Retired),
            other => Err(CoreError::InvalidStatus(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_four() {
        assert_eq!(Status::from_str("current").unwrap(), Status::Current);
        assert_eq!(Status::from_str("proposed").unwrap(), Status::Proposed);
        assert_eq!(Status::from_str("reserved").unwrap(), Status::Reserved);
        assert_eq!(Status::from_str("retired").unwrap(), Status::Retired);
    }

    #[test]
    fn default_filter_keeps_current_and_proposed() {
        assert!(Status::Current.should_index_by_default());
        assert!(Status::Proposed.should_index_by_default());
        assert!(!Status::Reserved.should_index_by_default());
        assert!(!Status::Retired.should_index_by_default());
    }

    #[test]
    fn unknown_value_errors() {
        assert!(Status::from_str("active").is_err());
    }
}
