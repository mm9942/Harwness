//! Baseline-Gate: welche Befund-Art eine Baseline je nach Lebenszyklus zulässt.
//!
//! # Der geschnittene Befund und was er mitbrachte
//! Ein Knoten hatte `harw-dod-rules` in `harw-sentinel` verdrahtet und dabei
//! **nachgezählt**: die Kante `harw-sentinel → harw-dod-rules → harw-knowledge`
//! brachte **21 neue Einträge** in den Abhängigkeitsbaum eines
//! unprivilegierten Sicherheitssensors — darunter, transitiv über
//! `harw-knowledge → harw-model-catalog`, einen vollständigen asynchronen
//! HTTP-Client-Stack (`reqwest` + `tokio`), obwohl keine einzige Regel dieser
//! Crate je ein Netz anspricht. **Ein Sicherheitssensor braucht keinen
//! HTTP-Client.** Die Ursache war minimal: dieses Modul reexportierte zuvor
//! `harw_knowledge::security::Baseline` und
//! `harw_knowledge::memory::palace::PalaceStatus` unverändert — zwei
//! Typreexporte, die die gesamte Wissensbasis samt ihrer schweren
//! Abhängigkeiten (`harw-model-catalog`, `harw-agent-dsl`, `harw-config`,
//! `harw-context`, `harw-job-runtime`, `harw-lens-types`, `harw-plan`,
//! `harw-session-store` und die externen Crates `fs4`, `ipnet`, `reqwest`,
//! `secrecy`, `serde_norway`, `tempfile`, `time`, `tokio`, `tracing`) in
//! dieses Binary zogen.
//!
//! # Der gewählte Weg — und warum die anderen nicht
//! **Weg 1 wurde gewählt: die Typen wandern nach unten.** `Baseline` und
//! `PalaceStatus` sind ab hier **lokal in `harw-dod-rules` definierte**
//! Typen — reine Daten, die eine Regel liest, keine Wissensbasis-Artefakte.
//! `harw-knowledge` konvertiert seinerseits (siehe
//! `harw_knowledge::security::Baseline::to_rule_baseline`) von seinem
//! eigenen, durablen `Baseline`-Artefakt in diesen leichten Typ — dieselbe
//! Form wie Korrektur K3 (`Confidence` wanderte nach `harw-types`, beide
//! Seiten reexportieren/konvertieren), nur mit vertauschten Rollen: hier ist
//! `harw-dod-rules` die fundamentale, dünn bezogene Seite, `harw-knowledge`
//! die schwere, aggregierende Seite, die auf das dünne Vokabular abbildet
//! statt es zu importieren.
//!
//! **Kantenrichtung geprüft:** vor dieser Änderung hing `harw-knowledge`
//! nicht von `harw-dod-rules` ab (die alte `Cargo.toml`-Notiz an dieser
//! Stelle sagte das ausdrücklich und hatte es geprüft). Die Umkehrung
//! (`harw-knowledge → harw-dod-rules`, produktiv, für die neue Konvertierung)
//! ist deshalb zyklusfrei: `harw-dod-rules` hängt seinerseits nur noch **als
//! `[dev-dependencies]`-Eintrag** an `harw-knowledge` (siehe unten), und
//! Dev-Dependency-Zyklen sind in Cargo ausdrücklich unterstützt — sie fließen
//! nie in den produktiven Abhängigkeitsgraphen ein, den `harw-sentinel`
//! baut.
//!
//! Nicht `id: harw_knowledge::artifact::ArtifactId` übernommen, obwohl der
//! unveränderliche Test in `crate::rules::baseline_deviation` genau einen
//! solchen Wert konstruiert und durchreicht: [`Baseline::new`] nimmt den
//! Bezeichner als `impl std::fmt::Display` entgegen und speichert nur die
//! formatierte Zeichenkette. `ArtifactId` erfüllt diese Schranke bereits
//! (siehe `harw_knowledge::id_newtype!`), ohne dass dieses Modul den Typ
//! oder die Crate außerhalb von Tests je beim Namen nennen muss — die
//! produktive Signatur bleibt also vollständig frei von `harw-knowledge`.
//!
//! **Weg 2 (ein `BaselineSource`-Trait) wurde erwogen und verworfen:** er
//! hätte dieselbe Kante nur hinter einer Abstraktion versteckt, sofern der
//! injizierte `BaselineSource`-Implementierer weiterhin
//! `harw_knowledge::security::Baseline` zurückgibt — die schwere Kante bliebe
//! bestehen, nur eine Ebene tiefer. Ein Trait lohnt sich erst, wenn es
//! mehrere echte Implementierungen gibt; hier gibt es (bislang) nur eine
//! Quelle, die Wissensbasis, und die kann direkt konvertieren.
//!
//! **Weg 3 (ein Feature-Schalter) wurde verworfen, weil er die schwächste
//! Antwort ist:** er hätte die Frage nur verschoben, nicht beantwortet — das
//! Standardprofil eines Cargo-Features hätte entschieden, ob `reqwest`/
//! `tokio` im Baum landen, und `harw-sentinel` hätte aktiv daran denken
//! müssen, das Feature **nicht** zu aktivieren. Ein unprivilegiertes Binary
//! soll diese Last nicht tragen; der Ausschluss soll strukturell sein, nicht
//! durch eine vergessene Flag.
//!
//! # Was nach dieser Änderung übrig bleibt
//! Von den ursprünglich 21 Einträgen bleiben in `harw-sentinel`s
//! Abhängigkeitsbaum ausschließlich diejenigen erhalten, die `harw-dod-rules`
//! unabhängig von dieser Kante ohnehin schon mitbrachte oder die
//! `harw-sentinel` bereits direkt bezieht: `harw-sandbox` (weiterhin
//! `[dependencies]` von `harw-dod-rules`, für `EgressFlowRule`, **und**
//! bereits direkt in `harw-sentinel/Cargo.toml` für `NetworkScope::empty()`
//! gelistet) und `harw-research` (weiterhin `[dependencies]`, für
//! `epistemic_confidence_for`, hängt selbst nur an `harw-macros`/
//! `harw-types`/serde/jiff). `harw-knowledge` selbst — und damit
//! `harw-model-catalog`, `harw-agent-dsl`, `harw-config`, `harw-context`,
//! `harw-job-runtime`, `harw-lens-types`, `harw-plan`, `harw-session-store`
//! sowie `fs4`, `ipnet`, `reqwest`, `secrecy`, `serde_norway`, `tempfile`,
//! `time`, `tokio` — erreichen `harw-sentinel` nach dieser Änderung **nicht
//! mehr**: `harw-knowledge` ist in `harw-dod-rules/Cargo.toml` von
//! `[dependencies]` nach `[dev-dependencies]` verschoben (gebraucht nur noch
//! für den Doctest/Test in `crate::rules::baseline_deviation`, die
//! `harw_knowledge::artifact::ArtifactId` zur Bequemlichkeit konstruieren),
//! und Dev-Dependencies eines Pfadglieds fließen nie in den Baum eines
//! Binaries ein, das dieses Glied nur produktiv einbindet. `tracing` bleibt
//! über `harw-sentinel`s eigene direkte Abhängigkeit erhalten — unabhängig
//! von dieser Kante.
//!
//! # Die Regel
//! [`finding_kind_for_status`] ist die eine Stelle, an der die Baseline-Regel
//! kodiert ist: `Established → Some(FindingKind::RuleTriggered)`,
//! `Provisional → Some(FindingKind::Anomaly)`, `Superseded → None` — eine
//! zurückgezogene Baseline gate't nichts mehr, weder eskalationswürdig noch
//! als Anomalie; sie ist Historie, kein aktiver Bezugspunkt. Jede Regel
//! dieser Crate, die Baselines konsumiert (siehe
//! `crate::rules::baseline_deviation`), ruft ausschließlich diese Funktion
//! auf, statt die Fallunterscheidung selbst zu wiederholen.
//!
//! [`PalaceStatus`] ist — wie sein Namensvetter in `harw-knowledge` — ein
//! **Lebenszyklus**, keine Konfidenzskala: `Established` ist nicht „mehr
//! Konfidenz" als `Provisional`, sondern eine spätere Phase in derselben
//! Promotion-Kette. Diese Datei bildet ihn auf keine Konfidenzskala ab und
//! führt keine Konvertierung dafür ein — `epistemic_confidence_for`
//! (die einzige Konfidenz-Konvertierung dieser Crate, Quelle
//! `harw_research::Confidence`) bleibt unverändert in `crate::confidence` und
//! berührt diesen Typ nicht.
//!
//! # Nebenläufigkeit
//! [`finding_kind_for_status`] ist eine reine, totale `fn` ohne inneren
//! Zustand: `Send + Sync`, beliebig aus mehreren Threads aufrufbar.
//! [`Baseline`] und [`PalaceStatus`] sind reine `Send + Sync`-Daten ohne
//! geteilten Zustand.
//!
//! # Fehler
//! [`Baseline::promote_to_established`] liefert [`BaselineError`], wenn eine
//! Promotion ohne Review versucht wird — siehe dort.
//!
//! # Examples
//! ```rust
//! use harw_dod_rules::baseline::{finding_kind_for_status, PalaceStatus};
//! use harw_dod_rules::FindingKind;
//!
//! assert_eq!(
//!     finding_kind_for_status(PalaceStatus::Established),
//!     Some(FindingKind::RuleTriggered)
//! );
//! assert_eq!(
//!     finding_kind_for_status(PalaceStatus::Provisional),
//!     Some(FindingKind::Anomaly)
//! );
//! assert_eq!(finding_kind_for_status(PalaceStatus::Superseded), None);
//! ```

