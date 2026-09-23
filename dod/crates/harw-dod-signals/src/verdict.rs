//! Der Verdict-Vertrag `harwness.security-verdict/v1`: [`SecurityVerdict`]
//! (Contract-Master, Knoten AW6-02).
//!
//! # Verantwortungsbereich
//! Ein Sicherheitsagent (Triage-Rolle, `family/security`, AW6-01) prüft einen
//! Befund und gibt sein Urteil als dieses JSON zurück. Dieses Modul definiert
//! den Vertrag — Feldbedeutung, Bindung, Versionierung — und die Funktionen,
//! die ein Empfänger benutzt, um ihn **nachzuprüfen**, statt dem Absender zu
//! glauben. Es committet nichts und autorisiert nichts (siehe unten).
//!
//! # Warum ein Verdikt an einen Digest gebunden ist
//! Ein Triage-Agent liest angreiferkontrollierte Sensorfelder (Dateinamen,
//! Prozessnamen, Log-Zeilen) — das ist die tragende Regel dieses Knotens.
//! Ein Urteil ohne Bindung an den geprüften Befund wäre wertlos: der
//! Empfänger könnte nicht prüfen, ob ein `Verdict{classification: Malicious}`
//! tatsächlich zu dem Beleg gehört, der gerade vorliegt, oder ob es sich um
//! ein altes, ein fremdes oder ein vom Angreifer selbst formuliertes Urteil
//! handelt, das nur zufällig durchgereicht wurde. [`SecurityVerdict`] trägt
//! deshalb `bound_evidence`: den [`harw_types::ContentDigest`] des Belegs, über
//! den geurteilt wurde — exakt derselbe Gedanke wie
//! `harw_dod_warden_proto::AuthorizationProof::bound_action`, das den
//! Inhaltsdigest der autorisierten Aktion trägt, statt nur ihre Art zu nennen.
//!
//! ## Woher der Vergleichswert kommt — die K43-Falle
//! [`SecurityVerdict::binds`] vergleicht `bound_evidence` **nicht** mit einem
//! Digest, der ebenfalls aus dem Verdikt selbst stammt (das wäre tautologisch:
//! ein manipuliertes Verdikt bestünde die eigene Prüfung immer). Der
//! Vergleichswert muss der Empfänger **selbst** mitbringen — aus dem Befund,
//! den er unabhängig hält: `crate::evidence::SecurityEvidence::digest` (AW0-07)
//! oder, sobald ein Plan-Knoten daraus eine Akte gemacht hat,
//! `EvidenceRef.digest` (`harw-plan::types`, AW4-04). Diese Crate kennt
//! `EvidenceRef` nicht (kein Abhängigkeitspfad zu `harw-plan` — Kreis
//! vermieden) und verlangt den Digest deshalb als Parameter, nicht als Typ.
//!
//! **Befund zu `EvidenceRef.digest`:** das Feld ist `Option<ContentDigest>`
//! (Rückwärtskompatibilität mit bereits geschriebenen Akten, siehe dortige
//! Doku). `TrustClass::Evidence` kann folglich nur vergeben werden, wenn
//! dieses `Option` tatsächlich `Some` ist — bei `None` bleibt nur
//! `TrustClass::Data` übrig, und zwar nicht wegen dieses Vertrags, sondern weil
//! der Aufrufer schlicht keinen Vergleichswert hat, gegen den
//! [`SecurityVerdict::binds`] prüfen könnte. Dieses Modul kann diese Lücke
//! nicht schließen — es kann nur verweigern, ein Urteil ungeprüft
//! durchzulassen, wenn kein Digest vorliegt.
//!
//! # Warum ein Verdikt nichts auslöst
//! [`SecurityVerdict`] ist eine Aussage, keine Anweisung. Diese Crate kennt
//! `harw_dod_warden_proto::WardenAction` nicht — es gibt **keinen
//! Abhängigkeitspfad** von `harw-dod-signals` zu `harw-dod-warden-proto` (siehe
//! `Cargo.toml`: nur `harw-types`, `harw-dod-cap`, `harw-observe`,
//! `harw-macros`, `serde`, `jiff`, `serde_json`). Der Typ ist damit im
//! Compiler-Sinn nicht *nennbar* an dieser Stelle — es kann hier keine
//! Funktion `SecurityVerdict -> WardenAction` geschrieben werden, gleich wie
//! sehr man es wollte. [`SuggestedResponse`] trägt deshalb bewusst nur eine
//! grobe Kategorie (`Monitor`/`Escalate`/`Contain`) ohne Ziel (keine
//! `CgroupId`, kein Prozess) — sie ist ein Vorschlag, den ausschließlich
//! `harw_dod_escalate::authorize` (AW5-03, dort `pub(crate)`) neu bewertet und
//! in eine tatsächliche `Action<Proposed>` übersetzt, mit eigener Prüfung der
//! Eskalationsstufe. Ein Test unten belegt strukturell, dass dieses Modul
//! keinen solchen Übersetzungspfad anbietet.
//!
//! # Versionierung — wie eine unbekannte Fassung erkannt wird
//! Zweifache Absicherung, keine geraten:
//! 1. **Wire-Ebene:** [`SecurityVerdict`] trägt selbst ein `contract`-Feld.
//!    [`parse_verdict`] prüft es explizit gegen [`SecurityVerdict::CONTRACT_ID`]
//!    und lehnt jede Abweichung mit
//!    [`crate::error::SignalsError::VerdictUnknownContractVersion`] ab — auch
//!    wenn ein künftiges `v2` zufällig dieselben Feldnamen mit anderer
//!    Bedeutung verwendet. `#[serde(deny_unknown_fields)]` fängt zusätzlich
//!    jedes `v2`, das ein neues Pflichtfeld einführt, bereits beim
//!    Deserialisieren ab.
//! 2. **IR-Ebene:** der Aufrufer (`harw-core-bridge`) löst den
//!    `return_pipeline().contract()`-Label des Kindes auf; ein Label, das
//!    dieses Werkzeug nicht kennt, fällt dort auf Freitext zurück, statt
//!    geraten gegen dieses Schema geprüft zu werden.
//!
//! # Inhaltsfreie Fehlermeldungen
//! Ein abgelehntes Verdikt sagt **dass**, nicht **was**: keine der
//! `Verdict*`-Fehlervarianten interpoliert den Rohtext, das Feld oder den
//! angreiferkontrollierten Wert, der die Ablehnung auslöste, in ihre
//! `Display`-Ausgabe (siehe `error.rs`). Der Grund ist derselbe wie bei jedem
//! Sensorfeld: was geloggt wird, verlässt den Host.
//!
//! # Nebenläufigkeit
//! Reine, unveränderliche Datentypen; alle Funktionen sind zustandslos und
//! aus jedem Thread aufrufbar. Keine Systemuhr wird gelesen — `issued_at`
//! kommt immer vom Aufrufer.

