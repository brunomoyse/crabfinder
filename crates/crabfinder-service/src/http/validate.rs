//! `GET /validate` (v3 Phase 11.2): is this a real BeSt address, and if
//! not quite, what is the official one and what differs?
//!
//! Same input as `/geocode` (free text `q`, or `street` / `housenumber` /
//! `box` / `postcode` / `city`), the batch top-1 decision
//! (`resolve_with`), then `validate::assess`.

use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::query_builder::analyze;
use crate::resolver::{AddressQuery, SearchOptions, resolve_with};
use crate::validate::{Correction, Status, assess};

use super::blocking;
use super::error::{ApiQuery, ServiceError};
use super::feature::{Feature, parse_lang};
use super::geocode::{InputFields, ParsedEcho, normalize_region_filter, parsed_input};

#[derive(Debug, Deserialize)]
pub struct ValidateParams {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub street: Option<String>,
    #[serde(default)]
    pub housenumber: Option<String>,
    #[serde(default, rename = "box")]
    pub box_number: Option<String>,
    #[serde(default)]
    pub postcode: Option<String>,
    #[serde(default)]
    pub city: Option<String>,
    /// Language of the label and of `street_language`: `fr`, `nl`, `de`.
    #[serde(default)]
    pub lang: Option<String>,
    /// Hard filter on the region, as `/geocode?region=`.
    #[serde(default)]
    pub region: Option<String>,
}

#[derive(Serialize)]
pub struct ValidateResponse {
    /// `valid`, `corrected`, `ambiguous`, `incomplete` or `not_found`.
    pub status: Status,
    pub query: ValidateEcho,
    /// The official BeSt address (a GeoJSON Feature with `label`); `null`
    /// when not found. For `incomplete` it is the street or place that
    /// matched, not an address.
    pub address: Option<Feature>,
    pub corrections: Vec<Correction>,
    pub confidence: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<&'static str>,
}

#[derive(Serialize)]
pub struct ValidateEcho {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    pub parsed: ParsedEcho,
    /// Which reading the answer comes from (0 = `parsed`).
    pub reading: usize,
}

pub async fn validate(
    State(s): State<AppState>,
    ApiQuery(p): ApiQuery<ValidateParams>,
) -> Result<Json<ValidateResponse>, ServiceError> {
    let (q, parsed) = parsed_input(InputFields {
        q: &p.q,
        street: &p.street,
        housenumber: &p.housenumber,
        box_number: &p.box_number,
        postcode: &p.postcode,
        city: &p.city,
    })?;
    let lang = parse_lang(p.lang.as_deref())?;
    let region = p
        .region
        .as_deref()
        .map(|raw| {
            normalize_region_filter(raw)
                .map(str::to_string)
                .ok_or_else(|| ServiceError::BadQuery(format!("unknown region '{raw}'")))
        })
        .transpose()?;
    let mut query = AddressQuery::from_parsed(&s.index, &parsed);
    query.city_tokens = p
        .city
        .as_deref()
        .filter(|_| q.is_none())
        .map_or(0, |c| analyze(&s.index, c).len());
    query.for_each_reading(|r| r.region_filter.clone_from(&region));
    let (r, validation) = blocking(s.clone(), move |s| {
        let ctx = s.resolver_context();
        let r = resolve_with(&query, &SearchOptions::default(), &ctx);
        let v = assess(&query, &r, ctx.index, &ctx.abbrev, lang);
        (r, v)
    })
    .await?;
    let address = r.hit.map(|hit| {
        Feature {
            confidence: Some(r.confidence.as_str()),
            fallback_reason: r.fallback_reason.map(|f| f.as_str()),
            house_number_delta: r.house_number_delta,
            ..Feature::from(hit)
        }
        .labelled(lang)
    });
    Ok(Json(ValidateResponse {
        status: validation.status,
        query: ValidateEcho {
            q,
            parsed: ParsedEcho::from(&parsed),
            reading: r.reading,
        },
        address,
        corrections: validation.corrections,
        confidence: r.confidence.as_str(),
        fallback_reason: r.fallback_reason.map(|f| f.as_str()),
    }))
}