use std::fmt;

use harw_dod_signals::{Hardness, SecurityEvidence};

use crate::finding::FindingKind;

/// Lebenszyklus-Status einer [`Baseline`] (lokal zu `harw-dod-rules`).
///
/// # Warum dieser Typ nicht aus `harw-knowledge` reexportiert wird
/// Siehe Moduldoku, Abschnitt „Der gewählte Weg". Dieselben drei Phasen wie
/// `harw_knowledge::memory::palace::PalaceStatus` (dort das kanonische
/// Original für die Wissensbasis), hier eine eigenständige, dünn bezogene
/// Kopie für die Regelseite. **Keine Konfidenzskala** — siehe dortige
/// Moduldoku für die volle Begründung, warum eine Lebenszyklus-Stufe keine
/// graduelle Konfidenz ist. Dieser Typ wird auf keine Konfidenzskala
/// abgebildet und nicht in eine umbenannt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PalaceStatus {
    /// Durable truth, die eine Regel direkt auslösen darf.
    Established,
    /// Beobachtet, aber noch nicht Review-gehärtet — nur `Anomaly`.
    Provisional,
    /// Zurückgezogen: kein aktiver Bezugspunkt mehr, gate't nichts.
    Superseded,
}

/// Eine eingefrorene Verhaltens-Baseline, gegen die eine Regel neue
/// Beobachtungen prüft (lokal zu `harw-dod-rules`; siehe Moduldoku).
///
/// # Warum `status` bestimmt, was diese Baseline auslösen darf
/// Eine `Established`-Baseline ist vertrauenswürdig genug, um eine Regel
/// direkt auszulösen; eine `Provisional`-Baseline darf nur eine Anomalie
/// für einen Menschen melden. Ob eine Abweichung von dieser Baseline
/// handlungsrelevant oder nur informativ ist, liest also direkt aus
/// `status` — siehe [`finding_kind_for_status`].
#[derive(Debug, Clone, PartialEq)]
pub struct Baseline {
    /// Stabiler Bezeichner, als formatierte Zeichenkette gespeichert (siehe
    /// [`Baseline::new`] für die Begründung des `impl Display`-Parameters).
    pub id: String,
    /// Menschenlesbare Beschreibung dessen, was diese Baseline
    /// charakterisiert.
    pub title: String,
    /// Epistemische Distanz der Beweise, aus denen diese Baseline abgeleitet
    /// wurde.
    pub hardness: Hardness,
    /// Die eingefrorenen Beobachtungen, aus denen diese Baseline abgeleitet
    /// wurde.
    pub evidence: SecurityEvidence,
    /// Lebenszyklus-Status: gate't, ob eine Regel `RuleTriggered`
    /// (`Established`) oder nur `Anomaly` (`Provisional`) auslösen darf.
    pub status: PalaceStatus,
    /// Thematische Tags, unabhängig vom Verweisgraphen.
    pub tags: Vec<String>,
}

