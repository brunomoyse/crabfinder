use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// The three official languages of Belgium.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Fr,
    Nl,
    De,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Language::Fr => "fr",
            Language::Nl => "nl",
            Language::De => "de",
        }
    }
}

impl FromStr for Language {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "fr" => Ok(Language::Fr),
            "nl" => Ok(Language::Nl),
            "de" => Ok(Language::De),
            other => Err(CoreError::InvalidLanguage(other.to_string())),
        }
    }
}

/// A name that may have FR / NL / DE variants. Per file 11 §4.1, a given entity
/// may have 0–3 `<com:name>` siblings. Brussels populates fr+nl; Flanders
/// usually nl only; Wallonia all three.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultilingualName {
    pub fr: Option<String>,
    pub nl: Option<String>,
    pub de: Option<String>,
}

impl MultilingualName {
    pub fn get(&self, lang: Language) -> Option<&str> {
        match lang {
            Language::Fr => self.fr.as_deref(),
            Language::Nl => self.nl.as_deref(),
            Language::De => self.de.as_deref(),
        }
    }

    pub fn set(&mut self, lang: Language, spelling: String) {
        match lang {
            Language::Fr => self.fr = Some(spelling),
            Language::Nl => self.nl = Some(spelling),
            Language::De => self.de = Some(spelling),
        }
    }

    /// Returns true when no language variant is populated.
    pub fn is_empty(&self) -> bool {
        self.fr.is_none() && self.nl.is_none() && self.de.is_none()
    }

    /// Returns the first populated language in FR → NL → DE order.
    /// Useful for fallback display when the caller hasn't specified a preferred language.
    pub fn primary(&self) -> Option<&str> {
        self.fr
            .as_deref()
            .or(self.nl.as_deref())
            .or(self.de.as_deref())
    }

    /// Iterate over populated (language, spelling) pairs in FR → NL → DE order.
    pub fn iter(&self) -> impl Iterator<Item = (Language, &str)> {
        [
            (Language::Fr, self.fr.as_deref()),
            (Language::Nl, self.nl.as_deref()),
            (Language::De, self.de.as_deref()),
        ]
        .into_iter()
        .filter_map(|(l, s)| s.map(|x| (l, x)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iter_skips_empty() {
        let n = MultilingualName {
            fr: Some("Avenue Louise".into()),
            nl: Some("Louizalaan".into()),
            de: None,
        };
        let collected: Vec<_> = n.iter().collect();
        assert_eq!(
            collected,
            vec![
                (Language::Fr, "Avenue Louise"),
                (Language::Nl, "Louizalaan")
            ]
        );
    }

    #[test]
    fn primary_prefers_fr_then_nl_then_de() {
        assert_eq!(
            MultilingualName {
                fr: None,
                nl: Some("Brussel".into()),
                de: Some("Brüssel".into())
            }
            .primary(),
            Some("Brussel")
        );
        assert_eq!(
            MultilingualName {
                fr: None,
                nl: None,
                de: Some("Brüssel".into())
            }
            .primary(),
            Some("Brüssel")
        );
        assert!(MultilingualName::default().primary().is_none());
    }
}
