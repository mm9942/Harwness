//! Embedding-Katalog: welches Modell wofür, wo es rechnen darf.
//!
//! # Verantwortungsbereich
//! Besitzt [`EmbeddingCatalog`], [`ModelEntry`], [`EmbeddingRole`] und
//! [`route`]. Der Katalog bindet die drei statischen Schichten
//! ([`crate::spec::EmbeddingSpec`], [`crate::descriptor::EmbeddingDescriptor`],
//! [`crate::runtime::RuntimeProfile`]) je Modell zusammen, dazu eine anfangs
//! leere [`crate::observed::ObservedBehavior`], und ordnet jedem Modell die
//! Rollen zu, für die es zuständig ist.
//!
//! `route` filtert für [`EmbeddingRole::Confidential`] **vor** der Auswahl
//! auf [`Locality::Local`]: kein vertraulicher Inhalt verlässt den Host zum
//! Einbetten. Findet sich kein lokales Profil, ist das ein Fehler
//! (Fail-Closed) -- nie ein Ausweichen auf ein entferntes Profil. Ein
//! Filter, der erst nach der Auswahl griffe, hätte den Text schon an das
//! falsche Backend gereicht.
//!
//! # Nebenläufigkeit
//! [`EmbeddingCatalog`] ist reine Daten (`Clone`) ohne interne
//! Veränderlichkeit, ohne Einschränkung zwischen Threads teilbar.
//!
//! # Fehler
//! - [`crate::error::EmbedError::CatalogParse`]: die Katalogdatei ist kein
//!   gültiges TOML oder verletzt eines der `deny_unknown_fields`-Schemata.
//! - [`crate::error::EmbedError::NoProfileForRole`]: kein Modell ist für die
//!   angefragte Rolle registriert.
//! - [`crate::error::EmbedError::NoLocalProfileForConfidential`]: es gibt
//!   Modelle für [`EmbeddingRole::Confidential`], aber keines mit
//!   [`Locality::Local`].
//!
//! # Examples
//! ```rust
//! use harw_lens_embed::{route, EmbeddingCatalog, EmbeddingRole};
//!
//! let catalog = EmbeddingCatalog::load_default().expect("embeddings.toml parses");
//! let profile = route(&catalog, EmbeddingRole::Code).expect("code role is routed");
//! assert!(!profile.backend.is_empty());
//! ```

use serde::{Deserialize, Serialize};

use crate::descriptor::EmbeddingDescriptor;
use crate::error::EmbedError;
use crate::observed::ObservedBehavior;
use crate::runtime::RuntimeProfile;
use crate::spec::EmbeddingSpec;
use harw_lens_types::Locality;

/// Die eingebettete Katalogdatei. `include_str!` bindet sie zur
/// Kompilierzeit ein, kein Lesen zur Laufzeit nötig.
const EMBEDDINGS_TOML: &str = include_str!("../embeddings.toml");

/// Wofür ein Embedding gebraucht wird.
///
/// # Description
/// Geschlossen: eine neue Rolle ist eine Entscheidung, kein freier String.
/// [`EmbeddingRole::Confidential`] trägt eine harte Garantie -- siehe
/// [`route`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbeddingRole {
    /// Quellcode.
    Code,
    /// Fließtext/Prosa.
    Prose,
    /// Abfragetext (im Gegensatz zu Dokumenttext).
    Query,
    /// Vertraulicher Inhalt. Muss lokal eingebettet werden -- siehe
    /// [`route`].
    Confidential,
}

/// Ein Modelleintrag des Katalogs: alle drei statischen Schichten plus die
/// anfangs leere gemessene Schicht.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEntry {
    /// Was das Modell ist.
    pub spec: EmbeddingSpec,
    /// Wie das Modell benutzt wird.
    pub descriptor: EmbeddingDescriptor,
    /// Was zur Laufzeit für dieses Modell gilt.
    pub runtime: RuntimeProfile,
    /// Was für dieses Modell gemessen wurde. Anfangs immer leer.
    pub observed: ObservedBehavior,
    /// Rollen, für die dieses Modell zuständig ist.
    pub roles: Vec<EmbeddingRole>,
}

/// Eine Zeile aus der Katalogdatei, vor dem Aufbau von [`ModelEntry`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogFileEntry {
    spec: EmbeddingSpec,
    descriptor: EmbeddingDescriptor,
    runtime: RuntimeProfile,
    roles: Vec<EmbeddingRole>,
}