impl Baseline {
    /// Erstellt eine frische, `Provisional` gestartete Baseline.
    ///
    /// # Description
    /// `id` ist bewusst `impl std::fmt::Display` statt eines konkreten
    /// Bezeichnertyps: das erlaubt Aufrufern, jeden anzeigbaren Bezeichner
    /// durchzureichen (etwa `harw_knowledge::artifact::ArtifactId`, das
    /// `Display` implementiert), ohne dass diese Signatur dessen Crate
    /// produktiv referenzieren muss — siehe Moduldoku.
    ///
    /// # Arguments
    /// - `id` (`impl std::fmt::Display`): Bezeichner, formatiert und als
    ///   `String` gespeichert.
    /// - `title` (`impl Into<String>`): menschenlesbare Beschreibung.
    /// - `hardness` (`Hardness`): epistemische Distanz der Beweise.
    /// - `evidence` (`SecurityEvidence`): die eingefrorenen Beobachtungen.
    ///
    /// # Returns
    /// Eine neue [`Baseline`] mit `status: PalaceStatus::Provisional` und
    /// leeren `tags`.
    #[must_use]
    pub fn new(
        id: impl fmt::Display,
        title: impl Into<String>,
        hardness: Hardness,
        evidence: SecurityEvidence,
    ) -> Self {
        Self {
            id: id.to_string(),
            title: title.into(),
            hardness,
            evidence,
            status: PalaceStatus::Provisional,
            tags: Vec::new(),
        }
    }