use jiff::Timestamp;

use crate::error::SignalsError;
use crate::evidence::Severity;
use harw_types::{ContentDigest, FindingId};

/// Wie ein Sicherheitsagent einen Befund einstuft.
///
/// # Description
/// Die eigentliche Aussage des Urteils: ob der geprüfte Beleg auf einen
/// echten Sicherheitsvorfall hindeutet. Unabhängig von [`Severity`] (dem
/// *Gewicht*, das schon [`crate::evidence::SecurityEvidence`] kennt) — ein
/// `Suspicious`-Urteil kann trotzdem `Severity::Critical` tragen, wenn der
/// Verdacht schwer wiegt, aber (noch) nicht bestätigt ist.
///
/// # Errors
/// Keine eigenen Fehler.
///
/// # Examples
/// ```rust
/// use harw_dod_signals::VerdictClassification;
///
/// assert_ne!(VerdictClassification::Benign, VerdictClassification::Confirmed);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerdictClassification {
    /// Der Beleg erklärt sich harmlos; kein Sicherheitsvorfall.
    Benign,
    /// Der Beleg ist auffällig, aber nicht eindeutig bösartig.
    Suspicious,
    /// Der Beleg bestätigt einen Sicherheitsvorfall.
    Confirmed,
}

/// Eine grobe, nicht ausführbare Kategorie dessen, was als Nächstes
/// geschehen könnte.
///
/// # Description
/// **Kein Auftrag.** Trägt bewusst kein Ziel (keine `CgroupId`, kein
/// Prozess, keine konkrete Warden-Aktion) — nur eine Kategorie, die
/// `harw_dod_escalate::authorize` (AW5-03) gegen die tatsächliche
/// Eskalationsstufe und den tatsächlichen Befund neu bewertet. Siehe
/// Moduldoku, Abschnitt „Warum ein Verdikt nichts auslöst".
///
/// # Errors
/// Keine eigenen Fehler.
///
/// # Examples
/// ```rust
/// use harw_dod_signals::SuggestedResponse;
///
/// let suggestion = SuggestedResponse::Escalate;
/// assert_eq!(suggestion, SuggestedResponse::Escalate);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SuggestedResponse {
    /// Weiter beobachten, keine Eskalation vorschlagen.
    Monitor,
    /// Eine Eskalation vorschlagen — die Leiter entscheidet, ob und wie.
    Escalate,
    /// Eindämmung vorschlagen (z. B. Freeze) — die Leiter entscheidet, ob
    /// und wie.
    Contain,
}

