//! Parsers for the four BeSt XML entity files.
//!
//! All four parsers use a single pattern: a path stack (current element names
//! from root to leaf) lets us route `<text>` events to the right field. This
//! is mechanical but always works and is easy to extend.
//!
//! The three small entities (Municipality, PostalInfo, StreetName,
//! PartOfMunicipality) parse into `Vec<T>` because the consumer wants the
//! whole thing in a HashMap. The Address parser is callback-based to support
//! true streaming on the multi-gigabyte file.

use std::io::BufRead;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow};
use quick_xml::Reader;
use quick_xml::events::{BytesText, Event};

/// Decode BeSt-XML text. The dumps are UTF-8 (declared in the XML header) and
/// in practice contain no HTML entity escapes — names with apostrophes are
/// stored literally (e.g. `Braine-l'Alleud`). quick-xml already hands us
/// UTF-8-validated `str` content, so no unescaping is needed.
fn decode_text(t: &BytesText<'_>) -> Result<String> {
    Ok(t.as_ref().to_owned())
}

use crabfinder_core::{
    BestId, Language, Municipality, MunicipalityId, MultilingualName, PartOfMunicipality,
    PartOfMunicipalityId, PostalInfo, PostalInfoId, Status, Street, StreetId, StreetType,
};

// --- Internal helpers -------------------------------------------------------

#[derive(Default)]
struct CodeFields {
    namespace: String,
    object_identifier: String,
    version_identifier: String,
}

impl CodeFields {
    fn into_best_id(self) -> Result<BestId> {
        if self.namespace.is_empty() || self.object_identifier.is_empty() {
            return Err(anyhow!("missing namespace or objectIdentifier in <com:code>"));
        }
        Ok(BestId::new(
            self.namespace,
            self.object_identifier,
            self.version_identifier,
        ))
    }
}

fn make_reader<R: BufRead>(reader: R) -> Reader<R> {
    let mut xml = Reader::from_reader(reader);
    xml.config_mut().trim_text(true);
    xml
}

/// Compare a quick-xml element name against a literal byte string.
fn name_is(e: &quick_xml::events::BytesStart, expected: &[u8]) -> bool {
    e.name().as_ref().as_bytes() == expected
}

fn end_is(e: &quick_xml::events::BytesEnd, expected: &[u8]) -> bool {
    e.name().as_ref().as_bytes() == expected
}

// --- Municipality -----------------------------------------------------------

