//! Generate tiny BeSt-shaped zip bundles for indexer tests.
//!
//! Renders the same element paths `crabfinder_indexer::xml` routes on
//! (`tns:<record>` > `com:code` > `com:namespace`, `com:name` >
//! `com:language`/`com:spelling`, …) and writes one
//! `{Region}{Entity}20260101.zip` per entity, each holding a single `.xml`
//! entry — the layout `bundle::find_zip` + `zip_read::with_inner_xml` expect.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crabfinder_core::Region;

use crate::namespaces;

#[derive(Debug, Clone)]
pub struct MuniSpec {
    pub oid: String,
    pub fr: Option<String>,
    pub nl: Option<String>,
    pub de: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PostalSpec {
    pub code: String,
    pub nl: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StreetSpec {
    pub oid: String,
    pub fr: Option<String>,
    pub nl: Option<String>,
    pub de: Option<String>,
    /// BeSt status spelling (`current`, `retired`, …).
    pub status: String,
    pub muni_oid: String,
}

#[derive(Debug, Clone)]
pub struct PomSpec {
    pub oid: String,
    pub fr: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AddrSpec {
    pub oid: String,
    pub street_oid: String,
    pub muni_oid: String,
    pub postal: String,
    pub pom_oid: Option<String>,
    pub house: String,
    pub box_no: Option<String>,
    pub status: String,
    pub officially_assigned: bool,
    /// Lambert 72 easting / northing.
    pub x: f64,
    pub y: f64,
    /// `endLifeSpanVersion` attribute of `<tns:address>`.
    pub end_life_span: Option<String>,
}

impl AddrSpec {
    pub fn new(oid: &str, street_oid: &str, muni_oid: &str, postal: &str, house: &str) -> Self {
        Self {
            oid: oid.into(),
            street_oid: street_oid.into(),
            muni_oid: muni_oid.into(),
            postal: postal.into(),
            pom_oid: None,
            house: house.into(),
            box_no: None,
            status: "current".into(),
            officially_assigned: true,
            // Brussels Grand-Place, Lambert 72.
            x: 148_950.0,
            y: 170_500.0,
            end_life_span: None,
        }
    }
}

/// One region's worth of BeSt entities.
#[derive(Debug, Clone)]
pub struct BestBundle {
    pub region: Region,
    pub municipalities: Vec<MuniSpec>,
    pub postals: Vec<PostalSpec>,
    pub streets: Vec<StreetSpec>,
    pub poms: Vec<PomSpec>,
    pub addresses: Vec<AddrSpec>,
}

fn opt(s: &str) -> Option<String> {
    Some(s.to_string())
}

impl BestBundle {
    /// A small but complete Brussels bundle: 2 municipalities, 2 postal
    /// codes, 3 streets, 6 addresses (one retired, one with a dangling
    /// street reference, one with a box number).
    pub fn brussels_sample() -> Self {
        let street = |oid: &str, fr: &str, nl: &str, muni: &str| StreetSpec {
            oid: oid.into(),
            fr: opt(fr),
            nl: opt(nl),
            de: None,
            status: "current".into(),
            muni_oid: muni.into(),
        };
        let mut boxed = AddrSpec::new("1003", "3919", "21009", "1050", "32");
        boxed.box_no = opt("2");
        let mut retired = AddrSpec::new("1004", "3919", "21009", "1050", "99");
        retired.status = "retired".into();
        retired.end_life_span = opt("2024-01-01T10:00:00Z");
        let mut dangling = AddrSpec::new("1005", "9999", "21009", "1050", "1");
        dangling.officially_assigned = false;
        Self {
            region: Region::Brussels,
            municipalities: vec![
                MuniSpec {
                    oid: "21004".into(),
                    fr: opt("Bruxelles"),
                    nl: opt("Brussel"),
                    de: None,
                },
                MuniSpec {
                    oid: "21009".into(),
                    fr: opt("Ixelles"),
                    nl: opt("Elsene"),
                    de: None,
                },
            ],
            postals: vec![
                PostalSpec {
                    code: "1000".into(),
                    nl: None,
                },
                PostalSpec {
                    code: "1050".into(),
                    nl: None,
                },
            ],
            streets: vec![
                street("7", "Grand-Place", "Grote Markt", "21004"),
                street("3919", "Avenue Louise", "Louizalaan", "21009"),
                street("100", "Rue de la Loi", "Wetstraat", "21004"),
            ],
            poms: vec![],
            addresses: vec![
                AddrSpec::new("1000", "7", "21004", "1000", "1"),
                AddrSpec::new("1001", "100", "21004", "1000", "16"),
                AddrSpec::new("1002", "3919", "21009", "1050", "10"),
                boxed,
                retired,
                dangling,
            ],
        }
    }

    /// A small Wallonia bundle with a part of municipality.
    pub fn wallonia_sample() -> Self {
        let mut a = AddrSpec::new("2000", "6001", "91114", "5580", "3");
        a.pom_oid = opt("9111401");
        a.x = 205_000.0;
        a.y = 95_000.0;
        Self {
            region: Region::Wallonia,
            municipalities: vec![MuniSpec {
                oid: "91114".into(),
                fr: opt("Rochefort"),
                nl: None,
                de: None,
            }],
            postals: vec![PostalSpec {
                code: "5580".into(),
                nl: None,
            }],
            streets: vec![StreetSpec {
                oid: "6001".into(),
                fr: opt("Rue du Village"),
                nl: None,
                de: None,
                status: "current".into(),
                muni_oid: "91114".into(),
            }],
            poms: vec![PomSpec {
                oid: "9111401".into(),
                fr: opt("Han-sur-Lesse"),
            }],
            addresses: vec![a],
        }
    }

    fn ns(&self) -> [&'static str; 5] {
        namespaces(self.region)
    }

    fn prefix(&self) -> &'static str {
        match self.region {
            Region::Brussels => "Brussels",
            Region::Flanders => "Flanders",
            Region::Wallonia => "Wallonia",
        }
    }

    pub fn municipality_xml(&self) -> String {
        let ns = self.ns()[2];
        wrap(
            "municipalities",
            self.municipalities.iter().map(|m| {
                format!(
                    "<tns:municipality>{}{}</tns:municipality>",
                    code(ns, &m.oid),
                    names(&[("fr", &m.fr), ("nl", &m.nl), ("de", &m.de)])
                )
            }),
        )
    }

    pub fn postal_xml(&self) -> String {
        let ns = self.ns()[3];
        wrap(
            "postalInfos",
            self.postals.iter().map(|p| {
                format!(
                    "<tns:postalInfo>{}{}</tns:postalInfo>",
                    code(ns, &p.code),
                    names(&[("nl", &p.nl)])
                )
            }),
        )
    }

    pub fn street_xml(&self) -> String {
        let [_, ns_street, ns_muni, _, _] = self.ns();
        wrap(
            "streetNames",
            self.streets.iter().map(|s| {
                format!(
                    "<tns:streetName>{}{}<com:type>streetname</com:type>\
                     <com:streetNameStatus><com:status>{}</com:status></com:streetNameStatus>\
                     <com:isAssignedByMunicipality>{}</com:isAssignedByMunicipality>\
                     </tns:streetName>",
                    code(ns_street, &s.oid),
                    names(&[("fr", &s.fr), ("nl", &s.nl), ("de", &s.de)]),
                    s.status,
                    triple(ns_muni, &s.muni_oid),
                )
            }),
        )
    }

    pub fn pom_xml(&self) -> String {
        let ns = self.ns()[4];
        wrap(
            "partOfMunicipalities",
            self.poms.iter().map(|p| {
                format!(
                    "<tns:partOfMunicipality>{}{}</tns:partOfMunicipality>",
                    code(ns, &p.oid),
                    names(&[("fr", &p.fr)])
                )
            }),
        )
    }

    pub fn address_xml(&self) -> String {
        let [ns_addr, ns_street, ns_muni, ns_postal, ns_pom] = self.ns();
        wrap(
            "addresses",
            self.addresses.iter().map(|a| {
                let mut s = String::new();
                match &a.end_life_span {
                    Some(end) => {
                        let _ = write!(s, "<tns:address endLifeSpanVersion=\"{end}\">");
                    }
                    None => s.push_str("<tns:address>"),
                }
                s.push_str(&code(ns_addr, &a.oid));
                let _ = write!(
                    s,
                    "<com:position><com:pointGeometry><com:point><com:pos>{} {}</com:pos>\
                     </com:point></com:pointGeometry></com:position>",
                    a.x, a.y
                );
                let _ = write!(
                    s,
                    "<com:addressStatus><com:status>{}</com:status></com:addressStatus>",
                    a.status
                );
                let _ = write!(s, "<com:houseNumber>{}</com:houseNumber>", a.house);
                if let Some(b) = &a.box_no {
                    let _ = write!(s, "<com:boxNumber>{b}</com:boxNumber>");
                }
                let _ = write!(
                    s,
                    "<com:officiallyAssigned>{}</com:officiallyAssigned>",
                    a.officially_assigned
                );
                let _ = write!(
                    s,
                    "<com:hasStreetName>{}</com:hasStreetName>\
                     <com:hasMunicipality>{}</com:hasMunicipality>\
                     <com:hasPostalInfo>{}</com:hasPostalInfo>",
                    triple(ns_street, &a.street_oid),
                    triple(ns_muni, &a.muni_oid),
                    triple(ns_postal, &a.postal),
                );
                if let Some(p) = &a.pom_oid {
                    let _ = write!(
                        s,
                        "<com:hasPartOfMunicipality>{}</com:hasPartOfMunicipality>",
                        triple(ns_pom, p)
                    );
                }
                s.push_str("</tns:address>");
                s
            }),
        )
    }

    /// Write every entity zip into `dir` and return the paths written.
    pub fn write(&self, dir: &Path) -> Vec<PathBuf> {
        let mut entities = vec![
            ("Municipality", self.municipality_xml()),
            ("PostalInfo", self.postal_xml()),
            ("StreetName", self.street_xml()),
            ("Address", self.address_xml()),
        ];
        if self.region == Region::Wallonia {
            entities.push(("PartOfMunicipality", self.pom_xml()));
        }
        entities
            .into_iter()
            .map(|(entity, xml)| {
                let path = dir.join(format!("{}{}20260101.zip", self.prefix(), entity));
                write_zip(&path, &format!("{}{}.xml", self.prefix(), entity), &xml);
                path
            })
            .collect()
    }
}

/// Write a zip at `path` containing one entry `entry_name` with `contents`.
pub fn write_zip(path: &Path, entry_name: &str, contents: &str) {
    let f = std::fs::File::create(path).expect("create zip");
    let mut zw = zip::ZipWriter::new(f);
    zw.start_file(entry_name, zip::write::SimpleFileOptions::default())
        .expect("zip entry");
    zw.write_all(contents.as_bytes()).expect("zip write");
    zw.finish().expect("zip finish");
}

fn wrap(root: &str, records: impl Iterator<Item = String>) -> String {
    let body: String = records.collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <tns:{root} xmlns:tns=\"urn:best:tns\" xmlns:com=\"urn:best:com\">{body}</tns:{root}>"
    )
}

fn triple(ns: &str, oid: &str) -> String {
    format!(
        "<com:namespace>{ns}</com:namespace>\
         <com:objectIdentifier>{oid}</com:objectIdentifier>\
         <com:versionIdentifier>1</com:versionIdentifier>"
    )
}

fn code(ns: &str, oid: &str) -> String {
    format!("<com:code>{}</com:code>", triple(ns, oid))
}

fn names(langs: &[(&str, &Option<String>)]) -> String {
    langs
        .iter()
        .filter_map(|(lang, v)| {
            v.as_ref().map(|s| {
                format!(
                    "<com:name><com:language>{lang}</com:language>\
                     <com:spelling>{s}</com:spelling></com:name>"
                )
            })
        })
        .collect()
}