/// Das Urteil eines Sicherheitsagenten über einen geprüften Befund.
///
/// # Description
/// Siehe Moduldoku für die Begründung jedes Feldes, die Bindungsregel und die
/// Versionierung. Alle Felder sind privat; Lesezugriff geht ausschließlich
/// über die gleichnamigen Methoden — derselbe Aufbau wie
/// `harw_dod_warden_proto::AuthorizationProof`, aus demselben Grund: der
/// Empfänger soll ein Verdikt nur über geprüfte Zugriffe lesen, nie ein Feld
/// nachträglich unbemerkt verändern können.
///
/// # Wire-Format
/// `deny_unknown_fields` (K19) — dieser Typ nimmt Daten entgegen, die von
/// einem Kind-Agenten kommen.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityVerdict {
    /// Selbstbeschriebene Vertragsfassung; muss
    /// [`SecurityVerdict::CONTRACT_ID`] entsprechen (siehe Moduldoku,
    /// Abschnitt „Versionierung").
    contract: String,
    /// Welcher Befund geprüft wurde.
    finding: FindingId,
    /// Der Inhaltsdigest des geprüften Belegs (siehe Moduldoku, Abschnitt
    /// „Warum ein Verdikt an einen Digest gebunden ist").
    bound_evidence: ContentDigest,
    /// Die Einstufung des Befunds.
    classification: VerdictClassification,
    /// Wie schwer die Einstufung wiegt (wiederverwendet aus
    /// [`crate::evidence::Severity`] statt einer zweiten Gewichtsskala).
    severity: Severity,
    /// Kurze Begründung für einen menschlichen oder nachgelagerten Leser.
    /// Freitext — bewusst kein strukturiertes Beweisfeld: der Beleg selbst
    /// bleibt beim Empfänger, dieser Text ersetzt ihn nicht.
    rationale: String,
    /// Eine grobe, nicht ausführbare Kategorie für das weitere Vorgehen
    /// (siehe [`SuggestedResponse`]). `None`, wenn der Agent keinen
    /// Vorschlag macht.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    suggested_response: Option<SuggestedResponse>,
    /// Rollenname oder ID des urteilenden Agenten.
    issued_by: String,
    /// Wann das Urteil ausgestellt wurde — vom Aufrufer geliefert, nie über
    /// eine Systemuhr gelesen (siehe Moduldoku, Abschnitt „Nebenläufigkeit").
    issued_at: Timestamp,
}

impl SecurityVerdict {
    /// Stabiles Label dieses Vertrags (`docs/aw-plan.md`, Knoten AW6-02;
    /// `harw-registry-defaults/agents/families/security/security.toml`,
    /// `[defaults] return_contract`). Wortwörtlich übernommen — eine
    /// Abweichung im Namen würde die dortige Vorgabe wirkungslos machen,
    /// ohne dass etwas fehlschlägt.
    pub const CONTRACT_ID: &'static str = "harwness.security-verdict/v1";

