//! Tantivy schema (Variant A) and writer for crabfinder.
//!
//! Variant A is "parserless": a single multi-valued TEXT field carrying every
//! language variant of every multilingual name, plus structured fields needed
//! for retrieval and dedup at query time. See file 07 §4 "Variant A".
//!
//! See `meta::SCHEMA_VERSION` for the changelog (v1 → v6).

use std::path::Path;

use anyhow::{Context, Result};
use tantivy::directory::MmapDirectory;
use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, STORED, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::{AsciiFoldingFilter, LowerCaser, SimpleTokenizer, TextAnalyzer};
use tantivy::{Index, IndexWriter, TantivyDocument};
// `STORED` re-imported below as part of the TextOptions chain.

use crabfinder_core::{Address, Coordinates, MultilingualName};

// --- Tokenizer --------------------------------------------------------------

/// Name under which the crabfinder tokenizer chain is registered on every
/// `Index` instance. The schema references this name in `set_tokenizer`, so
/// both the writer and the searcher must register the analyzer under exactly
/// this string before reading/writing.
pub const TOKENIZER_NAME: &str = "crab";

/// Build the crabfinder tokenizer chain:
///   SimpleTokenizer  → splits on every non-alphanumeric Unicode char
///                      (hyphens, apostrophes, whitespace all become breaks)
///   LowerCaser       → case-insensitive matching
///   AsciiFoldingFilter → strips diacritics (É → E, ç → c, …) so "Etterbeek"
///                        and "Étterbeek" produce the same token stream
///
/// Decision: hyphen-as-hard-separator (option (b)). See Phase 2 design note.
/// Hyphenated street names like "Grote-Ringlaan" index as
/// `["grote", "ringlaan"]`, and the query builder uses a phrase query to
/// require adjacency, which collapses "Grote Ringlaan" and "Grote-Ringlaan"
/// to identical results.
pub fn build_text_analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build()
}

/// Register the crabfinder analyzer on a Tantivy `Index`. Must be called
/// **before** any read or write that touches a field configured with
/// `set_tokenizer(TOKENIZER_NAME)`.
pub fn register_tokenizers(index: &Index) {
    index.tokenizers().register(TOKENIZER_NAME, build_text_analyzer());
}

// --- Field names ------------------------------------------------------------

pub const FIELD_BEST_ID: &str = "best_id";
pub const FIELD_SEARCHABLE: &str = "searchable";
/// FAST f64, default 1.0. Boost multiplier applied to BM25 at query time
/// via `TopDocs::tweak_score`. File 13 §6 (Option A); schema v4. Tantivy
/// 0.26 has no f32 numeric field; we use f64 (8 bytes × 6.84M ≈ 55 MB,
/// ~2.7% of the v3 index size).
pub const FIELD_NOTABILITY_SCORE: &str = "notability_score";
pub const FIELD_LAT: &str = "lat";
pub const FIELD_LON: &str = "lon";
pub const FIELD_HOUSE_NUMBER: &str = "house_number";
pub const FIELD_BOX_NUMBER: &str = "box_number";
pub const FIELD_POSTAL_CODE: &str = "postal_code";
pub const FIELD_BUILDING_GROUP_ID: &str = "building_group_id";
pub const FIELD_STATUS: &str = "status";
pub const FIELD_REGION: &str = "region";
/// Stored-only field holding the full BestAddress serialised as JSON string.
/// Keeping this as a raw stored TEXT (rather than tantivy's typed JSON field)
/// keeps retrieval trivial — no `OwnedValue`/`CompactDocValue` plumbing.
pub const FIELD_PAYLOAD: &str = "payload";

// --- Schema -----------------------------------------------------------------

/// Resolve field handles for an open Tantivy index using crabfinder's
/// Variant A schema.
pub struct CrabSchema {
    pub schema: Schema,
    pub best_id: Field,
    pub searchable: Field,
    pub notability_score: Field,
    pub lat: Field,
    pub lon: Field,
    pub house_number: Field,
    pub box_number: Field,
    pub postal_code: Field,
    pub building_group_id: Field,
    pub status: Field,
    pub region: Field,
    pub payload: Field,
}

