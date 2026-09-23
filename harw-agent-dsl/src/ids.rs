//! Namensraum-Identifikatoren für Harwness Agent Definitions (§5 DSL-Spec).
//!
//! Dieses Modul definiert die stabilen, versionierten IDs, die jede wiederverwendbare
//! Definition im gesamten Harwness-System eindeutig benennen.
//!
//! # Schlüsseltypen
//! - [`DefinitionId`] — vollständige namensraum-ID (`<namespace>.<kind>.<name>@<major>`)
//! - [`Version`] — semantische Vollversion einer Definition
//! - [`DefinitionRef`] — Verweis auf eine Definition (mit optionaler Versionsangabe)
//!
//! # Format der ID (§5)
//! ```text
//! <namespace>.<kind>.<name>@<major-version>
//! Beispiel: harwness.agent.focused-pure-coding@1
//! ```
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Send + Sync` und klonierbar.

use serde::{Deserialize, Serialize};

use crate::error::DslError;

/// Vollständige, stabile Namensraum-ID einer Harwness-Definition (§5).
///
/// # Beschreibung
/// Das Format lautet `<namespace>.<kind>.<name>@<major>`. Alle vier Felder
/// müssen nicht leer sein. Der Namespace und Kind trennen die Zuständigkeits-
/// bereiche und Typklassen; der Name identifiziert die konkrete Definition.
///
/// # Argumente
/// Keine — wird über `parse()` oder `FromStr` erzeugt.
///
/// # Fehler
/// - [`DslError::InvalidId`]: wenn die ID nicht dem Format entspricht.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::ids::DefinitionId;
///
/// let id = DefinitionId::parse("harwness.agent.focused-pure-coding@1").unwrap();
/// assert_eq!(id.namespace, "harwness");
/// assert_eq!(id.kind, "agent");
/// assert_eq!(id.name, "focused-pure-coding");
/// assert_eq!(id.major, 1);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DefinitionId {
    /// Namespace, z. B. `"harwness"` oder `"mia"`.
    pub namespace: String,
    /// Definitionstyp, z. B. `"agent"`, `"family"`, `"mixin"`.
    pub kind: String,
    /// Konkreter Name der Definition, z. B. `"focused-pure-coding"`.
    pub name: String,
    /// Major-Version für Kompatibilitätsauflösung.
    pub major: u32,
}

impl DefinitionId {
    /// Parst eine Namensraum-ID aus einem String.
    ///
    /// # Beschreibung
    /// Erwartet das Format `<namespace>.<kind>.<name>@<major>`. Alle Felder
    /// müssen nicht-leer sein.
    ///
    /// # Argumente
    /// - `s` (`&str`): der zu parsende ID-String.
    ///
    /// # Rückgabe
    /// `Ok(DefinitionId)` bei korrektem Format, sonst `Err(DslError::InvalidId)`.
    ///
    /// # Fehler
    /// - [`DslError::InvalidId`]: wenn das Format falsch oder ein Feld leer ist.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_agent_dsl::ids::DefinitionId;
    ///
    /// let id = DefinitionId::parse("mia.agent.rust-pqc-worker@1").unwrap();
    /// assert_eq!(id.major, 1);
    /// ```
    pub fn parse(s: &str) -> Result<Self, DslError> {
        // Trenne am @-Zeichen für die Version
        let (prefix, major_str) = s.split_once('@').ok_or_else(|| DslError::InvalidId {
            input: s.to_owned(),
            reason: "kein '@' gefunden; Format muss <namespace>.<kind>.<name>@<major> sein",
        })?;

        if major_str.is_empty() {
            return Err(DslError::InvalidId {
                input: s.to_owned(),
                reason: "Major-Version nach '@' ist leer",
            });
        }

        let major: u32 = major_str.parse().map_err(|_| DslError::InvalidId {
            input: s.to_owned(),
            reason: "Major-Version ist keine gültige positive Ganzzahl",
        })?;

        // prefix = <namespace>.<kind>.<name>
        // Mindestens 3 Teile nach '.'
        let parts: Vec<&str> = prefix.splitn(3, '.').collect();
        if parts.len() < 3 {
            return Err(DslError::InvalidId {
                input: s.to_owned(),
                reason: "Prefix muss genau drei Segmente haben: <namespace>.<kind>.<name>",
            });
        }

        let namespace = parts[0];
        let kind = parts[1];
        let name = parts[2];

        if namespace.is_empty() {
            return Err(DslError::InvalidId {
                input: s.to_owned(),
                reason: "Namespace darf nicht leer sein",
            });
        }
        if kind.is_empty() {
            return Err(DslError::InvalidId {
                input: s.to_owned(),
                reason: "Kind darf nicht leer sein",
            });
        }
        if name.is_empty() {
            return Err(DslError::InvalidId {
                input: s.to_owned(),
                reason: "Name darf nicht leer sein",
            });
        }

        Ok(DefinitionId {
            namespace: namespace.to_owned(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            major,
        })
    }

    /// Gibt die ID als kanonischen String zurück.
    ///
    /// # Beschreibung
    /// Erzeugt den String im Format `<namespace>.<kind>.<name>@<major>`.
    ///
    /// # Rückgabe
    /// Kanonischer `String` der ID.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_agent_dsl::ids::DefinitionId;
    ///
    /// let id = DefinitionId::parse("harwness.agent.worker-base@1").unwrap();
    /// assert_eq!(id.as_string(), "harwness.agent.worker-base@1");
    /// ```
    pub fn as_string(&self) -> String {
        format!(
            "{}.{}.{}@{}",
            self.namespace, self.kind, self.name, self.major
        )
    }
}