    /// Baut ein Urteil und setzt `contract` immer auf [`Self::CONTRACT_ID`].
    ///
    /// # Description
    /// Der einzige öffentliche Konstruktionsweg. `contract` ist kein
    /// Parameter — ein Aufrufer kann kein Urteil mit einer falschen
    /// Vertragsfassung selbst zusammenbauen; nur eine Deserialisierung von
    /// bereits vorliegenden (potenziell falschen) Wire-Bytes kann das, und
    /// genau die prüft [`parse_verdict`] explizit.
    ///
    /// # Arguments
    /// - `finding` (`FindingId`): der geprüfte Befund.
    /// - `bound_evidence` (`ContentDigest`): der Digest des geprüften Belegs.
    /// - `classification` ([`VerdictClassification`]): die Einstufung.
    /// - `severity` ([`Severity`]): wie schwer die Einstufung wiegt.
    /// - `rationale` (`String`): kurze Begründung.
    /// - `suggested_response` (`Option<SuggestedResponse>`): optionaler,
    ///   nicht ausführbarer Vorschlag.
    /// - `issued_by` (`String`): Rollenname oder ID des urteilenden Agenten.
    /// - `issued_at` (`jiff::Timestamp`): vom Aufrufer geliefert.
    ///
    /// # Returns
    /// Das zusammengesetzte Urteil.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_signals::{SecurityVerdict, Severity, VerdictClassification};
    /// use harw_types::{ContentDigest, FindingId};
    ///
    /// let verdict = SecurityVerdict::new(
    ///     FindingId::try_from_str("finding-1").unwrap(),
    ///     ContentDigest::of(b"evidence-bytes"),
    ///     VerdictClassification::Suspicious,
    ///     Severity::Medium,
    ///     "ungewöhnliche Prozesskette".to_owned(),
    ///     None,
    ///     "security-triage-1".to_owned(),
    ///     jiff::Timestamp::UNIX_EPOCH,
    /// );
    /// assert_eq!(verdict.classification(), VerdictClassification::Suspicious);
    /// ```
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        finding: FindingId,
        bound_evidence: ContentDigest,
        classification: VerdictClassification,
        severity: Severity,
        rationale: String,
        suggested_response: Option<SuggestedResponse>,
        issued_by: String,
        issued_at: Timestamp,
    ) -> Self {
        Self {
            contract: Self::CONTRACT_ID.to_owned(),
            finding,
            bound_evidence,
            classification,
            severity,
            rationale,
            suggested_response,
            issued_by,
            issued_at,
        }
    }

    /// Die selbstbeschriebene Vertragsfassung.
    #[must_use]
    pub fn contract(&self) -> &str {
        &self.contract
    }

    /// Der geprüfte Befund.
    #[must_use]
    pub fn finding(&self) -> &FindingId {
        &self.finding
    }

    /// Der Inhaltsdigest des geprüften Belegs.
    #[must_use]
    pub fn bound_evidence(&self) -> ContentDigest {
        self.bound_evidence
    }

    /// Die Einstufung des Befunds.
    #[must_use]
    pub fn classification(&self) -> VerdictClassification {
        self.classification
    }

    /// Wie schwer die Einstufung wiegt.
    #[must_use]
    pub fn severity(&self) -> Severity {
        self.severity
    }

    /// Die Begründung des Agenten.
    #[must_use]
    pub fn rationale(&self) -> &str {
        &self.rationale
    }

    /// Der optionale, nicht ausführbare Vorschlag.
    #[must_use]
    pub fn suggested_response(&self) -> Option<SuggestedResponse> {
        self.suggested_response
    }

    /// Rollenname oder ID des urteilenden Agenten.
    #[must_use]
    pub fn issued_by(&self) -> &str {
        &self.issued_by
    }

    /// Wann das Urteil ausgestellt wurde.
    #[must_use]
    pub fn issued_at(&self) -> Timestamp {
        self.issued_at
    }

    /// Prüft, ob dieses Urteil an genau `finding` und `evidence_digest`
    /// gebunden ist.
    ///
    /// # Description
    /// Die zentrale Nachprüfung dieses Vertrags (siehe Moduldoku, Abschnitt
    /// „Woher der Vergleichswert kommt"). **`evidence_digest` muss vom
    /// Empfänger selbst stammen** — aus dessen eigenem
    /// `crate::evidence::SecurityEvidence::digest` oder aus einem
    /// `EvidenceRef.digest`, das er unabhängig vom vorliegenden Verdikt hält.
    /// Ein Aufrufer, der stattdessen `verdict.bound_evidence()` an sich
    /// selbst zurückgibt, hat nichts geprüft: die Funktion kann eine solche
    /// Verwechslung nicht erkennen, weil sie strukturell nicht von einer
    /// echten Prüfung zu unterscheiden ist — die Verantwortung liegt beim
    /// Aufrufer, einen unabhängig gehaltenen Wert zu übergeben.
    ///
    /// # Arguments
    /// - `finding` (`&FindingId`): der Befund, gegen den geprüft wird —
    ///   typischerweise der Befund, zu dem der Empfänger gerade ein Urteil
    ///   erwartet.
    /// - `evidence_digest` (`ContentDigest`): der Digest, den der Empfänger
    ///   selbst für diesen Befund hält.
    ///
    /// # Returns
    /// `Ok(())`, wenn Befund und Belegdigest zum Urteil passen.
    ///
    /// # Errors
    /// - [`SignalsError::VerdictBindingMismatch`]: `finding` oder
    ///   `evidence_digest` stimmen nicht mit diesem Urteil überein.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_signals::{SecurityVerdict, Severity, VerdictClassification};
    /// use harw_types::{ContentDigest, FindingId};
    ///
    /// let finding = FindingId::try_from_str("finding-1").unwrap();
    /// let digest = ContentDigest::of(b"evidence-bytes");
    /// let verdict = SecurityVerdict::new(
    ///     finding.clone(),
    ///     digest,
    ///     VerdictClassification::Benign,
    ///     Severity::Info,
    ///     "keine Auffälligkeit".to_owned(),
    ///     None,
    ///     "security-triage-1".to_owned(),
    ///     jiff::Timestamp::UNIX_EPOCH,
    /// );
    /// assert!(verdict.binds(&finding, digest).is_ok());
    ///
    /// let other_digest = ContentDigest::of(b"different-evidence");
    /// assert!(verdict.binds(&finding, other_digest).is_err());
    /// ```
    pub fn binds(
        &self,
        finding: &FindingId,
        evidence_digest: ContentDigest,
    ) -> Result<(), SignalsError> {
        if &self.finding != finding {
            return Err(SignalsError::VerdictBindingMismatch);
        }
        if self.bound_evidence != evidence_digest {
            return Err(SignalsError::VerdictBindingMismatch);
        }
        Ok(())
    }
}