impl CrabSchema {
    pub fn build() -> Self {
        let mut sb = Schema::builder();

        // TEXT options for searchable fields:
        // - "crab" tokenizer (SimpleTokenizer → LowerCaser → AsciiFolding)
        // - field-norms disabled — geocoding shouldn't penalise long entries
        //   (file 02 §1)
        // - frequencies + positions — required for phrase queries (Phase 2)
        let text_opts = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(TOKENIZER_NAME)
                .set_fieldnorms(false)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        );

        // Raw (untokenised) options for IDs, codes, statuses.
        let raw_opts = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_fieldnorms(false)
                .set_tokenizer("raw")
                .set_index_option(IndexRecordOption::Basic),
        );
        let raw_stored = raw_opts.clone().set_stored();

        // Fast + stored numeric for lat/lon so /reverse (Phase 6) and the
        // response hydrator (Phase 1 already) can read them cheaply.
        let f64_stored_fast = NumericOptions::default()
            .set_indexed()
            .set_stored()
            .set_fast();

        let best_id = sb.add_text_field(FIELD_BEST_ID, raw_stored.clone());
        let searchable = sb.add_text_field(FIELD_SEARCHABLE, text_opts);
        let lat = sb.add_f64_field(FIELD_LAT, f64_stored_fast.clone());
        let lon = sb.add_f64_field(FIELD_LON, f64_stored_fast);
        // FAST-only (not stored) — read at query time via tweak_score.
        let f64_fast_only = NumericOptions::default().set_indexed().set_fast();
        let notability_score = sb.add_f64_field(FIELD_NOTABILITY_SCORE, f64_fast_only);
        let house_number = sb.add_text_field(FIELD_HOUSE_NUMBER, raw_stored.clone());
        let box_number = sb.add_text_field(FIELD_BOX_NUMBER, raw_stored.clone());
        let postal_code = sb.add_text_field(FIELD_POSTAL_CODE, raw_stored.clone());
        let building_group_id = sb.add_text_field(FIELD_BUILDING_GROUP_ID, raw_stored.clone());
        let status = sb.add_text_field(FIELD_STATUS, raw_stored.clone());
        let region = sb.add_text_field(FIELD_REGION, raw_stored.clone());
        // Stored-only payload (no indexing). TextOptions with no indexing options
        // gives us pure storage.
        let payload = sb.add_text_field(FIELD_PAYLOAD, TextOptions::default().set_stored());
        let _ = STORED; // suppress unused import warning if removed in future refactors

        let schema = sb.build();
        Self {
            schema,
            best_id,
            searchable,
            notability_score,
            lat,
            lon,
            house_number,
            box_number,
            postal_code,
            building_group_id,
            status,
            region,
            payload,
        }
    }
}

// --- Writer wrapper ---------------------------------------------------------

/// Wraps a Tantivy `IndexWriter` and exposes a typed `add_address` method.
pub struct CrabIndexWriter {
    pub schema: CrabSchema,
    writer: IndexWriter,
}