impl std::fmt::Display for DefinitionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_string())
    }
}

impl std::str::FromStr for DefinitionId {
    type Err = DslError;

    /// Parst eine `DefinitionId` aus einem String.
    ///
    /// # Fehler
    /// - [`DslError::InvalidId`]: wenn das Format falsch ist.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        DefinitionId::parse(s)
    }
}

impl TryFrom<String> for DefinitionId {
    type Error = DslError;

    /// Konvertiert einen `String` in eine `DefinitionId`.
    ///
    /// # Fehler
    /// - [`DslError::InvalidId`]: wenn das Format falsch ist.
    fn try_from(s: String) -> Result<Self, Self::Error> {
        DefinitionId::parse(&s)
    }
}

impl From<DefinitionId> for String {
    /// Konvertiert eine `DefinitionId` in ihren kanonischen String.
    fn from(id: DefinitionId) -> Self {
        id.as_string()
    }
}

/// Vollständige semantische Version einer Definition (semver).
///
/// # Beschreibung
/// Wrapper um [`semver::Version`] mit transparenter Serialisierung.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::ids::Version;
///
/// let v: Version = serde_json::from_str("\"1.4.0\"").unwrap();
/// assert_eq!(v.0.major, 1);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Version(pub semver::Version);

/// Verweis auf eine Harwness-Definition, optional mit expliziter Version.
///
/// # Beschreibung
/// Wird in `extends` und `mixins`-Feldern verwendet. Wenn keine `version`
/// angegeben ist, wird die neueste kompatible Version aus der Schicht gewählt.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::ids::{DefinitionId, DefinitionRef};
///
/// let r = DefinitionRef {
///     id: DefinitionId::parse("harwness.agent.worker-base@1").unwrap(),
///     version: None,
/// };
/// assert_eq!(r.id.kind, "agent");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct DefinitionRef {
    /// ID der referenzierten Definition.
    pub id: DefinitionId,
    /// Optionale explizite Versionsangabe.
    #[serde(default)]
    pub version: Option<Version>,
}

