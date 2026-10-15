use serde::{Deserialize, Serialize};

use crate::coordinates::Coordinates;
use crate::ids::{
    AddressId, BuildingGroupId, MunicipalityId, PartOfMunicipalityId, PostalInfoId, StreetId,
};
use crate::multilingual::MultilingualName;
use crate::region::Region;
use crate::status::Status;
use crate::street_type::StreetType;

// --- Dictionary entry types -------------------------------------------------
// These are the *denormalised* dictionary value types — what the indexer loads
// in Pass 1 and looks up during Pass 2. Raw XML record types live next to the
// XML readers, in crabfinder-indexer.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Municipality {
    pub id: MunicipalityId,
    pub names: MultilingualName,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Street {
    pub id: StreetId,
    pub names: MultilingualName,
    pub street_type: StreetType,
    pub status: Status,
    pub assigned_by_municipality: MunicipalityId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostalInfo {
    pub id: PostalInfoId,
    /// Empty for Brussels and Wallonia (per file 11 §4.2 only Flanders publishes
    /// postal-area names). The 4-digit postal code lives in
    /// `id.0.object_identifier`.
    pub names: MultilingualName,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartOfMunicipality {
    pub id: PartOfMunicipalityId,
    pub names: MultilingualName,
}

// --- The denormalised address record ---------------------------------------

/// A single addressable point — what gets stored as one Tantivy document.
///
/// One Tantivy doc per BeSt Address XML record (file 07 §5.2). All references
/// resolved against the dictionaries; coordinates already reprojected to WGS84.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Address {
    pub id: AddressId,
    pub street_id: StreetId,
    pub municipality_id: MunicipalityId,
    pub postal_info_id: PostalInfoId,
    /// Wallonia only — see file 11 §9.3 (100% present for Wallonia, absent
    /// elsewhere).
    pub part_of_municipality_id: Option<PartOfMunicipalityId>,

    pub region: Region,
    pub status: Status,

    pub house_number: String,
    /// Optional. Brussels: ~100% populated with textual designations
    /// (`RDC`, `2eET`, …). Flanders: 0%. Wallonia: ~20% (structured codes
    /// `B001`, `C003`). See file 11 §9.3.
    pub box_number: Option<String>,

    /// `<com:officiallyAssigned>` — see file 07 §5.4 for the assertion strategy.
    /// Defaults to true; only `false` if the source XML says so explicitly.
    pub officially_assigned: bool,

    // Resolved names (denormalised from the dictionaries) — included in the
    // stored payload so the service can return them in /geocode responses
    // without an extra lookup.
    pub street_name: MultilingualName,
    pub street_type: StreetType,
    pub municipality_name: MultilingualName,
    pub postal_code: String,
    pub postal_name: MultilingualName,
    pub part_of_municipality_name: Option<MultilingualName>,

    /// Reprojected to WGS84 at index time.
    pub coordinates: Coordinates,

    pub building_group_id: BuildingGroupId,

    /// Nearest OpenStreetMap place (hamlet, village, …) when it adds a name
    /// the BeSt hierarchy doesn't carry (schema v8, `--places`). Absent in
    /// payloads written before v8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place_name: Option<MultilingualName>,
}