/// Entfernt eine umschließende ```` ``` ```` - oder ```` ```json ````
/// -Code-Fence, falls vorhanden — dieselbe Toleranz wie
/// `harw_research::validate::strip_code_fence`, hier eigenständig
/// nachgebildet, weil diese Crate nicht von `harw-research` abhängt.
fn strip_code_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let after_open = after_open
        .strip_prefix("json")
        .unwrap_or(after_open)
        .trim_start_matches('\n');
    let Some(body) = after_open.strip_suffix("```") else {
        return trimmed;
    };
    body.trim()
}

/// Parst ein [`SecurityVerdict`] aus rohem Kind-Text und prüft seine
/// Vertragsfassung.
///
/// # Description
/// Toleriert eine umschließende ```` ```json ```` -Fence. Prüft nach dem
/// Parsen explizit, dass [`SecurityVerdict::contract`] gleich
/// [`SecurityVerdict::CONTRACT_ID`] ist (siehe Moduldoku, Abschnitt
/// „Versionierung") — eine strukturell kompatible, aber anders versionierte
/// Nutzlast wird dadurch erkannt, nicht geraten. Führt **keine**
/// Bindungsprüfung durch — dafür [`SecurityVerdict::binds`] verwenden.
///
/// # Arguments
/// - `raw` (`&str`): der rohe Text des Kind-Agenten.
///
/// # Returns
/// Das geparste, versionsgeprüfte [`SecurityVerdict`].
///
/// # Errors
/// - [`SignalsError::VerdictMalformed`]: der (fence-bereinigte) Text ist kein
///   gültiges `SecurityVerdict`-JSON.
/// - [`SignalsError::VerdictUnknownContractVersion`]: das JSON ist gültig,
///   aber sein `contract`-Feld ist nicht [`SecurityVerdict::CONTRACT_ID`].
///
/// # Examples
/// ```rust
/// use harw_dod_signals::parse_verdict;
///
/// let _err = parse_verdict("not json").unwrap_err();
/// ```
pub fn parse_verdict(raw: &str) -> Result<SecurityVerdict, SignalsError> {
    let cleaned = strip_code_fence(raw);
    let verdict: SecurityVerdict =
        serde_json::from_str(cleaned).map_err(|_| SignalsError::VerdictMalformed)?;
    if verdict.contract != SecurityVerdict::CONTRACT_ID {
        return Err(SignalsError::VerdictUnknownContractVersion);
    }
    Ok(verdict)
}