impl<'de> Deserialize<'de> for DefinitionRef {
    /// Deserialises a `DefinitionRef` from either a string or a table form.
    ///
    /// # String form
    ///
    /// ```toml
    /// extends = "harwness.agent.worker-base@1"
    /// ```
    ///
    /// The string is parsed as a [`DefinitionId`] with `version = None`.
    ///
    /// # Table form
    ///
    /// ```toml
    /// extends = { id = "harwness.agent.worker-base@1", version = "1.2.0" }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `D::Error` when:
    /// - the string form is not a valid [`DefinitionId`], or
    /// - the table form is missing `id` or has an invalid `id`/`version`.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de;

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct TableForm {
            id: DefinitionId,
            #[serde(default)]
            version: Option<Version>,
        }

        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = DefinitionRef;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a string like \"harwness.agent.worker-base@1\" or a table with `id` and optional `version` fields")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                let id = DefinitionId::parse(v).map_err(|e| {
                    de::Error::custom(format!("invalid DefinitionId in DefinitionRef string: {e}"))
                })?;
                Ok(DefinitionRef { id, version: None })
            }

            fn visit_map<M: de::MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
                let table = TableForm::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(DefinitionRef {
                    id: table.id,
                    version: table.version,
                })
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn test_parse_valid() -> TestResult {
        let id = DefinitionId::parse("harwness.agent.focused-pure-coding@1")?;
        assert_eq!(id.namespace, "harwness");
        assert_eq!(id.kind, "agent");
        assert_eq!(id.name, "focused-pure-coding");
        assert_eq!(id.major, 1);
        Ok(())
    }

    #[test]
    fn test_parse_missing_version() {
        let result = DefinitionId::parse("harwness.agent.foo");
        assert!(matches!(result, Err(DslError::InvalidId { .. })));
    }

    #[test]
    fn test_parse_invalid_kind_empty() {
        // kind ist leer: "harwness..foo@1"
        let result = DefinitionId::parse("harwness..foo@1");
        assert!(matches!(result, Err(DslError::InvalidId { .. })));
    }

    #[test]
    fn test_display_roundtrip() -> TestResult {
        let original = "mia.agent.rust-pqc-worker@2";
        let id = DefinitionId::parse(original)?;
        assert_eq!(id.to_string(), original);
        Ok(())
    }

    #[test]
    fn test_as_string_matches_display() -> TestResult {
        let id = DefinitionId::parse("harwness.family.focused-coding@1")?;
        assert_eq!(id.as_string(), id.to_string());
        Ok(())
    }

    #[test]
    fn test_from_str_trait() -> TestResult {
        let id: DefinitionId = "harwness.mixin.rust-coding@1".parse()?;
        assert_eq!(id.kind, "mixin");
        Ok(())
    }

    #[test]
    fn test_try_from_string() -> TestResult {
        let s = "harwness.policy.focused-worker@3".to_owned();
        let id = DefinitionId::try_from(s)?;
        assert_eq!(id.major, 3);
        Ok(())
    }

    #[test]
    fn test_into_string() -> TestResult {
        let id = DefinitionId::parse("harwness.agent.worker-base@1")?;
        let s: String = id.into();
        assert_eq!(s, "harwness.agent.worker-base@1");
        Ok(())
    }

    #[test]
    fn test_parse_missing_namespace() {
        let result = DefinitionId::parse(".agent.foo@1");
        assert!(matches!(result, Err(DslError::InvalidId { .. })));
    }

    #[test]
    fn test_parse_missing_name() {
        let result = DefinitionId::parse("harwness.agent.@1");
        assert!(matches!(result, Err(DslError::InvalidId { .. })));
    }

    #[test]
    fn test_parse_non_numeric_major() {
        let result = DefinitionId::parse("harwness.agent.foo@abc");
        assert!(matches!(result, Err(DslError::InvalidId { .. })));
    }

    #[test]
    fn test_definition_ref_deserialise_from_string() -> TestResult {
        let json = "\"harwness.agent.worker-base@1\"";
        let r: DefinitionRef = serde_json::from_str(json)?;
        assert_eq!(r.id.name, "worker-base");
        assert!(r.version.is_none());
        Ok(())
    }

    #[test]
    fn test_definition_ref_deserialise_from_table() -> TestResult {
        let json = r#"{"id":"harwness.agent.worker-base@1","version":"1.2.0"}"#;
        let r: DefinitionRef = serde_json::from_str(json)?;
        assert_eq!(r.id.name, "worker-base");
        assert!(r.version.is_some());
        let version = r
            .version
            .as_ref()
            .ok_or(crate::test_support::TestError::Missing("r.version"))?;
        assert_eq!(version.0.major, 1);
        Ok(())
    }

    #[test]
    fn test_definition_ref_string_form_toml() -> TestResult {
        let toml_str = r#"extends = "harwness.agent.worker-base@1""#;
        #[derive(Deserialize)]
        struct Wrapper {
            extends: DefinitionRef,
        }
        let w: Wrapper = toml::from_str(toml_str)?;
        assert_eq!(w.extends.id.name, "worker-base");
        assert!(w.extends.version.is_none());
        Ok(())
    }

    #[test]
    fn test_definition_ref_table_form_toml() -> TestResult {
        let toml_str = r#"extends = { id = "harwness.agent.worker-base@1", version = "1.2.0" }"#;
        #[derive(Deserialize)]
        struct Wrapper {
            extends: DefinitionRef,
        }
        let w: Wrapper = toml::from_str(toml_str)?;
        assert_eq!(w.extends.id.name, "worker-base");
        assert!(w.extends.version.is_some());
        Ok(())
    }

    #[test]
    fn test_serde_roundtrip() -> TestResult {
        let id = DefinitionId::parse("harwness.agent.focused-pure-coding@1")?;
        let json = serde_json::to_string(&id)?;
        let recovered: DefinitionId = serde_json::from_str(&json)?;
        assert_eq!(id, recovered);
        Ok(())
    }
}