    /// Befördert diese Baseline zu `Established`.
    ///
    /// # Description
    /// Verweigert die Beförderung ohne einen dokumentierten Review — dieselbe
    /// Auflage wie jede Promotion in `harw-knowledge` (§2.5 dort).
    ///
    /// # Errors
    /// [`BaselineError::PromotionNotReviewed`], sofern `reviewed` nicht
    /// `true` ist.
    pub fn promote_to_established(&mut self, reviewed: bool) -> Result<(), BaselineError> {
        if !reviewed {
            return Err(BaselineError::PromotionNotReviewed {
                from: format!("baseline/{}:provisional", self.id),
                to: format!("baseline/{}:established", self.id),
            });
        }
        self.status = PalaceStatus::Established;
        Ok(())
    }
}

/// Fehler dieses Moduls: eine Baseline-Promotion ohne Review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaselineError {
    /// Eine Beförderung wurde ohne gesetztes `reviewed` versucht.
    PromotionNotReviewed {
        /// Label des Ausgangszustands (`baseline/<id>:provisional`).
        from: String,
        /// Label des angestrebten Zielzustands (`baseline/<id>:established`).
        to: String,
    },
}

impl fmt::Display for BaselineError {
    #[allow(unused_variables)]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PromotionNotReviewed { from, to } => write!(
                f,
                "baseline promotion from {from} to {to} refused: not reviewed"
            ),
        }
    }
}

impl std::error::Error for BaselineError {}

