//! Discover BeSt zip files inside a `--best-dir` directory.
//!
//! Naming convention observed in the May 2026 dump (file 11 §1):
//!   `{Region}{Entity}{YYYYMMDD}[L72].zip`
//! where `L72` appears on Flanders + Wallonia and on Brussels' Address file
//! but not its smaller files (yes really — see file 11 §1).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    Brussels,
    Flanders,
    Wallonia,
}

impl Region {
    pub fn prefix(self) -> &'static str {
        match self {
            Region::Brussels => "Brussels",
            Region::Flanders => "Flanders",
            Region::Wallonia => "Wallonia",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entity {
    Municipality,
    PostalInfo,
    StreetName,
    Address,
    PartOfMunicipality,
}

impl Entity {
    pub fn token(self) -> &'static str {
        match self {
            Entity::Municipality => "Municipality",
            Entity::PostalInfo => "PostalInfo",
            Entity::StreetName => "StreetName",
            Entity::Address => "Address",
            Entity::PartOfMunicipality => "PartOfMunicipality",
        }
    }
}

/// Locate the zip file for a given (region, entity) under `best_dir`.
///
/// We don't insist on an exact name; we accept anything that starts with
/// `{Region}{Entity}` and ends with `.zip`. If multiple match, the lexically
/// largest is used (which equals the most recent date because the convention
/// embeds `YYYYMMDD`).
pub fn find_zip(best_dir: &Path, region: Region, entity: Entity) -> Result<PathBuf> {
    let needle_prefix = format!("{}{}", region.prefix(), entity.token());
    let mut candidates: Vec<PathBuf> = fs::read_dir(best_dir)
        .with_context(|| format!("reading {best_dir:?}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|s| s.to_str()) == Some("zip")
                && p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.starts_with(&needle_prefix))
        })
        .collect();

    candidates.sort();
    candidates
        .pop()
        .ok_or_else(|| anyhow!("no zip starting with {needle_prefix:?} found in {best_dir:?}"))
}

/// Scan `best_dir` and return the regions for which an Address zip is
/// present. Order is fixed (Brussels → Flanders → Wallonia) so the indexer
/// produces a deterministic run-order regardless of filesystem ordering.
///
/// Detection is presence-of-Address-zip — if Address is missing, we skip the
/// region even if the dictionary zips are there (we can't index addresses
/// without addresses). The per-entity zip lookup in `load_dictionaries` /
/// `stream_resolved_addresses` is what catches the case where a dict zip is
/// missing for an otherwise-present region.
pub fn detect_regions(best_dir: &Path) -> Result<Vec<Region>> {
    let mut out = Vec::new();
    for r in [Region::Brussels, Region::Flanders, Region::Wallonia] {
        if find_zip(best_dir, r, Entity::Address).is_ok() {
            out.push(r);
        }
    }
    if out.is_empty() {
        return Err(anyhow!(
            "no region Address zip found under {best_dir:?} — expected at least one of \
             BrusselsAddress*.zip / FlandersAddress*.zip / WalloniaAddress*.zip"
        ));
    }
    Ok(out)
}
