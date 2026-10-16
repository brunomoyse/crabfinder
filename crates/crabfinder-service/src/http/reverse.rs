//! `GET /reverse` — coordinates → nearest features by great-circle distance.

use std::collections::HashMap;

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use tantivy::{TantivyDocument, schema::Value};

use crabfinder_core::house_number::leading_number;
use crabfinder_core::{Address, Language, MultilingualName};

use crate::AppState;
use crate::dedup::{MatchType, RawHit, group_by_building, raw_limit};
use crate::interpolate::{Anchor, interpolate};
use crate::mid_street::street_key_from_bgid;
use crate::spatial::SpatialHit;

use super::blocking;
use super::error::{ApiQuery, ServiceError};
use super::feature::{Feature, hydrate_feature, parse_lang};

#[derive(Debug, Deserialize)]
pub struct ReverseParams {
    pub lat: f64,
    pub lng: f64,
    #[serde(default = "default_reverse_limit")]
    pub limit: usize,
    #[serde(default)]
    pub radius_m: Option<f64>,
    /// Language of `properties.label`: `fr`, `nl` or `de`.
    #[serde(default)]
    pub lang: Option<String>,
    /// Also estimate the house number at the point when it falls in a
    /// gap between two known numbers of the nearest street (v2 Phase 10).
    #[serde(default)]
    pub interpolate: bool,
}

fn default_reverse_limit() -> usize {
    1
}

#[derive(Serialize)]
pub struct ReverseEcho {
    pub lat: f64,
    pub lng: f64,
    pub limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius_m: Option<f64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub interpolate: bool,
}

/// `interpolate=true`: the number the point would carry on the nearest
/// building's street side, between two known ones.
#[derive(Debug, Serialize)]
pub struct InterpolationEcho {
    pub house_number: String,
    /// The known numbers on either side.
    pub between: [String; 2],
    pub street_name: MultilingualName,
    pub postal_code: String,
    pub municipality_name: MultilingualName,
    /// On the segment between the two, `[lon, lat]`.
    pub coordinates: [f64; 2],
    /// From the query point to that segment, metres.
    pub distance_m: f64,
    /// `"Rue de la Loi 19, 1000 Bruxelles"` in `lang`.
    pub label: String,
}

#[derive(Serialize)]
pub struct ReverseCollection {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub query: ReverseEcho,
    pub count: usize,
    pub deduplicated: bool,
    pub raw_hits: usize,
    #[serde(rename = "_note")]
    pub note: &'static str,
    /// Only with `interpolate=true`, when the point lies in a gap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interpolation: Option<InterpolationEcho>,
    pub features: Vec<Feature>,
}

pub async fn reverse(
    State(s): State<AppState>,
    ApiQuery(params): ApiQuery<ReverseParams>,
) -> Result<Json<ReverseCollection>, ServiceError> {
    validate(&params)?;
    let lang = parse_lang(params.lang.as_deref())?;
    let limit = params.limit.clamp(1, 100);

    let (lat, lng, radius_m) = (params.lat, params.lng, params.radius_m);
    let interpolate = params.interpolate;
    let (features, raw_hit_count, interpolation) = blocking(s, move |s| {
        let (features, raw) = nearest_features(&s, (lat, lng), radius_m, limit, lang)?;
        let interpolation = if interpolate {
            interpolate_at(&s, (lat, lng), lang)?
        } else {
            None
        };
        Ok::<_, ServiceError>((features, raw, interpolation))
    })
    .await??;

    Ok(Json(ReverseCollection {
        kind: "FeatureCollection",
        query: ReverseEcho {
            lat: params.lat,
            lng: params.lng,
            limit,
            radius_m: params.radius_m,
            interpolate,
        },
        count: features.len(),
        deduplicated: true,
        raw_hits: raw_hit_count,
        note: "Phase 4: nearest-neighbour by great-circle distance; collapsed by building_group_id.",
        interpolation,
        features,
    }))
}

