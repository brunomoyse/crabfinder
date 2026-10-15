//! `GET /geocode` — free text → ranked address features.

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crabfinder_core::MultilingualName;
use crabfinder_indexer::localities::Locality;
use crabfinder_parser::ParsedQuery;

use crate::AppState;
use crate::dedup::{MatchType, RAW_HIT_CAP};
use crate::focus::parse_focus_point;
use crate::resolver::{AddressQuery, SearchOptions, Tier, search};

use super::blocking;
use super::error::{ApiQuery, ServiceError};
use super::feature::{Feature, parse_lang};

#[derive(Debug, Deserialize)]
pub struct GeocodeParams {
    /// Free text. Exclusive with the structured fields below.
    #[serde(default)]
    pub q: Option<String>,
    /// Structured input (v2 Phase 6), exclusive with `q`: street name…
    #[serde(default)]
    pub street: Option<String>,
    /// …house number (`12`, `12A`)…
    #[serde(default)]
    pub housenumber: Option<String>,
    /// …box (`3`, `B001`); not a filter, it picks the unit…
    #[serde(default, rename = "box")]
    pub box_number: Option<String>,
    /// …4-digit postal code (a hard filter)…
    #[serde(default)]
    pub postcode: Option<String>,
    /// …and municipality or locality name, matched like street words.
    #[serde(default)]
    pub city: Option<String>,
    /// Language of `properties.label`: `fr`, `nl` or `de`.
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// When true, return one feature per BeSt address row. When false
    /// (default), collapse rows sharing a `building_group_id` into one
    /// feature with `unit_count` ≥ 1.
    #[serde(default)]
    pub expand: bool,
    /// Hard filter on `region`: ISO codes (`BE-BRU`, `BE-VLG`, `BE-WAL`) or
    /// friendly aliases (`brussels`, `flanders`, `wallonia`, …).
    #[serde(default)]
    pub region: Option<String>,
    /// Hard filter on `postal_code`. Overrides any postal code parsed out
    /// of `q`.
    #[serde(default)]
    pub postal_code: Option<String>,
    /// Optional `"lat,lng"` hint; scores decay exponentially with distance.
    #[serde(default)]
    pub focus_point: Option<String>,
}

fn default_limit() -> usize {
    10
}

/// Normalise a user-supplied region string to the indexed ISO form
/// (`BE-BRU` / `BE-VLG` / `BE-WAL`). `None` if the input doesn't resolve.
pub fn normalize_region_filter(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "be-bru" | "brussels" | "brussel" | "bruxelles" | "bru" => Some("BE-BRU"),
        "be-vlg" | "flanders" | "vlaanderen" | "flandre" | "vlg" => Some("BE-VLG"),
        "be-wal" | "wallonia" | "wallonie" | "wal" => Some("BE-WAL"),
        _ => None,
    }
}

#[derive(Serialize)]
pub struct FeatureCollection {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub query: GeocodeEcho,
    pub count: usize,
    /// The two best features lie on different streets with nearly equal
    /// scores (`CandidateSet::ambiguity`): the query doesn't say which.
    pub ambiguous: bool,
    /// `1 − score₂/score₁` between the two best features; absent with
    /// fewer than two, or when scores aren't text relevance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_gap: Option<f32>,
    /// "strict" if any returned building had a strict-matching member,
    /// "fuzzy" if every one came from the typo or OR-token tiers.
    pub match_type: &'static str,
    /// False when `?expand=true` was set.
    pub deduplicated: bool,
    /// Raw BeSt rows considered before dedup.
    pub raw_hits: usize,
    #[serde(rename = "_note")]
    pub note: &'static str,
    /// The place the query names, when it only names a place ("Spontin",
    /// "Celles Houyet"): its centre and postal codes. The features are
    /// then the buildings around that centre (`confidence: locality`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locality: Option<LocalityEcho>,
    pub features: Vec<Feature>,
}

