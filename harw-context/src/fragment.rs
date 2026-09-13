//! Fragmente, Vertrauensklassen und ihre Namensräume.
//!
//! # Verantwortungsbereich
//! Besitzt [`TrustClass`], [`Stability`], [`FragmentOrigin`], [`Fragment`],
//! [`FragmentLabel`] und [`SectionName`] — das Vokabular für „ein Stück
//! Kontext mit allem, was die Montage über es wissen muss" (Contract-Master
//! AW0 §E). Diese Datei trifft keine Montage-Entscheidung; sie definiert nur,
//! *was* ein Fragment ist, nicht *ob* es verwendet wird.
//!
//! # Die Vertrauensfalle
//! [`TrustClass`] leitet `PartialOrd`/`Ord` aus der Deklarationsreihenfolge
//! ab (`Instruction < Evidence < Data`, Rust-Standardverhalten für
//! `#[derive(Ord)]` auf einem Enum). Diese abgeleitete Ordnung ist **nicht**
//! die Vertrauensordnung. „Höher" im Sinn von `ContextCeiling::max_trust`
//! bedeutet *vertrauenswürdiger*, und `Instruction` ist die vertrauens-
//! würdigste Klasse (die einzige, die im Instruktionsblock erscheinen darf),
//! nicht `Data`. Wer stattdessen die abgeleitete `Ord`-Instanz für einen
//! Vertrauensvergleich verwendet, vergleicht in die falsche Richtung.
//! [`TrustClass::trust_rank`] ist der einzige korrekte Weg, „höher" im
//! Vertrauenssinn auszudrücken.
//!
//! # Exportierte Typen
//! [`TrustClass`], [`Stability`], [`FragmentOrigin`], [`Fragment`],
//! [`FragmentLabel`], [`SectionName`].
//!
//! # Nebenläufigkeit
//! Reine, unveränderliche Werttypen: `Send + Sync`, kein `Rc`/`RefCell`.
//!
//! # Fehler
//! [`crate::error::ContextError`] beim Konstruieren eines [`SectionName`]
//! oder [`FragmentLabel`] aus einem leeren oder steuerzeichenhaltigen String.
//!
//! # Examples
//! ```rust
//! use harw_context::{FragmentLabel, SectionName, TrustClass};
//!
//! let section = SectionName::try_new("history.tail").unwrap();
//! let label = FragmentLabel::try_new("turn-42").unwrap();
//! assert_eq!(section.as_str(), "history.tail");
//! assert_eq!(label.as_str(), "turn-42");
//! assert!(TrustClass::Instruction.trust_rank() > TrustClass::Data.trust_rank());
//! ```

use crate::error::{validate_name, ContextError};

/// Vertrauensklasse eines Fragments. **Drei Werte, geschlossen.**
///
/// # Description
/// Die Klasse entscheidet, in welchen Block ein Fragment gerendert wird.
/// `Instruction` ist die einzige Klasse, die im Instruktionsblock erscheinen
/// darf — sie ist damit die *vertrauenswürdigste* Klasse, nicht die
/// „größte" im Sinn der abgeleiteten `Ord`-Instanz. Siehe die Modul-
/// dokumentation (Abschnitt „Die Vertrauensfalle") und [`Self::trust_rank`].
///
/// Die abgeleitete `PartialOrd`/`Ord`-Instanz folgt der Deklarations-
/// reihenfolge (`Instruction < Evidence < Data`) und ist für Sortierungen
/// gedacht, die keine Vertrauensaussage treffen (z. B. eine stabile,
/// deterministische Anzeigereihenfolge). Für jeden Vertrauensvergleich —
/// insbesondere `ContextCeiling::admits` — ist ausschließlich
/// [`Self::trust_rank`] zu verwenden.
/// **Bewusst ohne `PartialOrd`/`Ord`.** Die Deklarationsreihenfolge
/// (`Instruction`, `Evidence`, `Data`) ergäbe eine abgeleitete Ordnung, die
/// der gemeinten **exakt entgegengesetzt** ist: `Instruction < Data`, während
/// `Instruction` die *vertrauenswürdigste* Klasse ist.
///
/// Die Moduldokumentation warnte davor, und ein Test ist trotzdem
/// hineingelaufen -- `assert!(TrustClass::Data < TrustClass::Evidence)` in
/// einer sicherheitsrelevanten Zusicherung über den Vorgabewert für
/// Kontextanbieter. Er war inhaltlich richtig und mechanisch falsch.
///
/// Statt die Warnung zu wiederholen, ist der Vergleich jetzt **nicht mehr
/// ausdrückbar**: wer Vertrauenswürdigkeit vergleichen will, muss
/// [`TrustClass::trust_rank`] nehmen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustClass {
    Instruction,
    Evidence,
    Data,
}