/// The `limit` buildings nearest to `(lat, lng)` (within `radius_m`),
/// nearest first, and the raw spatial hit count.
fn nearest_features(
    s: &AppState,
    (lat, lng): (f64, f64),
    radius_m: Option<f64>,
    limit: usize,
    lang: Option<Language>,
) -> Result<(Vec<Feature>, usize), ServiceError> {
    // Oversearch so dedup can find `limit` distinct buildings; a dense city
    // block can have hundreds of units sharing very close coordinates.
    let spatial_hits = s
        .spatial
        .nearest(lat, lng, raw_limit(limit, false), radius_m);
    let raw_hit_count = spatial_hits.len();

    // Score = -distance_m so dedup's max-score semantics resolve to
    // nearest-wins.
    let searcher = s.reader.searcher();
    let mut docs: Vec<TantivyDocument> = Vec::with_capacity(spatial_hits.len());
    let mut raw_hits: Vec<RawHit> = Vec::with_capacity(spatial_hits.len());
    let mut distances: Vec<f64> = Vec::with_capacity(spatial_hits.len());
    for (
        i,
        SpatialHit {
            doc_addr,
            distance_m,
            ..
        },
    ) in spatial_hits.iter().enumerate()
    {
        let doc = searcher
            .doc::<TantivyDocument>(*doc_addr)
            .map_err(|e| ServiceError::internal("Tantivy doc", e))?;
        raw_hits.push(RawHit::from_doc(
            &doc,
            &s.schema,
            (-distance_m) as f32,
            MatchType::Spatial,
            i,
        ));
        docs.push(doc);
        distances.push(*distance_m);
    }

    // max(-distance) = min(distance), so groups come out nearest-first.
    let features = group_by_building(&raw_hits)
        .into_iter()
        .take(limit)
        .map(|g| {
            hydrate_feature(
                &docs[g.representative_index],
                &s.schema,
                g.max_score,
                g.match_type,
                g.unit_count,
                g.building_group_id,
                Some(distances[g.representative_index]),
            )
            .map(|f| f.labelled(lang))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((features, raw_hit_count))
}

/// Known addresses gathered around the point for interpolation.
const INTERPOLATE_RADIUS_M: f64 = 200.0;
const INTERPOLATE_MAX_POINTS: usize = 800;

/// The interpolated number at `(lat, lng)`. Every street side (street
/// object + postal code + parity) with known numbers within
/// `INTERPOLATE_RADIUS_M` is tried (`interpolate::interpolate`); the gap
/// segment nearest to the point wins, provided the point is nearer to it
/// than to any existing address — else that address is the answer.
fn interpolate_at(
    s: &AppState,
    (lat, lng): (f64, f64),
    lang: Option<Language>,
) -> Result<Option<InterpolationEcho>, ServiceError> {
    let hits = s
        .spatial
        .nearest(lat, lng, INTERPOLATE_MAX_POINTS, Some(INTERPOLATE_RADIUS_M));
    let Some(nearest_m) = hits.first().map(|h| h.distance_m) else {
        return Ok(None);
    };
    let searcher = s.reader.searcher();
    let text = |d: &TantivyDocument, f| {
        d.get_first(f)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let num = |d: &TantivyDocument, f| d.get_first(f).and_then(|v| v.as_f64());
    // (street key, postal code) → anchors, and one payload of the street.
    let mut streets: HashMap<(String, String), (Vec<Anchor>, String)> = HashMap::new();
    for h in &hits {
        let d: TantivyDocument = searcher
            .doc(h.doc_addr)
            .map_err(|e| ServiceError::internal("Tantivy doc", e))?;
        let bgid = text(&d, s.schema.building_group_id);
        let hn = text(&d, s.schema.house_number);
        let (Some(street), Some(number), Some(lat), Some(lon)) = (
            street_key_from_bgid(&bgid),
            leading_number(&hn),
            num(&d, s.schema.lat),
            num(&d, s.schema.lon),
        ) else {
            continue;
        };
        let key = (street.to_string(), text(&d, s.schema.postal_code));
        let entry = streets
            .entry(key)
            .or_insert_with(|| (Vec::new(), text(&d, s.schema.payload)));
        entry.0.push(Anchor {
            number,
            house_number: hn,
            lat,
            lon,
        });
    }
    let best = streets
        .values()
        .flat_map(|(anchors, payload)| {
            [0, 1]
                .into_iter()
                .filter_map(move |parity| interpolate(lat, lng, parity, anchors))
                .map(move |e| (e, payload))
        })
        .filter(|(e, _)| e.distance_m < nearest_m)
        .min_by(|(a, _), (b, _)| a.distance_m.total_cmp(&b.distance_m));
    let Some((e, payload)) = best else {
        return Ok(None);
    };
    let mut address: Address =
        serde_json::from_str(payload).map_err(|e| ServiceError::internal("payload decode", e))?;
    address.house_number = e.house_number.to_string();
    address.box_number = None;
    Ok(Some(InterpolationEcho {
        label: address.display_name(lang),
        house_number: address.house_number,
        between: [e.before, e.after],
        street_name: address.street_name,
        postal_code: address.postal_code,
        municipality_name: address.municipality_name,
        coordinates: [e.lon, e.lat],
        distance_m: e.distance_m,
    }))
}

fn validate(p: &ReverseParams) -> Result<(), ServiceError> {
    if !p.lat.is_finite() || !(-90.0..=90.0).contains(&p.lat) {
        return Err(ServiceError::BadQuery(format!(
            "lat out of range [-90, 90]: {}",
            p.lat
        )));
    }
    if !p.lng.is_finite() || !(-180.0..=180.0).contains(&p.lng) {
        return Err(ServiceError::BadQuery(format!(
            "lng out of range [-180, 180]: {}",
            p.lng
        )));
    }
    if let Some(r) = p.radius_m
        && (!r.is_finite() || r < 0.0)
    {
        return Err(ServiceError::BadQuery(format!(
            "radius_m must be non-negative finite, got {r}"
        )));
    }
    Ok(())
}