#[derive(Serialize)]
pub struct LocalityEcho {
    /// `municipality`, `part_of_municipality`, `postal_area`, `village`,
    /// `hamlet`, `locality` or `isolated_dwelling`.
    pub kind: &'static str,
    pub name: MultilingualName,
    pub postal_codes: Vec<String>,
    /// `[lon, lat]`, as in GeoJSON.
    pub coordinates: [f64; 2],
}

impl From<Locality> for LocalityEcho {
    fn from(l: Locality) -> Self {
        Self {
            kind: l.kind.as_str(),
            name: l.name,
            postal_codes: l.postal_codes,
            coordinates: [l.lon, l.lat],
        }
    }
}

#[derive(Serialize)]
pub struct GeocodeEcho {
    /// The free text; absent for a structured query.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    pub limit: usize,
    pub expand: bool,
    /// What the parser made of the query — useful for debugging.
    pub parsed: ParsedEcho,
    /// Which reading produced the features: 0 = `parsed`, `n` =
    /// `parsed.alternatives[n - 1]`.
    pub reading: usize,
}

#[derive(Serialize)]
pub struct ParsedEcho {
    pub street_tokens: Vec<String>,
    pub house_number: Option<String>,
    pub box_number: Option<String>,
    pub postal_code: Option<String>,
    /// Other readings the parser considered (Phase 2), best first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<ParsedEcho>,
}

impl From<&ParsedQuery> for ParsedEcho {
    fn from(p: &ParsedQuery) -> Self {
        Self {
            street_tokens: p.street_tokens.clone(),
            house_number: p.house_number.clone(),
            box_number: p.box_number.clone(),
            postal_code: p.postal_code.clone(),
            alternatives: p.alternatives.iter().map(ParsedEcho::from).collect(),
        }
    }
}

/// What the client asked for: free text, or structured fields turned into
/// the same `ParsedQuery` shape.
fn parsed_input(p: &GeocodeParams) -> Result<(Option<String>, ParsedQuery), ServiceError> {
    let field = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    };
    let (street, housenumber, box_number, postcode, city) = (
        field(&p.street),
        field(&p.housenumber),
        field(&p.box_number),
        field(&p.postcode),
        field(&p.city),
    );
    let structured = street.is_some()
        || housenumber.is_some()
        || box_number.is_some()
        || postcode.is_some()
        || city.is_some();
    match (field(&p.q), structured) {
        (Some(_), true) => Err(ServiceError::BadQuery(
            "q cannot be combined with street, housenumber, box, postcode or city".into(),
        )),
        (Some(q), false) => {
            let parsed = crabfinder_parser::parse(&q);
            Ok((Some(q), parsed))
        }
        (None, false) => Err(ServiceError::BadQuery(
            "empty query: give q, or street / city / postcode".into(),
        )),
        (None, true) => {
            if street.is_none() && city.is_none() && postcode.is_none() {
                return Err(ServiceError::BadQuery(
                    "a structured query needs street, city or postcode".into(),
                ));
            }
            if let Some(pc) = &postcode
                && !(pc.len() == 4 && pc.bytes().all(|b| b.is_ascii_digit()))
            {
                return Err(ServiceError::BadQuery(format!(
                    "postcode must be 4 digits, got '{pc}'"
                )));
            }
            let words = |v: &Option<String>| -> Vec<String> {
                v.iter()
                    .flat_map(|t| t.split_whitespace())
                    .map(str::to_string)
                    .collect()
            };
            let mut street_tokens = words(&street);
            street_tokens.extend(words(&city));
            Ok((
                None,
                ParsedQuery {
                    street_tokens,
                    house_number: housenumber.map(|h| h.to_uppercase()),
                    box_number,
                    postal_code: postcode,
                    ..Default::default()
                },
            ))
        }
    }
}