/// Die vollständige Katalogdatei (`embeddings.toml`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogFile {
    model: Vec<CatalogFileEntry>,
}

/// Der Embedding-Katalog: welches Modell wofür, wo es rechnen darf, und wie
/// ein Text vor dem Einbetten präfixiert wird.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingCatalog {
    entries: Vec<ModelEntry>,
}

impl EmbeddingCatalog {
    /// Lädt den eingebetteten Standardkatalog aus `embeddings.toml`.
    ///
    /// # Returns
    /// Ein `EmbeddingCatalog` mit allen in der Datei beschriebenen Modellen;
    /// `observed` ist für jeden Eintrag [`ObservedBehavior::default`].
    ///
    /// # Errors
    /// - [`EmbedError::CatalogParse`]: wenn `embeddings.toml` kein gültiges
    ///   TOML ist oder ein unbekanntes Feld enthält.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_embed::EmbeddingCatalog;
    /// let catalog = EmbeddingCatalog::load_default().expect("embeddings.toml parses");
    /// assert!(!catalog.entries().is_empty());
    /// ```
    pub fn load_default() -> Result<Self, EmbedError> {
        Self::parse(EMBEDDINGS_TOML)
    }

    /// Parst eine Katalogdatei aus einem TOML-String.
    ///
    /// # Description
    /// Getrennt von [`EmbeddingCatalog::load_default`], damit Tests eigene
    /// Kataloge ohne Dateisystemzugriff parsen können.
    ///
    /// # Arguments
    /// - `toml_src` (`&str`): der TOML-Quelltext.
    ///
    /// # Returns
    /// Ein `EmbeddingCatalog` mit allen beschriebenen Modellen.
    ///
    /// # Errors
    /// - [`EmbedError::CatalogParse`]: wenn `toml_src` kein gültiges TOML
    ///   ist oder ein unbekanntes Feld enthält.
    pub fn parse(toml_src: &str) -> Result<Self, EmbedError> {
        let file: CatalogFile =
            toml::from_str(toml_src).map_err(|error| EmbedError::CatalogParse {
                reason: error.to_string(),
            })?;
        let entries = file
            .model
            .into_iter()
            .map(|entry| ModelEntry {
                spec: entry.spec,
                descriptor: entry.descriptor,
                runtime: entry.runtime,
                observed: ObservedBehavior::default(),
                roles: entry.roles,
            })
            .collect();
        Ok(Self { entries })
    }

    /// Baut einen Katalog direkt aus fertigen Einträgen, ohne TOML.
    ///
    /// # Description
    /// Für Aufrufer (insbesondere Tests), die den Router ([`route`]) gegen
    /// einen selbst zusammengestellten Katalog prüfen wollen.
    ///
    /// # Arguments
    /// - `entries` (`Vec<ModelEntry>`): die Modelleinträge des Katalogs.
    ///
    /// # Returns
    /// Ein neuer `EmbeddingCatalog` mit genau diesen Einträgen.
    #[must_use]
    pub fn from_entries(entries: Vec<ModelEntry>) -> Self {
        Self { entries }
    }

    /// Alle Modelleinträge dieses Katalogs.
    ///
    /// # Returns
    /// Ein Slice über alle registrierten [`ModelEntry`]-Werte.
    #[must_use]
    pub fn entries(&self) -> &[ModelEntry] {
        &self.entries
    }

    /// Der Modelleintrag, den [`route`] für `role` auswählen würde.
    ///
    /// # Description
    /// Für [`EmbeddingRole::Confidential`] wird die Kandidatenmenge **vor**
    /// der Auswahl auf [`Locality::Local`] gefiltert -- nicht danach. Ein
    /// Filter, der erst nach der Auswahl griffe, hätte den Text schon an
    /// das falsche Backend gereicht.
    ///
    /// # Arguments
    /// - `role` (`EmbeddingRole`): die angefragte Rolle.
    ///
    /// # Returns
    /// Der erste zur Kandidatenreihenfolge passende [`ModelEntry`].
    ///
    /// # Errors
    /// - [`EmbedError::NoLocalProfileForConfidential`] — für
    ///   `role == EmbeddingRole::Confidential`, wenn kein Kandidat
    ///   [`Locality::Local`] ist.
    /// - [`EmbedError::NoProfileForRole`]: für jede andere Rolle, wenn kein
    ///   Kandidat registriert ist.
    pub fn entry_for_role(&self, role: EmbeddingRole) -> Result<&ModelEntry, EmbedError> {
        let mut candidates = self
            .entries
            .iter()
            .filter(|entry| entry.roles.contains(&role));

        if role == EmbeddingRole::Confidential {
            candidates
                .find(|entry| entry.runtime.locality == Locality::Local)
                .ok_or(EmbedError::NoLocalProfileForConfidential)
        } else {
            candidates
                .next()
                .ok_or(EmbedError::NoProfileForRole { role })
        }
    }
}