/// Leitet aus dem Lebenszyklus-Status einer Baseline ab, welche Befund-Art
/// eine Regel gegen sie auslösen darf.
///
/// # Description
/// Siehe Moduldoku für die vollständige Begründung. Total: jeder
/// [`PalaceStatus`]-Wert hat eine definierte Antwort.
///
/// # Arguments
/// - `status` (`PalaceStatus`): der Lebenszyklus-Status einer
///   [`Baseline`].
///
/// # Returns
/// - `Some(FindingKind::RuleTriggered)` für `Established`.
/// - `Some(FindingKind::Anomaly)` für `Provisional`.
/// - `None` für `Superseded` — eine zurückgezogene Baseline lässt keine der
///   beiden Befund-Arten mehr zu; eine konsumierende Regel überspringt sie.
///
/// # Errors
/// Keine.
///
/// # Examples
/// ```rust
/// use harw_dod_rules::baseline::{finding_kind_for_status, PalaceStatus};
///
/// assert!(finding_kind_for_status(PalaceStatus::Superseded).is_none());
/// ```
#[must_use]
pub fn finding_kind_for_status(status: PalaceStatus) -> Option<FindingKind> {
    match status {
        PalaceStatus::Established => Some(FindingKind::RuleTriggered),
        PalaceStatus::Provisional => Some(FindingKind::Anomaly),
        PalaceStatus::Superseded => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;

    fn evidence() -> SecurityEvidence {
        SecurityEvidence::capture(vec![], vec![], Timestamp::UNIX_EPOCH)
            .expect("empty evidence always encodes")
    }

    #[test]
    fn test_established_yields_rule_triggered() {
        assert_eq!(
            finding_kind_for_status(PalaceStatus::Established),
            Some(FindingKind::RuleTriggered)
        );
    }

    #[test]
    fn test_provisional_yields_anomaly_only() {
        assert_eq!(
            finding_kind_for_status(PalaceStatus::Provisional),
            Some(FindingKind::Anomaly)
        );
    }

    #[test]
    fn test_superseded_yields_no_finding_kind() {
        assert_eq!(finding_kind_for_status(PalaceStatus::Superseded), None);
    }

    #[test]
    fn test_established_and_provisional_yield_different_kinds() {
        // Der wichtigste Regeltest dieses Knotens: die zwei aktiven Status
        // dürfen niemals dieselbe Befund-Art zulassen, sonst wäre das Feld
        // wirkungslos.
        assert_ne!(
            finding_kind_for_status(PalaceStatus::Established),
            finding_kind_for_status(PalaceStatus::Provisional)
        );
    }

    #[test]
    fn test_baseline_new_starts_provisional_and_formats_a_display_id() {
        let baseline = Baseline::new("baseline/cpu", "cpu baseline", Hardness::Observed, evidence());

        assert_eq!(baseline.status, PalaceStatus::Provisional);
        assert_eq!(baseline.id, "baseline/cpu");
    }

    #[test]
    fn test_baseline_promotion_without_review_is_rejected() {
        let mut baseline = Baseline::new("baseline/cpu", "cpu baseline", Hardness::Observed, evidence());

        let error = baseline
            .promote_to_established(false)
            .expect_err("unreviewed promotion must be refused");

        assert!(matches!(error, BaselineError::PromotionNotReviewed { .. }));
        assert_eq!(baseline.status, PalaceStatus::Provisional);
    }

    #[test]
    fn test_baseline_promotion_with_review_becomes_established() {
        let mut baseline = Baseline::new("baseline/cpu", "cpu baseline", Hardness::Observed, evidence());

        baseline
            .promote_to_established(true)
            .expect("reviewed promotion succeeds");

        assert_eq!(baseline.status, PalaceStatus::Established);
    }

    /// `harw_knowledge::artifact::ArtifactId` implementiert `Display`
    /// (`harw_knowledge::id_newtype!`), erfüllt also `Baseline::new`s
    /// Schranke ohne dass diese Datei den Typ produktiv nennen muss — genau
    /// das Zusammenspiel, das die Kante schneidet. Nur der Test greift auf
    /// die Wissensbasis zu (`[dev-dependencies]`).
    #[test]
    fn test_baseline_new_accepts_a_harw_knowledge_artifact_id() {
        let id = harw_knowledge::artifact::ArtifactId::new("baseline/from-knowledge");
        let baseline = Baseline::new(id, "from knowledge", Hardness::Observed, evidence());

        assert_eq!(baseline.id, "baseline/from-knowledge");
    }
}