impl TrustClass {
    /// Rang der Vertrauenswürdigkeit: **höherer Wert = vertrauenswürdiger.**
    ///
    /// # Description
    /// Absichtlich unabhängig von der abgeleiteten `Ord`-Instanz auf
    /// `TrustClass` (siehe Moduldokumentation). `ContextCeiling::admits`
    /// vergleicht ausschließlich über diese Funktion, nie über `<`/`>` auf
    /// `TrustClass` selbst.
    ///
    /// # Returns
    /// `2` für [`TrustClass::Instruction`] (am vertrauenswürdigsten), `1`
    /// für [`TrustClass::Evidence`], `0` für [`TrustClass::Data`] (am
    /// wenigsten vertrauenswürdig).
    ///
    /// # Examples
    /// ```rust
    /// use harw_context::TrustClass;
    /// assert!(TrustClass::Instruction.trust_rank() > TrustClass::Evidence.trust_rank());
    /// assert!(TrustClass::Evidence.trust_rank() > TrustClass::Data.trust_rank());
    /// ```
    #[must_use]
    pub const fn trust_rank(self) -> u8 {
        match self {
            TrustClass::Instruction => 2,
            TrustClass::Evidence => 1,
            TrustClass::Data => 0,
        }
    }
}

/// Wie beständig ein Fragment über Turns hinweg ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stability {
    Pinned,
    Stable,
    Fresh,
    Volatile,
}

/// Wer ein Fragment wann geliefert hat.
///
/// # Description
/// **Heißt bewusst nicht `Provenance`:** diesen Namen besitzt bereits
/// `harw_memory::epistemic::Provenance` — dort beantwortet der Typ die
/// Frage „woher weiß ich das" (epistemische Herkunft eines Glaubens). Dieser
/// Typ beantwortet eine andere Frage: „welcher Provider hat das wann
/// geliefert" (Lieferkette eines Fragments in die Montage hinein). Zwei
/// inhaltlich verschiedene Typen mit demselben Namen wären der Anfang der
/// nächsten Verwechslung — deshalb der eigene Name `FragmentOrigin`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FragmentOrigin {
    pub provider: String,
    pub namespace: String,
    pub produced_at: jiff::Timestamp,
}

/// Ein Stück Kontext mit allem, was die Montage über es wissen muss.
///
/// # Description
/// Reiner Datenträger ohne Montagelogik (die lebt in `Assembly<...>` in
/// `harw-core`, Knoten AW1-03). `cost` und `digest` verweisen auf die in
/// `harw-lens-types` bzw. `harw-types` festgelegten Kostenschätzungs- und
/// Inhaltsadressierungstypen, damit ein Fragment-Digest und ein anderer
/// 32-Byte-Digest nie versehentlich verwechselt werden können.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fragment {
    pub label: FragmentLabel,
    pub section: SectionName,
    pub trust: TrustClass,
    pub stability: Stability,
    pub origin: FragmentOrigin,
    pub cost: harw_lens_types::CostEstimate,
    pub digest: harw_types::ContentDigest,
    pub body: String,
}

/// Name einer Kontextsektion, etwa `history.tail` oder `plan.current`.
///
/// # Description
/// Newtype über `String` mit Validierung: leer oder steuerzeichenhaltig ist
/// kein gültiger Sektionsname. Nur über [`Self::try_new`] konstruierbar, da
/// der Kontrakt keinen infalliblen Konstruktor vorsieht.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct SectionName(String);

impl SectionName {
    /// Konstruiert einen Sektionsnamen aus einem beliebigen `String`-artigen
    /// Wert.
    ///
    /// # Arguments
    /// - `name` (`impl Into<String>`): der Rohname, z. B. `"history.tail"`.
    ///
    /// # Returns
    /// `Ok(Self)`, wenn der getrimmte Name nicht leer ist und kein
    /// Steuerzeichen enthält.
    ///
    /// # Errors
    /// [`ContextError::InvalidName`] mit `kind = "section"`, wenn der Name
    /// leer oder steuerzeichenhaltig ist.
    ///
    /// # Examples
    /// ```rust
    /// use harw_context::SectionName;
    /// assert!(SectionName::try_new("history.tail").is_ok());
    /// assert!(SectionName::try_new("").is_err());
    /// ```
    pub fn try_new(name: impl Into<String>) -> Result<Self, ContextError> {
        validate_name("section", name).map(Self)
    }

    /// Borrowt den Sektionsnamen als String-Slice.
    ///
    /// # Returns
    /// Der Name ohne Kopie.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SectionName {
    /// Schreibt den rohen Sektionsnamen ohne Anführungszeichen oder
    /// Typ-Wrapper — geeignet für Fehlermeldungen wie
    /// [`crate::ceiling::CeilingViolation`].
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Kennung eines einzelnen Fragments innerhalb seiner Sektion.
///
/// # Description
/// Newtype über `String` mit derselben Validierung wie [`SectionName`]. Nur
/// über [`Self::try_new`] konstruierbar.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FragmentLabel(String);

impl FragmentLabel {
    /// Konstruiert ein Fragment-Label aus einem beliebigen `String`-artigen
    /// Wert.
    ///
    /// # Arguments
    /// - `label` (`impl Into<String>`): das Roh-Label, z. B. `"turn-42"`.
    ///
    /// # Returns
    /// `Ok(Self)`, wenn das getrimmte Label nicht leer ist und kein
    /// Steuerzeichen enthält.
    ///
    /// # Errors
    /// [`ContextError::InvalidName`] mit `kind = "fragment label"`, wenn das
    /// Label leer oder steuerzeichenhaltig ist.
    ///
    /// # Examples
    /// ```rust
    /// use harw_context::FragmentLabel;
    /// assert!(FragmentLabel::try_new("turn-42").is_ok());
    /// assert!(FragmentLabel::try_new("").is_err());
    /// ```
    pub fn try_new(label: impl Into<String>) -> Result<Self, ContextError> {
        validate_name("fragment label", label).map(Self)
    }