/// Wählt das Laufzeitprofil, das für `role` zuständig ist.
///
/// # Description
/// Für [`EmbeddingRole::Confidential`] filtert diese Funktion über
/// [`EmbeddingCatalog::entry_for_role`] **vor** der Auswahl auf
/// [`Locality::Local`]: kein vertraulicher Inhalt verlässt den Host zum
/// Einbetten. Findet sich dafür kein lokales Profil, ist das ein Fehler,
/// kein Ausweichen auf ein entferntes.
///
/// # Arguments
/// - `catalog` (`&EmbeddingCatalog`): der zu befragende Katalog.
/// - `role` (`EmbeddingRole`): die angefragte Rolle.
///
/// # Returns
/// Das [`RuntimeProfile`] des ausgewählten Modells.
///
/// # Errors
/// - [`EmbedError::NoLocalProfileForConfidential`] — siehe
///   [`EmbeddingCatalog::entry_for_role`].
/// - [`EmbedError::NoProfileForRole`] — siehe
///   [`EmbeddingCatalog::entry_for_role`].
///
/// # Examples
/// ```rust
/// use harw_lens_embed::{route, EmbeddingCatalog, EmbeddingRole};
///
/// let catalog = EmbeddingCatalog::load_default().expect("embeddings.toml parses");
/// let profile = route(&catalog, EmbeddingRole::Confidential).expect("a local profile exists");
/// assert_eq!(profile.locality, harw_lens_types::Locality::Local);
/// ```
pub fn route(
    catalog: &EmbeddingCatalog,
    role: EmbeddingRole,
) -> Result<&RuntimeProfile, EmbedError> {
    catalog.entry_for_role(role).map(|entry| &entry.runtime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn sample_catalog_toml() -> &'static str {
        r#"
            [[model]]
            roles = ["code", "confidential"]

            [model.spec]
            name = "local-minilm-l6-v2"
            dimensions = 384
            metric = "cosine"
            max_input_chars = 2048

            [model.descriptor]
            document_prefix = "passage: "
            query_prefix = "query: "
            normalize = true

            [model.runtime]
            backend = "onnx-local"
            locality = "local"
            batch_size = 32

            [[model]]
            roles = ["prose", "query"]

            [model.spec]
            name = "remote-embed-3-large"
            dimensions = 3072
            metric = "cosine"
            max_input_chars = 8192

            [model.descriptor]
            document_prefix = "passage: "
            query_prefix = "query: "
            normalize = false

            [model.runtime]
            backend = "http-provider-catalog"
            locality = "remote"
            batch_size = 64
        "#
    }

    #[test]
    fn test_load_default_parses_embedded_toml() -> TestResult {
        let catalog = EmbeddingCatalog::load_default().map_err(ctx("embeddings.toml parses"))?;
        assert!(!catalog.entries().is_empty());
        Ok(())
    }

    #[test]
    fn test_parse_valid_catalog_succeeds() -> TestResult {
        let catalog = EmbeddingCatalog::parse(sample_catalog_toml()).map_err(ctx("parses"))?;
        assert_eq!(catalog.entries().len(), 2);
        Ok(())
    }

    #[test]
    fn test_parse_rejects_unknown_top_level_field() {
        // Vorangestellt, damit das Feld vor dem ersten `[[model]]`-Header
        // und damit auf der Wurzelebene von `CatalogFile` landet -- nicht
        // in einer verschachtelten `model.*`-Tabelle.
        let toml_src = format!("unknown_field = true\n{}", sample_catalog_toml());
        assert!(EmbeddingCatalog::parse(&toml_src).is_err());
    }

    #[test]
    fn test_parse_rejects_unknown_field_inside_model_entry() {
        let toml_src = r#"
            [[model]]
            roles = ["code"]
            unexpected = "nope"

            [model.spec]
            name = "m"
            dimensions = 8
            metric = "cosine"
            max_input_chars = 100

            [model.descriptor]
            document_prefix = ""
            query_prefix = ""
            normalize = false

            [model.runtime]
            backend = "onnx-local"
            locality = "local"
            batch_size = 1
        "#;
        assert!(EmbeddingCatalog::parse(toml_src).is_err());
    }

    #[test]
    fn test_route_confidential_returns_local_profile() -> TestResult {
        let catalog = EmbeddingCatalog::parse(sample_catalog_toml()).map_err(ctx("parses"))?;
        let profile =
            route(&catalog, EmbeddingRole::Confidential).map_err(ctx("local profile exists"))?;
        assert_eq!(profile.locality, Locality::Local);
        Ok(())
    }

    #[test]
    fn test_route_confidential_fails_without_local_profile() -> TestResult {
        let toml_src = r#"
            [[model]]
            roles = ["confidential"]

            [model.spec]
            name = "remote-only"
            dimensions = 768
            metric = "cosine"
            max_input_chars = 4096

            [model.descriptor]
            document_prefix = "passage: "
            query_prefix = "query: "
            normalize = false

            [model.runtime]
            backend = "http-provider-catalog"
            locality = "remote"
            batch_size = 16
        "#;
        let catalog = EmbeddingCatalog::parse(toml_src).map_err(ctx("parses"))?;
        assert_eq!(
            route(&catalog, EmbeddingRole::Confidential),
            Err(EmbedError::NoLocalProfileForConfidential)
        );
        Ok(())
    }

    #[test]
    fn test_route_returns_err_for_role_with_no_candidates() -> TestResult {
        let toml_src = r#"
            [[model]]
            roles = ["code"]

            [model.spec]
            name = "local-only"
            dimensions = 384
            metric = "cosine"
            max_input_chars = 2048

            [model.descriptor]
            document_prefix = "passage: "
            query_prefix = "query: "
            normalize = true

            [model.runtime]
            backend = "onnx-local"
            locality = "local"
            batch_size = 32
        "#;
        let catalog = EmbeddingCatalog::parse(toml_src).map_err(ctx("parses"))?;
        assert_eq!(
            route(&catalog, EmbeddingRole::Query),
            Err(EmbedError::NoProfileForRole {
                role: EmbeddingRole::Query
            })
        );
        Ok(())
    }

    #[test]
    fn test_route_confidential_filters_before_picking_first_match() -> TestResult {
        // Der erste gelistete Kandidat ist entfernt; nur der zweite ist
        // lokal. Würde der Router zuerst "erster Treffer" wählen und danach
        // filtern, gäbe er fälschlich das entfernte Profil zurück.
        let toml_src = r#"
            [[model]]
            roles = ["confidential"]

            [model.spec]
            name = "remote-first"
            dimensions = 768
            metric = "cosine"
            max_input_chars = 4096

            [model.descriptor]
            document_prefix = "passage: "
            query_prefix = "query: "
            normalize = false

            [model.runtime]
            backend = "http-provider-catalog"
            locality = "remote"
            batch_size = 16

            [[model]]
            roles = ["confidential"]

            [model.spec]
            name = "local-second"
            dimensions = 384
            metric = "cosine"
            max_input_chars = 2048

            [model.descriptor]
            document_prefix = "passage: "
            query_prefix = "query: "
            normalize = true

            [model.runtime]
            backend = "onnx-local"
            locality = "local"
            batch_size = 32
        "#;
        let catalog = EmbeddingCatalog::parse(toml_src).map_err(ctx("parses"))?;
        let profile =
            route(&catalog, EmbeddingRole::Confidential).map_err(ctx("local profile exists"))?;
        assert_eq!(profile.backend, "onnx-local");
        assert_eq!(profile.locality, Locality::Local);
        Ok(())
    }

    #[test]
    fn test_embedding_role_deserializes_kebab_case_from_toml() -> TestResult {
        // Bestätigt, dass die `roles`-Liste (z. B. "confidential") über die
        // `#[serde(rename_all = "kebab-case")]`-Kodierung geparst wird, die
        // auch `embeddings.toml` verwendet.
        #[derive(Deserialize)]
        struct Wrapper {
            roles: Vec<EmbeddingRole>,
        }
        let wrapper: Wrapper =
            toml::from_str(r#"roles = ["confidential"]"#).map_err(ctx("parses"))?;
        assert_eq!(wrapper.roles, vec![EmbeddingRole::Confidential]);
        Ok(())
    }
}
