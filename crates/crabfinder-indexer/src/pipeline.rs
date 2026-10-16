//! Pass 1 + Pass 2 orchestration.
//!
//! Phase 1 walking skeleton: Brussels only, single-threaded, coordinates
//! still in Lambert 72 (Step 4 adds reprojection). The caller passes a
//! handler that decides what to do with each resolved `Address`.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use tracing::{info, warn};

use crabfinder_core::{
    Address, AddressId, BuildingGroupId, Coordinates, LambertCoord, MunicipalityId,
    PartOfMunicipalityId, PostalInfoId, Region, StreetId,
};

use crate::bundle::{Entity, Region as BundleRegion, find_zip};
use crate::dict::DictBundle;
use crate::reproject::Reprojector;
use crate::xml::{
    RawAddress, read_municipalities, read_parts_of_municipality, read_postal_infos, read_streets,
    stream_addresses,
};
use crate::zip_read::with_inner_xml;

/// Pass 1: load the four dictionaries for a single region. The Brussels and
/// Flanders bundles don't ship a PartOfMunicipality file; that dict stays
/// empty for those regions.
pub fn load_dictionaries(best_dir: &Path, region: BundleRegion) -> Result<DictBundle> {
    info!(region = ?region, "loading dictionaries");

    let municipality_zip = find_zip(best_dir, region, Entity::Municipality)?;
    let postal_zip = find_zip(best_dir, region, Entity::PostalInfo)?;
    let street_zip = find_zip(best_dir, region, Entity::StreetName)?;
    let pom_zip = if region == BundleRegion::Wallonia {
        Some(find_zip(best_dir, region, Entity::PartOfMunicipality)?)
    } else {
        None
    };

    info!(zip=?municipality_zip, "reading municipalities");
    let municipalities = with_inner_xml(&municipality_zip, |r| read_municipalities(r))
        .with_context(|| format!("parsing {municipality_zip:?}"))?;
    info!(count = municipalities.len(), "municipalities loaded");

    info!(zip=?postal_zip, "reading postal infos");
    let postal_infos = with_inner_xml(&postal_zip, |r| read_postal_infos(r))
        .with_context(|| format!("parsing {postal_zip:?}"))?;
    info!(count = postal_infos.len(), "postal infos loaded");

    info!(zip=?street_zip, "reading streets");
    let streets = with_inner_xml(&street_zip, |r| read_streets(r))
        .with_context(|| format!("parsing {street_zip:?}"))?;
    info!(count = streets.len(), "streets loaded");

    let parts = if let Some(p) = pom_zip {
        info!(zip=?p, "reading parts of municipality");
        let v = with_inner_xml(&p, |r| read_parts_of_municipality(r))
            .with_context(|| format!("parsing {p:?}"))?;
        info!(count = v.len(), "parts of municipality loaded");
        v
    } else {
        Vec::new()
    };

    let bundle = DictBundle::from_records(municipalities, postal_infos, streets, parts);
    info!(summary = %bundle.summary(), "dictionaries ready");
    Ok(bundle)
}

/// Map our internal `bundle::Region` (file discovery type) to the
/// `crabfinder_core::Region` used in the domain types.
fn to_core_region(r: BundleRegion) -> Region {
    match r {
        BundleRegion::Brussels => Region::Brussels,
        BundleRegion::Flanders => Region::Flanders,
        BundleRegion::Wallonia => Region::Wallonia,
    }
}