pub fn read_municipalities<R: BufRead>(reader: R) -> Result<Vec<Municipality>> {
    let mut xml = make_reader(reader);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut path: Vec<Vec<u8>> = Vec::new();

    let mut in_record = false;
    let mut code = CodeFields::default();
    let mut names = MultilingualName::default();
    let mut current_lang: Option<Language> = None;

    loop {
        match xml.read_event_into(&mut buf)? {
            Event::Start(e) => {
                if name_is(&e, b"tns:municipality") {
                    in_record = true;
                    code = CodeFields::default();
                    names = MultilingualName::default();
                }
                path.push(e.name().as_ref().as_bytes().to_vec());
            }
            Event::End(e) => {
                path.pop();
                if in_record && end_is(&e, b"tns:municipality") {
                    in_record = false;
                    let id = MunicipalityId::new(code.clone().into_best_id()?);
                    out.push(Municipality { id, names: names.clone() });
                }
            }
            Event::Text(t) if in_record => {
                let text = decode_text(&t)?;
                handle_municipality_text(&path, &text, &mut code, &mut names, &mut current_lang);
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

fn handle_municipality_text(
    path: &[Vec<u8>],
    text: &str,
    code: &mut CodeFields,
    names: &mut MultilingualName,
    current_lang: &mut Option<Language>,
) {
    let last = path.last().map(|v| v.as_slice());
    let parent = path.iter().rev().nth(1).map(|v| v.as_slice());
    match (parent, last) {
        (Some(b"com:code"), Some(b"com:namespace")) => code.namespace = text.to_string(),
        (Some(b"com:code"), Some(b"com:objectIdentifier")) => {
            code.object_identifier = text.to_string()
        }
        (Some(b"com:code"), Some(b"com:versionIdentifier")) => {
            code.version_identifier = text.to_string()
        }
        (Some(b"com:name"), Some(b"com:language")) => {
            *current_lang = Language::from_str(text).ok();
        }
        (Some(b"com:name"), Some(b"com:spelling")) => {
            if let Some(l) = current_lang.take() {
                names.set(l, text.to_string());
            }
        }
        _ => {}
    }
}

// Make CodeFields cloneable so we can copy it into into_best_id without losing
// the in-progress builder state. Cheap: 3 strings.
impl Clone for CodeFields {
    fn clone(&self) -> Self {
        Self {
            namespace: self.namespace.clone(),
            object_identifier: self.object_identifier.clone(),
            version_identifier: self.version_identifier.clone(),
        }
    }
}

// --- PostalInfo -------------------------------------------------------------

pub fn read_postal_infos<R: BufRead>(reader: R) -> Result<Vec<PostalInfo>> {
    let mut xml = make_reader(reader);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut path: Vec<Vec<u8>> = Vec::new();

    let mut in_record = false;
    let mut code = CodeFields::default();
    let mut names = MultilingualName::default();
    let mut current_lang: Option<Language> = None;

    loop {
        match xml.read_event_into(&mut buf)? {
            Event::Start(e) => {
                if name_is(&e, b"tns:postalInfo") {
                    in_record = true;
                    code = CodeFields::default();
                    names = MultilingualName::default();
                }
                path.push(e.name().as_ref().as_bytes().to_vec());
            }
            Event::End(e) => {
                path.pop();
                if in_record && end_is(&e, b"tns:postalInfo") {
                    in_record = false;
                    let id = PostalInfoId::new(code.clone().into_best_id()?);
                    out.push(PostalInfo { id, names: names.clone() });
                }
            }
            Event::Text(t) if in_record => {
                let text = decode_text(&t)?;
                handle_municipality_text(&path, &text, &mut code, &mut names, &mut current_lang);
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

// --- PartOfMunicipality (Wallonia only) -------------------------------------

pub fn read_parts_of_municipality<R: BufRead>(reader: R) -> Result<Vec<PartOfMunicipality>> {
    let mut xml = make_reader(reader);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut path: Vec<Vec<u8>> = Vec::new();

    let mut in_record = false;
    let mut code = CodeFields::default();
    let mut names = MultilingualName::default();
    let mut current_lang: Option<Language> = None;

    loop {
        match xml.read_event_into(&mut buf)? {
            Event::Start(e) => {
                if name_is(&e, b"tns:partOfMunicipality") {
                    in_record = true;
                    code = CodeFields::default();
                    names = MultilingualName::default();
                }
                path.push(e.name().as_ref().as_bytes().to_vec());
            }
            Event::End(e) => {
                path.pop();
                if in_record && end_is(&e, b"tns:partOfMunicipality") {
                    in_record = false;
                    let id = PartOfMunicipalityId::new(code.clone().into_best_id()?);
                    out.push(PartOfMunicipality { id, names: names.clone() });
                }
            }
            Event::Text(t) if in_record => {
                let text = decode_text(&t)?;
                handle_municipality_text(&path, &text, &mut code, &mut names, &mut current_lang);
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

// --- StreetName -------------------------------------------------------------

pub fn read_streets<R: BufRead>(reader: R) -> Result<Vec<Street>> {
    let mut xml = make_reader(reader);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut path: Vec<Vec<u8>> = Vec::new();

    let mut in_record = false;
    let mut code = CodeFields::default();
    let mut names = MultilingualName::default();
    let mut current_lang: Option<Language> = None;
    let mut status: Option<Status> = None;
    let mut street_type: Option<StreetType> = None;
    let mut assigned_by = CodeFields::default();

    loop {
        match xml.read_event_into(&mut buf)? {
            Event::Start(e) => {
                if name_is(&e, b"tns:streetName") {
                    in_record = true;
                    code = CodeFields::default();
                    names = MultilingualName::default();
                    status = None;
                    street_type = None;
                    assigned_by = CodeFields::default();
                }
                path.push(e.name().as_ref().as_bytes().to_vec());
            }
            Event::End(e) => {
                path.pop();
                if in_record && end_is(&e, b"tns:streetName") {
                    in_record = false;
                    let id = StreetId::new(code.clone().into_best_id()?);
                    let s = status.ok_or_else(|| {
                        anyhow!("street {:?} missing <com:status>", id.0.object_identifier)
                    })?;
                    let t = street_type.unwrap_or(StreetType::Streetname);
                    let m = MunicipalityId::new(
                        assigned_by
                            .clone()
                            .into_best_id()
                            .context("street missing <com:isAssignedByMunicipality>")?,
                    );
                    out.push(Street {
                        id,
                        names: names.clone(),
                        street_type: t,
                        status: s,
                        assigned_by_municipality: m,
                    });
                }
            }
            Event::Text(t) if in_record => {
                let text = decode_text(&t)?;
                handle_street_text(
                    &path,
                    &text,
                    &mut code,
                    &mut names,
                    &mut current_lang,
                    &mut status,
                    &mut street_type,
                    &mut assigned_by,
                );
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn handle_street_text(
    path: &[Vec<u8>],
    text: &str,
    code: &mut CodeFields,
    names: &mut MultilingualName,
    current_lang: &mut Option<Language>,
    status: &mut Option<Status>,
    street_type: &mut Option<StreetType>,
    assigned_by: &mut CodeFields,
) {
    let last = path.last().map(|v| v.as_slice());
    let parent = path.iter().rev().nth(1).map(|v| v.as_slice());
    let grand = path.iter().rev().nth(2).map(|v| v.as_slice());

    match (grand, parent, last) {
        (_, Some(b"com:code"), Some(b"com:namespace")) => code.namespace = text.to_string(),
        (_, Some(b"com:code"), Some(b"com:objectIdentifier")) => {
            code.object_identifier = text.to_string()
        }
        (_, Some(b"com:code"), Some(b"com:versionIdentifier")) => {
            code.version_identifier = text.to_string()
        }
        (_, Some(b"com:name"), Some(b"com:language")) => {
            *current_lang = Language::from_str(text).ok();
        }
        (_, Some(b"com:name"), Some(b"com:spelling")) => {
            if let Some(l) = current_lang.take() {
                names.set(l, text.to_string());
            }
        }
        (_, Some(b"com:streetNameStatus"), Some(b"com:status")) => {
            *status = Status::from_str(text).ok();
        }
        (_, Some(b"tns:streetName"), Some(b"com:type")) => {
            *street_type = StreetType::from_str(text).ok();
        }
        (_, Some(b"com:isAssignedByMunicipality"), Some(b"com:namespace")) => {
            assigned_by.namespace = text.to_string()
        }
        (_, Some(b"com:isAssignedByMunicipality"), Some(b"com:objectIdentifier")) => {
            assigned_by.object_identifier = text.to_string()
        }
        (_, Some(b"com:isAssignedByMunicipality"), Some(b"com:versionIdentifier")) => {
            assigned_by.version_identifier = text.to_string()
        }
        _ => {}
    }
}

// --- Address (streaming, callback-based) ------------------------------------

/// One BeSt Address record after XML parsing, before reference resolution.
#[derive(Debug, Clone)]
pub struct RawAddress {
    pub id: BestId,
    pub status: Status,
    pub house_number: String,
    pub box_number: Option<String>,
    pub officially_assigned: bool,
    pub street_ref: BestId,
    pub municipality_ref: BestId,
    pub postal_info_ref: BestId,
    pub part_of_municipality_ref: Option<BestId>,
    /// Position in EPSG:31370 (Lambert 72). Reprojection happens later.
    pub easting: f64,
    pub northing: f64,
}

/// Stream the Address XML, invoking `handler` for each parsed `RawAddress`.
/// Returns the total number of addresses parsed (including those for which
/// `handler` returned an error or `false`).
///
/// `handler` returns `Ok(true)` to continue, `Ok(false)` to stop early (used
/// for `--limit`).
pub fn stream_addresses<R, F>(reader: R, mut handler: F) -> Result<usize>
where
    R: BufRead,
    F: FnMut(RawAddress) -> Result<bool>,
{
    let mut xml = make_reader(reader);
    let mut buf = Vec::new();
    let mut path: Vec<Vec<u8>> = Vec::new();
    let mut total = 0usize;

    let mut in_record = false;
    let mut code = CodeFields::default();
    let mut status: Option<Status> = None;
    let mut house_number = String::new();
    let mut box_number: Option<String> = None;
    let mut officially_assigned = true;
    let mut pos_x: Option<f64> = None;
    let mut pos_y: Option<f64> = None;
    let mut street_ref = CodeFields::default();
    let mut municipality_ref = CodeFields::default();
    let mut postal_info_ref = CodeFields::default();
    let mut pom_ref: Option<CodeFields> = None;

    loop {
        match xml.read_event_into(&mut buf)? {
            Event::Start(e) => {
                if name_is(&e, b"tns:address") {
                    in_record = true;
                    code = CodeFields::default();
                    status = None;
                    house_number.clear();
                    box_number = None;
                    officially_assigned = true;
                    pos_x = None;
                    pos_y = None;
                    street_ref = CodeFields::default();
                    municipality_ref = CodeFields::default();
                    postal_info_ref = CodeFields::default();
                    pom_ref = None;
                } else if in_record && name_is(&e, b"com:hasPartOfMunicipality") {
                    pom_ref = Some(CodeFields::default());
                }
                path.push(e.name().as_ref().as_bytes().to_vec());
            }
            Event::End(e) => {
                path.pop();
                if in_record && end_is(&e, b"tns:address") {
                    in_record = false;
                    total += 1;

                    let id = code.clone().into_best_id()?;
                    let s = status.unwrap_or(Status::Current); // sensible fallback
                    let x = pos_x.ok_or_else(|| anyhow!("address {} missing pos x", id))?;
                    let y = pos_y.ok_or_else(|| anyhow!("address {} missing pos y", id))?;

                    let street = street_ref.clone().into_best_id()?;
                    let muni = municipality_ref.clone().into_best_id()?;
                    let postal = postal_info_ref.clone().into_best_id()?;
                    let pom = pom_ref.clone().and_then(|c| c.into_best_id().ok());

                    let raw = RawAddress {
                        id,
                        status: s,
                        house_number: house_number.clone(),
                        box_number: box_number.clone(),
                        officially_assigned,
                        street_ref: street,
                        municipality_ref: muni,
                        postal_info_ref: postal,
                        part_of_municipality_ref: pom,
                        easting: x,
                        northing: y,
                    };

                    if !handler(raw)? {
                        // caller asked us to stop
                        break;
                    }
                }
            }
            Event::Text(t) if in_record => {
                let text = decode_text(&t)?;
                handle_address_text(
                    &path,
                    &text,
                    &mut code,
                    &mut status,
                    &mut house_number,
                    &mut box_number,
                    &mut officially_assigned,
                    &mut pos_x,
                    &mut pos_y,
                    &mut street_ref,
                    &mut municipality_ref,
                    &mut postal_info_ref,
                    &mut pom_ref,
                );
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(total)
}

#[allow(clippy::too_many_arguments)]
fn handle_address_text(
    path: &[Vec<u8>],
    text: &str,
    code: &mut CodeFields,
    status: &mut Option<Status>,
    house_number: &mut String,
    box_number: &mut Option<String>,
    officially_assigned: &mut bool,
    pos_x: &mut Option<f64>,
    pos_y: &mut Option<f64>,
    street_ref: &mut CodeFields,
    municipality_ref: &mut CodeFields,
    postal_info_ref: &mut CodeFields,
    pom_ref: &mut Option<CodeFields>,
) {
    let last = path.last().map(|v| v.as_slice());
    let parent = path.iter().rev().nth(1).map(|v| v.as_slice());
    let grand = path.iter().rev().nth(2).map(|v| v.as_slice());

    // tns:address > com:code > com:{namespace,objectIdentifier,versionIdentifier}
    if grand == Some(b"tns:address") && parent == Some(b"com:code") {
        match last {
            Some(b"com:namespace") => code.namespace = text.to_string(),
            Some(b"com:objectIdentifier") => code.object_identifier = text.to_string(),
            Some(b"com:versionIdentifier") => code.version_identifier = text.to_string(),
            _ => {}
        }
        return;
    }

    // tns:address > com:addressStatus > com:status
    if parent == Some(b"com:addressStatus") && last == Some(b"com:status") {
        *status = Status::from_str(text).ok();
        return;
    }

    // direct children of tns:address
    if parent == Some(b"tns:address") {
        match last {
            Some(b"com:houseNumber") => house_number.push_str(text),
            Some(b"com:boxNumber") => *box_number = Some(text.to_string()),
            Some(b"com:officiallyAssigned") => {
                *officially_assigned = matches!(text, "true" | "1");
            }
            _ => {}
        }
        return;
    }

    // tns:address > com:position > com:pointGeometry > com:point > com:pos
    if last == Some(b"com:pos") {
        let parts: Vec<&str> = text.split_ascii_whitespace().collect();
        if parts.len() == 2 {
            if let (Ok(x), Ok(y)) = (parts[0].parse(), parts[1].parse()) {
                *pos_x = Some(x);
                *pos_y = Some(y);
            }
        }
        return;
    }

    // Reference triples: hasStreetName / hasMunicipality / hasPostalInfo / hasPartOfMunicipality
    match parent {
        Some(b"com:hasStreetName") => fill_code(street_ref, last, text),
        Some(b"com:hasMunicipality") => fill_code(municipality_ref, last, text),
        Some(b"com:hasPostalInfo") => fill_code(postal_info_ref, last, text),
        Some(b"com:hasPartOfMunicipality") => {
            let c = pom_ref.get_or_insert_with(CodeFields::default);
            fill_code(c, last, text);
        }
        _ => {}
    }
}

fn fill_code(target: &mut CodeFields, last: Option<&[u8]>, text: &str) {
    match last {
        Some(b"com:namespace") => target.namespace = text.to_string(),
        Some(b"com:objectIdentifier") => target.object_identifier = text.to_string(),
        Some(b"com:versionIdentifier") => target.version_identifier = text.to_string(),
        _ => {}
    }
}