pub async fn geocode(
    State(s): State<AppState>,
    ApiQuery(params): ApiQuery<GeocodeParams>,
) -> Result<Json<FeatureCollection>, ServiceError> {
    let (q, mut parsed) = parsed_input(&params)?;
    let lang = parse_lang(params.lang.as_deref())?;
    let limit = params.limit.clamp(1, 100);
    let expand = params.expand;

    let region_filter = match params.region.as_deref() {
        Some(raw) => Some(
            normalize_region_filter(raw)
                .ok_or_else(|| {
                    ServiceError::BadQuery(format!(
                        "unknown region '{raw}' (expected brussels/flanders/wallonia or BE-BRU/BE-VLG/BE-WAL)"
                    ))
                })?
                .to_string(),
        ),
        None => None,
    };
    let focus_point = match params.focus_point.as_deref() {
        Some(raw) => Some(parse_focus_point(raw).ok_or_else(|| {
            ServiceError::BadQuery(format!(
                "invalid focus_point '{raw}' (expected 'lat,lng' inside Belgium)"
            ))
        })?),
        None => None,
    };

    // An explicit `postal_code` param overrides the one parsed from q. A
    // 4-digit token the parser took as the postal code is then more
    // likely the house number: lead with a reading that kept it as one.
    let postal_override = params.postal_code.as_deref().map(|p| p.trim().to_string());
    if postal_override.is_some()
        && parsed.postal_code.is_some()
        && let Some(i) = parsed
            .alternatives
            .iter()
            .position(|a| a.postal_code.is_none())
    {
        parsed.promote(i);
    }
    let mut query = AddressQuery::from_parsed(&s.index, &parsed);
    query.for_each_reading(|r| {
        if postal_override.is_some() {
            r.postal_code.clone_from(&postal_override);
        }
        r.region_filter.clone_from(&region_filter);
        r.focus_point = focus_point;
    });
    // At least two candidates, so the ambiguity check has a runner-up.
    let opts = SearchOptions {
        limit: limit.max(2),
        expand,
        mid_street_fallback: true,
    };
    let set = blocking(s.clone(), move |s| {
        search(&query, &opts, &s.resolver_context())
    })
    .await?
    .map_err(|e| ServiceError::internal("Tantivy search", e))?;
    if set.capped {
        warn!(
            raw_hits = set.raw_hits,
            cap = RAW_HIT_CAP,
            q = %q_to_debug(&parsed),
            "oversearch cap reached; some buildings may be missing from the tail"
        );
    }
    if set.tier == Tier::OrStreet || set.tier == Tier::OrWithNumber {
        warn!(q = %q_to_debug(&parsed), "strict tiers empty; fell back to fuzzy");
    }
    let raw_hit_count = set.raw_hits;
    let (ambiguous, score_gap) = set.ambiguity();
    let locality = set.locality.map(LocalityEcho::from);
    let features: Vec<Feature> = set
        .candidates
        .into_iter()
        .take(limit)
        .map(|c| Feature::from(c).labelled(lang))
        .collect();

    // An empty result reports "strict": nothing fell back to fuzzy.
    let match_type = if features.is_empty()
        || features
            .iter()
            .any(|f| f.match_type == MatchType::Strict.as_str())
    {
        "strict"
    } else {
        "fuzzy"
    };

    Ok(Json(FeatureCollection {
        kind: "FeatureCollection",
        query: GeocodeEcho {
            q,
            limit,
            expand,
            parsed: ParsedEcho::from(&parsed),
            reading: set.reading,
        },
        count: features.len(),
        ambiguous,
        score_gap,
        match_type,
        deduplicated: !expand,
        raw_hits: raw_hit_count,
        note: if expand {
            "Phase 3: expand=true returns one feature per BeSt row (no dedup)."
        } else {
            "Phase 3: results collapsed by building_group_id; score = max(member.score). Use ?expand=true for raw rows."
        },
        locality,
        features,
    }))
}

/// Compact debug form of the parsed query for log lines.
fn q_to_debug(p: &ParsedQuery) -> String {
    format!(
        "{:?} hn={:?} pc={:?}",
        p.street_tokens, p.house_number, p.postal_code
    )
}