/// Validiert ein bereits geparstes [`SecurityVerdict`] gegen die
/// Pflichtfeldregeln dieses Vertrags.
///
/// # Description
/// Rein strukturell: leere Pflichtfelder sind kein gültiges Urteil, gleich
/// wie plausibel `classification`/`severity` sonst wirken.
///
/// # Errors
/// - [`SignalsError::VerdictEmptyField`]: `rationale` oder `issued_by` ist
///   leer oder nur Whitespace.
pub fn validate_verdict(verdict: &SecurityVerdict) -> Result<(), SignalsError> {
    if verdict.rationale.trim().is_empty() {
        return Err(SignalsError::VerdictEmptyField);
    }
    if verdict.issued_by.trim().is_empty() {
        return Err(SignalsError::VerdictEmptyField);
    }
    Ok(())
}

/// Kombiniert [`parse_verdict`] und [`validate_verdict`] in einem Schritt.
///
/// # Errors
/// Siehe [`parse_verdict`] und [`validate_verdict`].
pub fn parse_and_validate_verdict(raw: &str) -> Result<SecurityVerdict, SignalsError> {
    let verdict = parse_verdict(raw)?;
    validate_verdict(&verdict)?;
    Ok(verdict)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn finding(id: &str) -> TestResult<FindingId> {
        FindingId::try_from_str(id).map_err(ctx("non-empty id"))
    }

    fn sample(
        finding_id: FindingId,
        digest: ContentDigest,
        classification: VerdictClassification,
    ) -> SecurityVerdict {
        SecurityVerdict::new(
            finding_id,
            digest,
            classification,
            Severity::Medium,
            "eine Begründung".to_owned(),
            Some(SuggestedResponse::Escalate),
            "security-triage-1".to_owned(),
            Timestamp::UNIX_EPOCH,
        )
    }

    // -- Konstruktion setzt immer die aktuelle Vertragsfassung --------------

    #[test]
    fn test_new_always_sets_current_contract_id() -> TestResult {
        let verdict = sample(
            finding("finding-1")?,
            ContentDigest::of(b"evidence"),
            VerdictClassification::Benign,
        );
        assert_eq!(verdict.contract(), SecurityVerdict::CONTRACT_ID);
        Ok(())
    }

    // -- Rundlauf / deny_unknown_fields ---------------------------------------

    #[test]
    fn test_serde_roundtrip() -> TestResult {
        let verdict = sample(
            finding("finding-1")?,
            ContentDigest::of(b"evidence"),
            VerdictClassification::Suspicious,
        );
        let json = serde_json::to_string(&verdict).map_err(ctx("serializes"))?;
        let round_tripped: SecurityVerdict =
            serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        assert_eq!(round_tripped, verdict);
        Ok(())
    }

    #[test]
    fn test_rejects_unknown_field() -> TestResult {
        let verdict = sample(
            finding("finding-1")?,
            ContentDigest::of(b"evidence"),
            VerdictClassification::Suspicious,
        );
        let mut value = serde_json::to_value(&verdict).map_err(ctx("serializes"))?;
        value
            .as_object_mut()
            .ok_or(TestError::Missing("object"))?
            .insert("extra".to_string(), serde_json::Value::Bool(true));
        let result: Result<SecurityVerdict, _> = serde_json::from_value(value);
        assert!(result.is_err());
        Ok(())
    }

    // -- binds(): der wichtigste Test dieses Knotens --------------------------
    //
    // Der Vergleichswert kommt hier bewusst NICHT aus dem Verdikt selbst,
    // sondern aus einem unabhängig gebildeten `ContentDigest` — genau wie ein
    // Empfänger es täte, der seinen eigenen `SecurityEvidence::digest` hält.
    // Ein Verdikt, das an einen ANDEREN Befund/Beleg gebunden ist, muss über
    // diesen öffentlichen Weg erkennbar sein.

    #[test]
    fn test_binds_accepts_matching_finding_and_digest() -> TestResult {
        let f = finding("finding-1")?;
        let digest = ContentDigest::of(b"evidence-for-finding-1");
        let verdict = sample(f.clone(), digest, VerdictClassification::Confirmed);

        assert!(verdict.binds(&f, digest).is_ok());
        Ok(())
    }

    #[test]
    fn test_binds_rejects_verdict_bound_to_different_finding() -> TestResult {
        let bound_finding = finding("finding-1")?;
        let digest = ContentDigest::of(b"evidence-for-finding-1");
        let verdict = sample(bound_finding, digest, VerdictClassification::Confirmed);

        // Der Empfänger prüft für einen ANDEREN Befund, hält aber denselben
        // Belegdigest (z. B. weil zwei Befunde zufällig denselben Beleg
        // referenzieren) — die Bindung muss trotzdem scheitern, weil das
        // Verdikt nicht für diesen Befund ausgestellt wurde.
        let other_finding = finding("finding-2")?;
        let Err(err) = verdict.binds(&other_finding, digest) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, SignalsError::VerdictBindingMismatch));
        Ok(())
    }

    #[test]
    fn test_binds_rejects_verdict_bound_to_different_evidence() -> TestResult {
        let f = finding("finding-1")?;
        let bound_digest = ContentDigest::of(b"evidence-for-finding-1");
        let verdict = sample(f.clone(), bound_digest, VerdictClassification::Confirmed);

        // Der Empfänger hält für DENSELBEN Befund einen anderen Beleg-Digest
        // (der Beleg hat sich seither geändert, oder das Verdikt gehört zu
        // einem älteren Beleg desselben Befunds) — muss ebenfalls scheitern.
        let recipients_own_digest = ContentDigest::of(b"a-newer-evidence-capture");
        let Err(err) = verdict.binds(&f, recipients_own_digest) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, SignalsError::VerdictBindingMismatch));
        Ok(())
    }

    // -- Versionierung: unbekannte Fassung wird erkannt, nicht geraten --------

    #[test]
    fn test_parse_verdict_rejects_wrong_contract_id() -> TestResult {
        let raw = r#"{
            "contract": "harwness.security-verdict/v2",
            "finding": "finding-1",
            "bound_evidence": "0000000000000000000000000000000000000000000000000000000000000000",
            "classification": "benign",
            "severity": "info",
            "rationale": "x",
            "issued_by": "security-triage-1",
            "issued_at": "1970-01-01T00:00:00Z"
        }"#;
        let Err(err) = parse_verdict(raw) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, SignalsError::VerdictUnknownContractVersion));
        Ok(())
    }

    #[test]
    fn test_parse_verdict_accepts_current_contract_id() -> TestResult {
        let raw = r#"{
            "contract": "harwness.security-verdict/v1",
            "finding": "finding-1",
            "bound_evidence": "0000000000000000000000000000000000000000000000000000000000000000",
            "classification": "benign",
            "severity": "info",
            "rationale": "x",
            "issued_by": "security-triage-1",
            "issued_at": "1970-01-01T00:00:00Z"
        }"#;
        let verdict = parse_verdict(raw).map_err(ctx("parses"))?;
        assert_eq!(verdict.classification(), VerdictClassification::Benign);
        Ok(())
    }

    #[test]
    fn test_parse_verdict_tolerates_json_code_fence() -> TestResult {
        let raw = "```json\n{\"contract\":\"harwness.security-verdict/v1\",\
                   \"finding\":\"finding-1\",\"bound_evidence\":\
                   \"0000000000000000000000000000000000000000000000000000000000000000\",\
                   \"classification\":\"suspicious\",\"severity\":\"low\",\
                   \"rationale\":\"x\",\"issued_by\":\"a\",\
                   \"issued_at\":\"1970-01-01T00:00:00Z\"}\n```";
        let verdict = parse_verdict(raw).map_err(ctx("parses"))?;
        assert_eq!(verdict.classification(), VerdictClassification::Suspicious);
        Ok(())
    }

    #[test]
    fn test_parse_verdict_malformed_json_is_content_free() -> TestResult {
        let Err(err) = parse_verdict("not json at all, contains SECRET_TOKEN_123") else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, SignalsError::VerdictMalformed));
        // Die Ablehnung sagt "dass", nicht "was": der Rohtext taucht nicht
        // in der Display-Ausgabe auf.
        assert!(!err.to_string().contains("SECRET_TOKEN_123"));
        Ok(())
    }

    // -- Validierung -----------------------------------------------------------

    #[test]
    fn test_validate_verdict_rejects_empty_rationale() -> TestResult {
        let mut verdict = sample(
            finding("finding-1")?,
            ContentDigest::of(b"evidence"),
            VerdictClassification::Benign,
        );
        verdict.rationale = "   ".to_owned();
        let Err(err) = validate_verdict(&verdict) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, SignalsError::VerdictEmptyField));
        Ok(())
    }

    #[test]
    fn test_validate_verdict_rejects_empty_issued_by() -> TestResult {
        let mut verdict = sample(
            finding("finding-1")?,
            ContentDigest::of(b"evidence"),
            VerdictClassification::Benign,
        );
        verdict.issued_by = "".to_owned();
        let Err(err) = validate_verdict(&verdict) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, SignalsError::VerdictEmptyField));
        Ok(())
    }

    #[test]
    fn test_validate_verdict_accepts_well_formed_verdict() -> TestResult {
        let verdict = sample(
            finding("finding-1")?,
            ContentDigest::of(b"evidence"),
            VerdictClassification::Suspicious,
        );
        assert!(validate_verdict(&verdict).is_ok());
        Ok(())
    }

    #[test]
    fn test_parse_and_validate_end_to_end() -> TestResult {
        let raw = r#"{
            "contract": "harwness.security-verdict/v1",
            "finding": "finding-1",
            "bound_evidence": "0000000000000000000000000000000000000000000000000000000000000000",
            "classification": "confirmed",
            "severity": "critical",
            "rationale": "bestätigter Fund",
            "suggested_response": "contain",
            "issued_by": "security-triage-1",
            "issued_at": "1970-01-01T00:00:00Z"
        }"#;
        let verdict = parse_and_validate_verdict(raw).map_err(ctx("parses and validates"))?;
        assert_eq!(verdict.classification(), VerdictClassification::Confirmed);
        assert_eq!(
            verdict.suggested_response(),
            Some(SuggestedResponse::Contain)
        );
        Ok(())
    }

    // -- Kein Weg von einem Verdikt zu einer autorisierten Aktion -------------
    //
    // Diese Crate hat keinen Abhängigkeitseintrag auf
    // `harw-dod-warden-proto` (siehe Cargo.toml) — `WardenAction` ist hier
    // nicht nennbar. Der Beleg ist deshalb strukturell, nicht laufzeitseitig:
    // dieser Test dokumentiert und fixiert die einzige Vorschlagsform, die es
    // gibt ([`SuggestedResponse`]), und dass sie keine Zielangaben trägt, die
    // eine Aktion adressieren könnten (keine `CgroupId`, kein Prozess) — sie
    // taugt also nicht als Aktion, selbst wenn ein Aufrufer eine Übersetzung
    // versuchen wollte.
    #[test]
    fn test_suggested_response_carries_no_action_target() -> TestResult {
        let verdict = SecurityVerdict::new(
            finding("finding-1")?,
            ContentDigest::of(b"evidence"),
            VerdictClassification::Confirmed,
            Severity::Critical,
            "bestätigter Fund".to_owned(),
            Some(SuggestedResponse::Contain),
            "security-triage-1".to_owned(),
            Timestamp::UNIX_EPOCH,
        );
        // Die Vorschlagsform serialisiert als reiner Kategorie-String — kein
        // Objekt, das ein Ziel (cgroup/pid/host) tragen könnte, das sich zu
        // einer konkreten Aktion zusammensetzen ließe.
        let plain =
            serde_json::to_value(verdict.suggested_response()).map_err(ctx("serializes"))?;
        assert_eq!(plain, serde_json::json!("contain"));
        assert!(plain.is_string());
        Ok(())
    }
}
