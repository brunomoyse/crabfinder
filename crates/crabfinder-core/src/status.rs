use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// BeSt address/street status: the four values of file 11 §5, plus
/// `rejected` (Brussels, ~1.4k addresses; v3 Phase 11.4 — earlier builds
/// misread it as `current`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Current,
    Proposed,
    Reserved,
    Retired,
    Rejected,
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
            Status::Rejected => "rejected",
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
            "rejected" => Ok(Status::Rejected),
            other => Err(CoreError::InvalidStatus(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_status() {
        assert_eq!(Status::from_str("current").unwrap(), Status::Current);
        assert_eq!(Status::from_str("proposed").unwrap(), Status::Proposed);
        assert_eq!(Status::from_str("reserved").unwrap(), Status::Reserved);
        assert_eq!(Status::from_str("retired").unwrap(), Status::Retired);
        assert_eq!(Status::from_str("rejected").unwrap(), Status::Rejected);
        assert_eq!(Status::Rejected.as_str(), "rejected");
    }

    #[test]
    fn default_filter_keeps_current_and_proposed() {
        assert!(Status::Current.should_index_by_default());
        assert!(Status::Proposed.should_index_by_default());
        assert!(!Status::Reserved.should_index_by_default());
        assert!(!Status::Retired.should_index_by_default());
        assert!(!Status::Rejected.should_index_by_default());
    }

    #[test]
    fn unknown_value_errors() {
        assert!(Status::from_str("active").is_err());
    }
}