/// Pass 2: stream Address records, resolve references, invoke `handler` on
/// each resolved [`Address`].
///
/// Returns the count of (parsed, emitted, skipped) addresses. `parsed` =
/// total XML records read; `emitted` = passed status filter and resolved
/// successfully; `skipped_status` = filtered out by `Status::should_index_by_default`;
/// `skipped_unresolved` = at least one reference couldn't be resolved.
///
/// `handler` returns `Ok(true)` to continue, `Ok(false)` to stop early.
pub fn stream_resolved_addresses<F>(
    best_dir: &Path,
    region: BundleRegion,
    dicts: &DictBundle,
    limit: Option<usize>,
    mut handler: F,
) -> Result<StreamReport>
where
    F: FnMut(Address) -> Result<bool>,
{
    let address_zip = find_zip(best_dir, region, Entity::Address)?;
    let core_region = to_core_region(region);
    let reprojector = Reprojector::new()?;

    let mut report = StreamReport::default();

    with_inner_xml(&address_zip, |r| -> Result<()> {
        stream_addresses(r, |raw: RawAddress| -> Result<bool> {
            report.parsed += 1;

            // Status filter — drop Reserved + Retired by default (file 07 §5.3)
            if !raw.status.should_index_by_default() {
                report.skipped_status += 1;
                return Ok(true);
            }

            // Resolve references
            let street = dicts.streets.resolve(&raw.street_ref);
            let municipality = dicts.municipalities.resolve(&raw.municipality_ref);
            let postal = dicts.postal_infos.resolve(&raw.postal_info_ref);
            let pom = raw
                .part_of_municipality_ref
                .as_ref()
                .and_then(|p| dicts.parts_of_municipality.resolve(p));

            let (street, municipality, postal) = match (street, municipality, postal) {
                (Some(s), Some(m), Some(p)) => (s, m, p),
                _ => {
                    report.skipped_unresolved += 1;
                    warn!(
                        addr = %raw.id,
                        has_street = street.is_some(),
                        has_municipality = municipality.is_some(),
                        has_postal = postal.is_some(),
                        "skipping address with unresolved reference(s)"
                    );
                    return Ok(true);
                }
            };

            let address_id = AddressId::new(raw.id.clone());
            let street_id = StreetId::new(raw.street_ref.clone());
            let municipality_id = MunicipalityId::new(raw.municipality_ref.clone());
            let postal_info_id = PostalInfoId::new(raw.postal_info_ref.clone());
            let pom_id = raw
                .part_of_municipality_ref
                .clone()
                .map(PartOfMunicipalityId::new);

            let postal_code = postal.id.0.object_identifier.clone();
            let postal_name = postal.names.clone();

            let building_group_id = BuildingGroupId::from_parts(&street_id, &raw.house_number);

            let address = Address {
                id: address_id,
                street_id,
                municipality_id,
                postal_info_id,
                part_of_municipality_id: pom_id,

                region: core_region,
                status: raw.status,

                house_number: raw.house_number,
                box_number: raw.box_number,
                officially_assigned: raw.officially_assigned,

                street_name: street.names.clone(),
                street_type: street.street_type,
                municipality_name: municipality.names.clone(),
                postal_code,
                postal_name,
                part_of_municipality_name: pom.map(|p| p.names.clone()),
                // Attached later by the `--places` join (cli::run).
                position_source: Default::default(),

                // Anything that isn't a WGS84 point inside Belgium stays
                // Lambert 72: the caller imputes it (schema v9,
                // `impute.rs`). BeSt writes `0 0` for addresses with no
                // position yet (~140k in Wallonia).
                coordinates: {
                    let lc = LambertCoord {
                        easting: raw.easting,
                        northing: raw.northing,
                    };
                    match reprojector.lambert72_to_wgs84(lc) {
                        Ok(wgs) if wgs.is_in_belgium() => Coordinates::Wgs84(wgs),
                        Ok(_) => {
                            report.missing_position += 1;
                            Coordinates::Lambert72(lc)
                        }
                        Err(e) => {
                            warn!(error=%e, easting=lc.easting, northing=lc.northing,
                                  "reprojection failed; position will be imputed");
                            report.missing_position += 1;
                            Coordinates::Lambert72(lc)
                        }
                    }
                },

                building_group_id,
            };

            report.emitted += 1;

            let cont = handler(address)?;
            if !cont {
                return Ok(false);
            }
            if let Some(lim) = limit
                && report.emitted >= lim
            {
                return Ok(false);
            }
            Ok(true)
        })
        .map(|_| ())
    })?;

    Ok(report)
}

#[derive(Default, Debug, Clone, Copy)]
pub struct StreamReport {
    pub parsed: usize,
    pub emitted: usize,
    pub skipped_status: usize,
    pub skipped_unresolved: usize,
    /// Emitted without a usable position (left in Lambert 72 for the
    /// caller to impute).
    pub missing_position: usize,
}

impl std::fmt::Display for StreamReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "parsed={} emitted={} skipped_status={} skipped_unresolved={} missing_position={}",
            self.parsed,
            self.emitted,
            self.skipped_status,
            self.skipped_unresolved,
            self.missing_position
        )
    }
}

/// Re-export so the binary can mention this type without depending on the
/// internal `bundle` module.
pub use crate::bundle::Region as IngestRegion;

impl std::str::FromStr for IngestRegion {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "brussels" | "bru" => Ok(IngestRegion::Brussels),
            "flanders" | "vlg" => Ok(IngestRegion::Flanders),
            "wallonia" | "wal" => Ok(IngestRegion::Wallonia),
            other => Err(anyhow!("unknown region: {other}")),
        }
    }
}
