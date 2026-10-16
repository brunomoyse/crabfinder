use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::region::Region;

/// The BeSt identifier triple. Every entity (Municipality, Street, Address, …)
/// is keyed by `(namespace, objectIdentifier, versionIdentifier)`. See file
/// 11 §2.
///
/// Per file 11 §7.2 the canonical-form string is
/// `{namespace}/{objectIdentifier}/{versionIdentifier}` (matching the official
/// `/id/{best_id}` API style). We store the three parts as separate fields and
/// expose canonical form via `Display`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BestId {
    pub namespace: String,
    pub object_identifier: String,
    pub version_identifier: String,
}

impl BestId {
    pub fn new(
        namespace: impl Into<String>,
        object_identifier: impl Into<String>,
        version_identifier: impl Into<String>,
    ) -> Self {
        Self {
            namespace: namespace.into(),
            object_identifier: object_identifier.into(),
            version_identifier: version_identifier.into(),
        }
    }

    /// Returns the `(namespace, object_identifier)` pair — the dictionary join
    /// key per file 11 §9.2. Versions are tracked separately in the dictionary
    /// values.
    pub fn join_key(&self) -> (&str, &str) {
        (&self.namespace, &self.object_identifier)
    }

    /// `{namespace}/{objectIdentifier}`: the id of the object across all its
    /// versions (schema v12 `best_oid`, v3 Phase 11.4).
    pub fn object_key(&self) -> String {
        format!(
            "{}/{}",
            self.namespace.trim_end_matches('/'),
            self.object_identifier
        )
    }

    /// Returns the owning [`Region`] derived from this ID's namespace.
    pub fn region(&self) -> Result<Region, CoreError> {
        Region::from_namespace(&self.namespace)
    }
}

impl fmt::Display for BestId {
    /// `{namespace}/{objectIdentifier}/{versionIdentifier}` canonical form.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ns = self.namespace.trim_end_matches('/');
        write!(
            f,
            "{}/{}/{}",
            ns, self.object_identifier, self.version_identifier
        )
    }
}

// --- Typed wrappers ---------------------------------------------------------
// Distinct types prevent mixing up IDs of different entity kinds at compile
// time (e.g. passing an AddressId where a StreetId is expected). Each is a
// thin newtype over [`BestId`].

macro_rules! typed_best_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub BestId);

        impl $name {
            pub fn new(b: BestId) -> Self {
                Self(b)
            }
            pub fn inner(&self) -> &BestId {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

typed_best_id!(AddressId, "BeSt ID for an Address record.");
typed_best_id!(
    StreetId,
    "BeSt ID for a StreetName record (street or hamlet)."
);
typed_best_id!(MunicipalityId, "BeSt ID for a Municipality record.");
typed_best_id!(PostalInfoId, "BeSt ID for a PostalInfo record.");
typed_best_id!(
    PartOfMunicipalityId,
    "BeSt ID for a PartOfMunicipality record (Wallonia only)."
);

// --- BuildingGroupId --------------------------------------------------------

/// Synthetic identifier used to dedupe per-unit Brussels records at query time.
/// See file 07 §5.2.
///
/// Format: `"{street_namespace}|{street_object_id}|{house_number}"`. Pipe
/// separator chosen because colons appear inside BeSt namespaces (URLs).
/// We deliberately omit the version identifier and the box number so that all
/// units of one physical building share the same `BuildingGroupId`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BuildingGroupId(pub String);

impl BuildingGroupId {
    pub fn from_parts(street: &StreetId, house_number: &str) -> Self {
        Self(format!(
            "{}|{}|{}",
            street.0.namespace, street.0.object_identifier, house_number
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BuildingGroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_addr() -> BestId {
        BestId::new("https://databrussels.be/id/address", "615867", "1")
    }

    #[test]
    fn canonical_form() {
        let b = sample_addr();
        assert_eq!(b.to_string(), "https://databrussels.be/id/address/615867/1");
    }

    #[test]
    fn canonical_form_strips_trailing_slash() {
        let b = BestId::new(
            "https://data.vlaanderen.be/id/adres/",
            "200001",
            "2023-11-01",
        );
        // The namespace has a trailing slash; the canonical form should not double up.
        assert_eq!(
            b.to_string(),
            "https://data.vlaanderen.be/id/adres/200001/2023-11-01"
        );
    }

    #[test]
    fn object_key_drops_version_and_trailing_slash() {
        assert_eq!(
            sample_addr().object_key(),
            "https://databrussels.be/id/address/615867"
        );
        let b = BestId::new("https://data.vlaanderen.be/id/adres/", "200068", "2023");
        assert_eq!(b.object_key(), "https://data.vlaanderen.be/id/adres/200068");
    }

    #[test]
    fn join_key_excludes_version() {
        let b = sample_addr();
        assert_eq!(
            b.join_key(),
            ("https://databrussels.be/id/address", "615867")
        );
    }

    #[test]
    fn region_derives_from_namespace() {
        assert_eq!(sample_addr().region().unwrap(), Region::Brussels);
        assert_eq!(
            BestId::new("https://data.vlaanderen.be/id/adres/", "1", "1")
                .region()
                .unwrap(),
            Region::Flanders
        );
        assert_eq!(
            BestId::new("geodata.wallonie.be/id/Address", "1", "1")
                .region()
                .unwrap(),
            Region::Wallonia
        );
    }

    #[test]
    fn building_group_id_is_stable() {
        let s = StreetId(BestId::new(
            "https://databrussels.be/id/streetname",
            "4568",
            "1",
        ));
        let g1 = BuildingGroupId::from_parts(&s, "475");
        let g2 = BuildingGroupId::from_parts(&s, "475");
        assert_eq!(g1, g2);
        assert_eq!(
            g1.as_str(),
            "https://databrussels.be/id/streetname|4568|475"
        );
    }

    #[test]
    fn building_group_id_handles_alpha_house_numbers() {
        let s = StreetId(BestId::new(
            "https://databrussels.be/id/streetname",
            "4568",
            "1",
        ));
        let g = BuildingGroupId::from_parts(&s, "12A");
        assert!(g.as_str().ends_with("|12A"));
    }
}
