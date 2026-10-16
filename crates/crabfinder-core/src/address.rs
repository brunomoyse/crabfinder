use serde::{Deserialize, Serialize};

use crate::coordinates::Coordinates;
use crate::ids::{
    AddressId, BuildingGroupId, MunicipalityId, PartOfMunicipalityId, PostalInfoId, StreetId,
};
use crate::multilingual::{Language, MultilingualName};
use crate::position::PositionSource;
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

    /// How `coordinates` were obtained (schema v9). Omitted from payloads
    /// when `Measured`, so older payloads read back as measured.
    #[serde(default, skip_serializing_if = "PositionSource::is_measured")]
    pub position_source: PositionSource,
}

impl Address {
    /// The language an address is written in by default: French in
    /// Brussels and Wallonia, Dutch in Flanders, German in the
    /// German-speaking Community (postal codes 4700–4791).
    pub fn default_language(&self) -> Language {
        match self.region {
            Region::Flanders => Language::Nl,
            Region::Brussels => Language::Fr,
            Region::Wallonia => match self.postal_code.parse::<u32>() {
                Ok(4700..=4791) => Language::De,
                _ => Language::Fr,
            },
        }
    }

    /// One-line postal label, e.g. `"Rue de la Loi 16 bte 3, 1000
    /// Bruxelles"` or `"Wetstraat 16 bus 3, 1000 Brussel"`.
    ///
    /// Each name is taken in `lang` when BeSt has it, else in the
    /// address's `default_language`, else in any language. The place is
    /// the postal name (what bpost prints after the code), falling back
    /// to the municipality.
    pub fn display_name(&self, lang: Option<Language>) -> String {
        let fallback = self.default_language();
        let pick = |n: &MultilingualName| -> Option<String> {
            lang.and_then(|l| n.get(l))
                .or_else(|| n.get(fallback))
                .or_else(|| n.primary())
                .map(str::to_string)
        };
        let label_lang = lang.unwrap_or(fallback);
        let mut out = pick(&self.street_name).unwrap_or_default();
        if !self.house_number.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&self.house_number);
        }
        if let Some(b) = self.box_number.as_deref().filter(|b| !b.is_empty()) {
            let kw = match label_lang {
                Language::Fr => "bte",
                Language::Nl => "bus",
                Language::De => "Bfk.",
            };
            out.push_str(&format!(" {kw} {b}"));
        }
        let place = pick(&self.postal_name).or_else(|| pick(&self.municipality_name));
        let locality = match place {
            Some(p) => format!("{} {p}", self.postal_code),
            None => self.postal_code.clone(),
        };
        if out.is_empty() {
            locality
        } else {
            format!("{out}, {locality}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinates::WgsCoord;
    use crate::ids::BestId;

    fn ml(fr: Option<&str>, nl: Option<&str>, de: Option<&str>) -> MultilingualName {
        MultilingualName {
            fr: fr.map(Into::into),
            nl: nl.map(Into::into),
            de: de.map(Into::into),
        }
    }

    fn addr(region: Region, postal: &str) -> Address {
        let street_id = StreetId::new(BestId::new("ns", "1", "1"));
        Address {
            id: AddressId::new(BestId::new("ns", "a", "1")),
            building_group_id: BuildingGroupId::from_parts(&street_id, "16"),
            street_id,
            municipality_id: MunicipalityId::new(BestId::new("ns", "m", "1")),
            postal_info_id: PostalInfoId::new(BestId::new("ns", "p", "1")),
            part_of_municipality_id: None,
            region,
            status: Status::Current,
            house_number: "16".into(),
            box_number: None,
            officially_assigned: true,
            street_name: ml(Some("Rue de la Loi"), Some("Wetstraat"), None),
            street_type: StreetType::Streetname,
            municipality_name: ml(Some("Bruxelles"), Some("Brussel"), None),
            postal_code: postal.into(),
            postal_name: MultilingualName::default(),
            part_of_municipality_name: None,
            coordinates: Coordinates::Wgs84(WgsCoord {
                lat: 50.8,
                lon: 4.3,
            }),
            position_source: PositionSource::Measured,
        }
    }

    #[test]
    fn default_language_by_region_and_postal_code() {
        assert_eq!(
            addr(Region::Brussels, "1000").default_language(),
            Language::Fr
        );
        assert_eq!(
            addr(Region::Flanders, "2000").default_language(),
            Language::Nl
        );
        assert_eq!(
            addr(Region::Wallonia, "5000").default_language(),
            Language::Fr
        );
        assert_eq!(
            addr(Region::Wallonia, "4700").default_language(),
            Language::De
        );
        assert_eq!(
            addr(Region::Wallonia, "4791").default_language(),
            Language::De
        );
        assert_eq!(
            addr(Region::Wallonia, "4800").default_language(),
            Language::Fr
        );
        assert_eq!(
            addr(Region::Wallonia, "????").default_language(),
            Language::Fr
        );
    }

    #[test]
    fn display_name_in_each_language_with_box() {
        let mut a = addr(Region::Brussels, "1000");
        assert_eq!(a.display_name(None), "Rue de la Loi 16, 1000 Bruxelles");
        assert_eq!(
            a.display_name(Some(Language::Nl)),
            "Wetstraat 16, 1000 Brussel"
        );
        a.box_number = Some("3".into());
        assert_eq!(
            a.display_name(Some(Language::Fr)),
            "Rue de la Loi 16 bte 3, 1000 Bruxelles"
        );
        assert_eq!(
            a.display_name(Some(Language::Nl)),
            "Wetstraat 16 bus 3, 1000 Brussel"
        );
        // No German names: each name falls back to French, the box
        // keyword follows the requested language.
        assert_eq!(
            a.display_name(Some(Language::De)),
            "Rue de la Loi 16 Bfk. 3, 1000 Bruxelles"
        );
        a.box_number = Some(String::new());
        assert_eq!(a.display_name(None), "Rue de la Loi 16, 1000 Bruxelles");
    }

    #[test]
    fn display_name_prefers_the_postal_name_and_falls_back() {
        let mut a = addr(Region::Flanders, "1050");
        a.street_name = ml(None, Some("Kerkstraat"), None);
        a.postal_name = ml(Some("Ixelles"), Some("Elsene"), None);
        assert_eq!(a.display_name(None), "Kerkstraat 16, 1050 Elsene");
        assert_eq!(
            a.display_name(Some(Language::Fr)),
            "Kerkstraat 16, 1050 Ixelles"
        );
        // German asked, none present: the region default (Dutch).
        assert_eq!(
            a.display_name(Some(Language::De)),
            "Kerkstraat 16, 1050 Elsene"
        );

        a.municipality_name = MultilingualName::default();
        a.postal_name = MultilingualName::default();
        assert_eq!(a.display_name(None), "Kerkstraat 16, 1050");
        a.street_name = MultilingualName::default();
        assert_eq!(a.display_name(None), "16, 1050");
        a.house_number = String::new();
        assert_eq!(a.display_name(None), "1050");
        a.street_name = ml(None, None, Some("Hauptstraße"));
        assert_eq!(a.display_name(None), "Hauptstraße, 1050");
    }
}