    /// Borrowt das Fragment-Label als String-Slice.
    ///
    /// # Returns
    /// Das Label ohne Kopie.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FragmentLabel {
    /// Schreibt das rohe Label ohne Anführungszeichen oder Typ-Wrapper.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{Fragment, FragmentLabel, FragmentOrigin, SectionName, Stability, TrustClass};
    use crate::error::ContextError;

    #[test]
    fn test_trust_rank_orders_instruction_above_evidence_above_data() {
        assert!(TrustClass::Instruction.trust_rank() > TrustClass::Evidence.trust_rank());
        assert!(TrustClass::Evidence.trust_rank() > TrustClass::Data.trust_rank());
    }

    /// Die Vertrauensfalle ist nicht mehr ausdrückbar.
    ///
    /// Hier stand zuvor ein Test, der die Diskrepanz **festhielt**: die
    /// abgeleitete `Ord` folgte der Deklarationsreihenfolge und war damit der
    /// Vertrauensrichtung entgegengesetzt, und der Test pinnte das, „damit es
    /// nicht versehentlich korrigiert wird".
    ///
    /// Die Absicht war richtig, das Mittel zu schwach. Ein späterer Test in
    /// `harw-extension-api` schrieb `assert!(TrustClass::Data <
    /// TrustClass::Evidence)` — inhaltlich richtig, mechanisch falsch herum,
    /// und das in der Zusicherung über den Vorgabewert für Kontextanbieter.
    /// Eine dokumentierte Falle bleibt eine Falle.
    ///
    /// `TrustClass` trägt deshalb **kein `PartialOrd`/`Ord`** mehr. Wer
    /// Vertrauenswürdigkeit vergleichen will, muss [`TrustClass::trust_rank`]
    /// nehmen — und der zeigt in die richtige Richtung.
    #[test]
    fn test_trust_comparison_goes_through_trust_rank_only() {
        assert_eq!(TrustClass::Instruction.trust_rank(), 2);
        assert_eq!(TrustClass::Evidence.trust_rank(), 1);
        assert_eq!(TrustClass::Data.trust_rank(), 0);
        // Instruction ist die vertrauenswürdigste Klasse — obwohl sie in der
        // Deklaration zuerst steht.
        assert!(TrustClass::Instruction.trust_rank() > TrustClass::Data.trust_rank());
    }

    #[test]
    fn test_section_name_try_new_rejects_empty() {
        assert!(matches!(
            SectionName::try_new(""),
            Err(ContextError::InvalidName { .. })
        ));
    }

    #[test]
    fn test_section_name_try_new_accepts_valid_name() {
        let section = SectionName::try_new("history.tail").unwrap();
        assert_eq!(section.as_str(), "history.tail");
        assert_eq!(section.to_string(), "history.tail");
    }

    #[test]
    fn test_fragment_label_try_new_rejects_control_character() {
        assert!(matches!(
            FragmentLabel::try_new("turn\u{0007}42"),
            Err(ContextError::InvalidName { .. })
        ));
    }

    #[test]
    fn test_fragment_label_try_new_accepts_valid_label() {
        let label = FragmentLabel::try_new("turn-42").unwrap();
        assert_eq!(label.as_str(), "turn-42");
    }

    fn sample_fragment() -> Fragment {
        Fragment {
            label: FragmentLabel::try_new("turn-42").unwrap(),
            section: SectionName::try_new("history.tail").unwrap(),
            trust: TrustClass::Evidence,
            stability: Stability::Stable,
            origin: FragmentOrigin {
                provider: "harw-lens".to_owned(),
                namespace: "default".to_owned(),
                produced_at: jiff::Timestamp::UNIX_EPOCH,
            },
            cost: harw_lens_types::CostEstimate(12),
            digest: harw_types::ContentDigest::of(b"hello context"),
            body: "hello context".to_owned(),
        }
    }

    #[test]
    fn test_fragment_serde_roundtrip() {
        let fragment = sample_fragment();
        let json = serde_json::to_string(&fragment).expect("fragment must serialize");
        let restored: Fragment =
            serde_json::from_str(&json).expect("fragment must deserialize");
        assert_eq!(fragment, restored);
    }

    #[test]
    fn test_fragment_deserialize_rejects_unknown_field() {
        let fragment = sample_fragment();
        let mut value = serde_json::to_value(&fragment).expect("fragment must serialize to value");
        value
            .as_object_mut()
            .expect("fragment serializes to an object")
            .insert("unexpected".to_owned(), serde_json::json!(true));

        let result: Result<Fragment, _> = serde_json::from_value(value);
        assert!(result.is_err());
    }
}