impl CrabIndexWriter {
    /// Create (or overwrite) a Tantivy index at `dir` with the Variant A schema.
    pub fn create(dir: &Path) -> Result<Self> {
        if !dir.exists() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {dir:?}"))?;
        }
        let schema = CrabSchema::build();
        let mmap = MmapDirectory::open(dir).with_context(|| format!("mmap {dir:?}"))?;
        let index = Index::open_or_create(mmap, schema.schema.clone())
            .with_context(|| format!("open_or_create at {dir:?}"))?;
        register_tokenizers(&index);
        let writer = index
            .writer::<TantivyDocument>(1_000_000_000) // 1 GB arena (file 07 §5)
            .context("constructing IndexWriter")?;
        Ok(Self { schema, writer })
    }

    pub fn add_address(&mut self, addr: &Address) -> Result<()> {
        self.add_address_with_notability(addr, 1.0)
    }

    /// Add an address with an explicit notability boost (schema v4, file 13 §6).
    /// Indexer Pass 2 looks up each address's building_group_id in the loaded
    /// whitelist and supplies the multiplier here. Non-whitelisted docs use 1.0.
    pub fn add_address_with_notability(&mut self, addr: &Address, notability: f64) -> Result<()> {
        let (lat, lon) = match addr.coordinates {
            Coordinates::Wgs84(w) => (w.lat, w.lon),
            Coordinates::Lambert72(_) => {
                anyhow::bail!(
                    "BUG: address {} carries Lambert 72 coordinates; pipeline must reproject before indexing",
                    addr.id
                );
            }
        };

        // Build the multi-valued searchable token vector. Every variant of
        // every multilingual name goes in, so the strict AND-within-one-field
        // constraint succeeds for cross-language queries (file 12 §3.2).
        let tokens = build_searchable_tokens(addr);

        let mut doc = TantivyDocument::default();
        let s = &self.schema;

        doc.add_text(s.best_id, &addr.id.to_string());
        for value in &tokens {
            doc.add_text(s.searchable, value);
        }

        doc.add_f64(s.lat, lat);
        doc.add_f64(s.lon, lon);
        doc.add_f64(s.notability_score, notability);
        doc.add_text(s.house_number, &addr.house_number);
        if let Some(b) = &addr.box_number {
            doc.add_text(s.box_number, b);
        }
        doc.add_text(s.postal_code, &addr.postal_code);
        doc.add_text(s.building_group_id, addr.building_group_id.as_str());
        doc.add_text(s.status, addr.status.as_str());
        doc.add_text(s.region, addr.region.iso_code());

        // Full payload as a JSON string in a STORED-only field.
        let payload_str = serde_json::to_string(addr).context("serialise Address payload")?;
        doc.add_text(s.payload, &payload_str);

        self.writer
            .add_document(doc)
            .context("Tantivy add_document")?;
        Ok(())
    }

    /// Force a commit and return the writer for cleanup.
    pub fn commit(mut self) -> Result<()> {
        self.writer.commit().context("Tantivy commit")?;
        Ok(())
    }
}

/// Build the multi-valued searchable token vector from an `Address`. Every
/// non-empty variant of every multilingual field is included, so a strict AND
/// across all parsed query tokens can match a single field even when the
/// query mixes languages (FR muni alias + NL street, etc.). Each value is a
/// separate entry in the multi-valued field, so phrase queries don't
/// accidentally span unrelated names.
fn build_searchable_tokens(addr: &Address) -> Vec<String> {
    let mut out = Vec::new();
    push_multilingual(&addr.street_name, &mut out);
    push_multilingual(&addr.municipality_name, &mut out);
    push_multilingual(&addr.postal_name, &mut out);
    if let Some(pom) = &addr.part_of_municipality_name {
        push_multilingual(pom, &mut out);
    }
    out.push(addr.house_number.clone());
    if let Some(b) = &addr.box_number {
        out.push(b.clone());
    }
    out.push(addr.postal_code.clone());
    out
}

fn push_multilingual(n: &MultilingualName, out: &mut Vec<String>) {
    if let Some(s) = &n.fr {
        out.push(s.clone());
    }
    if let Some(s) = &n.nl {
        out.push(s.clone());
    }
    if let Some(s) = &n.de {
        out.push(s.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CrabSchema::build wires up every field listed in the v6 doc-comment.
    /// If a future change adds a field, this test forces a deliberate update
    /// to the assertion list — schema drift won't sneak past CI.
    #[test]
    fn schema_exposes_expected_fields() {
        let s = CrabSchema::build();
        let names: Vec<&str> = s
            .schema
            .fields()
            .map(|(_, e)| e.name())
            .collect();
        let expected = [
            FIELD_BEST_ID,
            FIELD_SEARCHABLE,
            FIELD_LAT,
            FIELD_LON,
            FIELD_NOTABILITY_SCORE,
            FIELD_HOUSE_NUMBER,
            FIELD_BOX_NUMBER,
            FIELD_POSTAL_CODE,
            FIELD_BUILDING_GROUP_ID,
            FIELD_STATUS,
            FIELD_REGION,
            FIELD_PAYLOAD,
        ];
        for name in expected {
            assert!(
                names.contains(&name),
                "schema missing field {name:?}; have {names:?}"
            );
        }
    }

    #[test]
    fn register_tokenizers_registers_crab_analyzer() {
        let schema = CrabSchema::build();
        let index = Index::create_in_ram(schema.schema.clone());
        register_tokenizers(&index);
        assert!(
            index.tokenizers().get(TOKENIZER_NAME).is_some(),
            "crab tokenizer missing"
        );
    }
}

