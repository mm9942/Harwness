//! Return-Contract für read-only Kind-Agenten.
//!
//! Verantwortungsbereich: `ReturnEnvelope` trägt das Ergebnis eines
//! Kind-Agenten (Ausgang, Zusammenfassung, Artefakte, Blocker, Payload)
//! zurück an seinen Eltern-Agenten — eine reduzierte Fassung des
//! Return-Contracts aus `agent-definition-dsl.md` §13 für read-only
//! Explorer-/Researcher-Worker (kein `state_revision_after`, kein
//! `priority`/`severity`, weil diese Worker keinen Plan-Zustand mutieren).
//!
//! [`sanitize_for_parent`] ist die Eltern-Grenze: bevor eine Envelope den
//! Eltern-Agenten erreicht, werden Bezeichner, Zusammenfassung, Listen und
//! Nutzlast begrenzt und auf Steuerzeichen geprüft (Run-Isolation, PL-90
//! W03d). Verstöße werden abgelehnt, nie stillschweigend repariert; nur eine
//! zu lange Zusammenfassung wird gekürzt.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ResearchError, ResearchResult};
use crate::types::ResearchFinding;
use crate::validate::strip_code_fence;

/// Ausgang eines Kind-Agenten-Laufs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReturnOutcome {
    /// Der Auftrag wurde vollständig erfüllt.
    Success,
    /// Der Auftrag wurde teilweise erfüllt.
    Partial,
    /// Der Auftrag konnte nicht erfüllt werden.
    Failed,
}

/// Reduzierter Return-Contract für read-only Kind-Agenten
/// (`agent-definition-dsl.md` §13).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReturnEnvelope {
    /// Bezeichner des berichtenden Agenten.
    pub agent_id: String,
    /// Optionale Zuordnung zu einem Plan-Knoten.
    #[serde(default)]
    pub plan_node_id: Option<String>,
    /// Ausgang des Laufs.
    pub outcome: ReturnOutcome,
    /// Kurze menschenlesbare Zusammenfassung.
    pub summary: String,
    /// Pfade oder Bezeichner erzeugter Artefakte.
    #[serde(default)]
    pub artifacts: Vec<String>,
    /// Dinge, die den Agenten blockiert haben.
    #[serde(default)]
    pub blockers: Vec<String>,
    /// Nicht blockierende Warnungen.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Vorgeschlagene nächste Schritte für den Elternagenten.
    #[serde(default)]
    pub suggested_next_actions: Vec<String>,
    /// Strukturierte Nutzlast (z. B. ein serialisiertes [`ResearchFinding`]).
    pub payload: Value,
}

/// Parst eine [`ReturnEnvelope`] aus rohem Kind-Agenten-Text.
///
/// # Description
/// Toleriert eine umschließende ```` ```json ```` -Fence wie
/// [`crate::validate::parse_finding`].
///
/// # Errors
/// - [`ResearchError::Json`]: wenn der (fence-bereinigte) Text kein gültiges
///   `ReturnEnvelope`-JSON ist.
pub fn parse_return_envelope(raw: &str) -> ResearchResult<ReturnEnvelope> {
    let cleaned = strip_code_fence(raw);
    serde_json::from_str(cleaned).map_err(ResearchError::from)
}

/// Baut eine `Success`-Envelope, die ein [`ResearchFinding`] als Payload trägt.
///
/// # Description
/// `artifacts`/`blockers`/`warnings`/`suggested_next_actions` bleiben leer;
/// `summary` wird aus `finding.conclusion` übernommen. Die Serialisierung von
/// `finding` nach `serde_json::Value` kann für die hier verwendeten Typen
/// praktisch nicht fehlschlagen; ein Fehlerfall fällt auf `Value::Null` zurück
/// statt zu paniken.
///
/// # Arguments
/// - `agent_id` (`&str`): Bezeichner des berichtenden Kind-Agenten.
/// - `plan_node_id` (`Option<&str>`): optionale Plan-Knoten-Zuordnung.
/// - `finding` (`&ResearchFinding`): das zu verpackende Recherche-Ergebnis.
///
/// # Returns
/// Eine `ReturnEnvelope` mit `outcome = Success` und `payload` = serialisiertes
/// `finding`.
#[must_use]
pub fn envelope_with_finding(
    agent_id: &str,
    plan_node_id: Option<&str>,
    finding: &ResearchFinding,
) -> ReturnEnvelope {
    let payload = serde_json::to_value(finding).unwrap_or(Value::Null);
    ReturnEnvelope {
        agent_id: agent_id.to_owned(),
        plan_node_id: plan_node_id.map(str::to_owned),
        outcome: ReturnOutcome::Success,
        summary: finding.conclusion.to_owned(),
        artifacts: Vec::new(),
        blockers: Vec::new(),
        warnings: Vec::new(),
        suggested_next_actions: Vec::new(),
        payload,
    }
}

/// Maximale Länge der Zusammenfassung in Zeichen (inklusive des Kürzungs-
/// zeichens `…`); längere Zusammenfassungen werden gekürzt.
pub const MAX_SUMMARY_CHARS: usize = 1_024;
/// Maximale Anzahl Einträge je Liste (`artifacts`, `blockers`, `warnings`,
/// `suggested_next_actions`).
pub const MAX_LIST_ENTRIES: usize = 16;
/// Maximale Länge eines Listeneintrags bzw. eines Bezeichners in Zeichen.
pub const MAX_ENTRY_CHARS: usize = 256;
/// Maximale serialisierte Größe der `payload` in Bytes.
pub const MAX_PAYLOAD_BYTES: usize = 16 * 1024;

/// Warum eine [`ReturnEnvelope`] an der Eltern-Grenze abgelehnt wurde.
///
/// # Description
/// Die Varianten nennen nur Feldnamen und Zählwerte, **nie** Inhalt des
/// Kindes: die `Display`-Ausgabe darf deshalb ungefiltert in einen
/// Vertragsbruch-Report für das Eltern-Modell übernommen werden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeRefusal {
    /// Eine Liste hat mehr als [`MAX_LIST_ENTRIES`] Einträge.
    TooManyEntries {
        /// Name des betroffenen Feldes.
        field: &'static str,
        /// Tatsächliche Anzahl der Einträge.
        count: usize,
    },
    /// Ein Eintrag (oder Bezeichner) ist länger als [`MAX_ENTRY_CHARS`].
    EntryTooLong {
        /// Name des betroffenen Feldes.
        field: &'static str,
    },
    /// Ein Feld enthält ein unzulässiges Steuerzeichen.
    ControlChars {
        /// Name des betroffenen Feldes.
        field: &'static str,
    },
    /// Die serialisierte `payload` ist größer als [`MAX_PAYLOAD_BYTES`].
    PayloadTooLarge {
        /// Serialisierte Größe in Bytes (bei fehlgeschlagener Serialisierung
        /// ein Wert oberhalb des Limits).
        bytes: usize,
    },
    /// Ein Bezeichner enthält Zeichen außerhalb von `[A-Za-z0-9-_./:@#]`.
    UnsafeIdentifier {
        /// Name des betroffenen Feldes.
        field: &'static str,
    },
}

impl fmt::Display for EnvelopeRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyEntries { field, count } => write!(
                f,
                "field '{field}' has {count} entries, more than the limit of {MAX_LIST_ENTRIES}"
            ),
            Self::EntryTooLong { field } => write!(
                f,
                "field '{field}' has an entry longer than {MAX_ENTRY_CHARS} characters"
            ),
            Self::ControlChars { field } => {
                write!(f, "field '{field}' contains control characters")
            }
            Self::PayloadTooLarge { bytes } => write!(
                f,
                "payload of {bytes} bytes exceeds the limit of {MAX_PAYLOAD_BYTES} bytes"
            ),
            Self::UnsafeIdentifier { field } => {
                write!(
                    f,
                    "field '{field}' contains characters not allowed in identifiers"
                )
            }
        }
    }
}

impl std::error::Error for EnvelopeRefusal {}

/// Eine geprüfte [`ReturnEnvelope`] samt Hinweis, ob die Zusammenfassung
/// gekürzt wurde.
#[derive(Debug, Clone, PartialEq)]
pub struct SanitizedReturnEnvelope {
    /// Die geprüfte Envelope mit begrenzter Zusammenfassung.
    pub envelope: ReturnEnvelope,
    /// `true`, wenn `summary` auf [`MAX_SUMMARY_CHARS`] gekürzt wurde.
    pub summary_truncated: bool,
}

/// Ist `c` ein im Bezeichner erlaubtes Zeichen (`[A-Za-z0-9-_./:@#]`)?
fn is_identifier_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '@' | '#')
}

/// Prüft einen Bezeichner (`agent_id`, `plan_node_id`).
fn check_identifier(field: &'static str, value: &str) -> Result<(), EnvelopeRefusal> {
    if !value.chars().all(is_identifier_char) {
        return Err(EnvelopeRefusal::UnsafeIdentifier { field });
    }
    if value.chars().count() > MAX_ENTRY_CHARS {
        return Err(EnvelopeRefusal::EntryTooLong { field });
    }
    Ok(())
}

/// Prüft eine Stringliste: Anzahl, Länge je Eintrag, Steuerzeichen (jede Art —
/// Listeneinträge sind einzeilige Verweise, kein Fließtext).
fn check_list(field: &'static str, entries: &[String]) -> Result<(), EnvelopeRefusal> {
    if entries.len() > MAX_LIST_ENTRIES {
        return Err(EnvelopeRefusal::TooManyEntries {
            field,
            count: entries.len(),
        });
    }
    for entry in entries {
        if entry.chars().count() > MAX_ENTRY_CHARS {
            return Err(EnvelopeRefusal::EntryTooLong { field });
        }
        if entry.chars().any(char::is_control) {
            return Err(EnvelopeRefusal::ControlChars { field });
        }
    }
    Ok(())
}

/// Kürzt `summary` auf [`MAX_SUMMARY_CHARS`] Zeichen (inklusive `…`) an einer
/// Zeichengrenze und meldet, ob gekürzt wurde.
fn truncate_summary(summary: String) -> (String, bool) {
    if summary.chars().count() <= MAX_SUMMARY_CHARS {
        return (summary, false);
    }
    let keep = MAX_SUMMARY_CHARS.saturating_sub(1);
    let mut shortened: String = summary.chars().take(keep).collect();
    shortened.push('…');
    (shortened, true)
}

/// Begrenzte, inhaltssichere Kopie der Envelope eines Kindes für den Eltern-
/// Agenten.
///
/// # Description
/// Die einzige Prüfstelle zwischen Kind und Eltern-Agent für eine
/// [`ReturnEnvelope`]. Strukturverstöße werden **abgelehnt**, nie stillschweigend
/// repariert; nur eine zu lange Zusammenfassung wird gekürzt (ein Größen-,
/// kein Vertrauensproblem):
/// - `agent_id` und `plan_node_id`: nur `[A-Za-z0-9-_./:@#]`, höchstens
///   [`MAX_ENTRY_CHARS`] Zeichen.
/// - `summary`: Steuerzeichen außer `\n`/`\t` werden abgelehnt; mehr als
///   [`MAX_SUMMARY_CHARS`] Zeichen werden an einer Zeichengrenze gekürzt und
///   mit `…` markiert (`summary_truncated`).
/// - `artifacts`, `blockers`, `warnings`, `suggested_next_actions`: höchstens
///   [`MAX_LIST_ENTRIES`] Einträge von je höchstens [`MAX_ENTRY_CHARS`]
///   Zeichen, ohne jedes Steuerzeichen.
/// - `payload`: serialisiert höchstens [`MAX_PAYLOAD_BYTES`] Bytes; scheitert
///   die Serialisierung, gilt die Nutzlast als zu groß.
///
/// `outcome` bleibt unverändert.
///
/// # Arguments
/// - `raw` (`ReturnEnvelope`): die vom Kind gelieferte, bereits geparste
///   Envelope.
///
/// # Returns
/// Die geprüfte Envelope und ob die Zusammenfassung gekürzt wurde.
///
/// # Errors
/// [`EnvelopeRefusal`] für die erste verletzte Grenze; der Fehler nennt nur
/// Feldnamen und Zählwerte, keinen Inhalt des Kindes.
pub fn sanitize_for_parent(
    raw: ReturnEnvelope,
) -> Result<SanitizedReturnEnvelope, EnvelopeRefusal> {
    check_identifier("agent_id", &raw.agent_id)?;
    if let Some(node) = &raw.plan_node_id {
        check_identifier("plan_node_id", node)?;
    }
    if raw
        .summary
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(EnvelopeRefusal::ControlChars { field: "summary" });
    }
    check_list("artifacts", &raw.artifacts)?;
    check_list("blockers", &raw.blockers)?;
    check_list("warnings", &raw.warnings)?;
    check_list("suggested_next_actions", &raw.suggested_next_actions)?;
    let payload_bytes = match serde_json::to_vec(&raw.payload) {
        Ok(bytes) => bytes.len(),
        // Nicht serialisierbar: als „zu groß“ behandeln, nicht durchlassen.
        Err(_) => MAX_PAYLOAD_BYTES.saturating_add(1),
    };
    if payload_bytes > MAX_PAYLOAD_BYTES {
        return Err(EnvelopeRefusal::PayloadTooLarge {
            bytes: payload_bytes,
        });
    }

    let ReturnEnvelope {
        agent_id,
        plan_node_id,
        outcome,
        summary,
        artifacts,
        blockers,
        warnings,
        suggested_next_actions,
        payload,
    } = raw;
    let (summary, summary_truncated) = truncate_summary(summary);
    Ok(SanitizedReturnEnvelope {
        envelope: ReturnEnvelope {
            agent_id,
            plan_node_id,
            outcome,
            summary,
            artifacts,
            blockers,
            warnings,
            suggested_next_actions,
            payload,
        },
        summary_truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::types::{Confidence, QuestionId};

    fn sample_finding() -> TestResult<ResearchFinding> {
        Ok(ResearchFinding {
            question_id: QuestionId::new("q-1"),
            likelihood: None,
            confidence_rationale: String::new(),
            hypotheses: vec![],
            key_assumptions: vec![],
            indicators: vec![],
            dissent: vec![],
            conclusion: "jiff 0.2.32 is current".to_owned(),
            evidence: vec![],
            verified_versions: vec![],
            constraints: vec![],
            compatibility_notes: vec![],
            unresolved_questions: vec![],
            confidence: Confidence::Low,
            produced_by: "explorer-1".to_owned(),
            produced_at: "2026-08-27T00:00:00Z"
                .parse()
                .map_err(ctx("timestamp fixture literal must parse"))?,
        })
    }

    #[test]
    fn test_return_outcome_serializes_snake_case() -> TestResult {
        let json = serde_json::to_string(&ReturnOutcome::Success)
            .map_err(ctx("ReturnOutcome must serialize"))?;
        assert_eq!(json, "\"success\"");
        Ok(())
    }

    #[test]
    fn test_envelope_with_finding_sets_payload_and_summary() -> TestResult {
        let finding = sample_finding()?;
        let env = envelope_with_finding("explorer-1", Some("node-42"), &finding);

        assert_eq!(env.agent_id, "explorer-1");
        assert_eq!(env.plan_node_id.as_deref(), Some("node-42"));
        assert_eq!(env.outcome, ReturnOutcome::Success);
        assert_eq!(env.summary, "jiff 0.2.32 is current");
        assert!(env.artifacts.is_empty());
        assert!(env.blockers.is_empty());

        let expected_payload =
            serde_json::to_value(&finding).map_err(ctx("finding must serialize to value"))?;
        assert_eq!(env.payload, expected_payload);
        Ok(())
    }

    #[test]
    fn test_envelope_with_finding_plan_node_id_none() -> TestResult {
        let finding = sample_finding()?;
        let env = envelope_with_finding("explorer-1", None, &finding);
        assert_eq!(env.plan_node_id, None);
        Ok(())
    }

    #[test]
    fn test_parse_return_envelope_strips_json_fence() -> TestResult {
        let raw = "```json\n{\"agent_id\":\"a1\",\"outcome\":\"partial\",\
                   \"summary\":\"partly done\",\"payload\":{}}\n```";
        let env = parse_return_envelope(raw).map_err(ctx("well-formed fenced JSON must parse"))?;
        assert_eq!(env.agent_id, "a1");
        assert_eq!(env.outcome, ReturnOutcome::Partial);
        assert!(env.artifacts.is_empty());
        Ok(())
    }

    #[test]
    fn test_parse_return_envelope_invalid_json_returns_json_error() -> TestResult {
        let result = parse_return_envelope("nope");
        let Err(err) = result else {
            return Err(TestError::Unexpected(
                "expected Err for malformed JSON".to_owned(),
            ));
        };
        assert!(matches!(err, ResearchError::Json(_)));
        Ok(())
    }

    #[test]
    fn test_return_envelope_roundtrip() -> TestResult {
        let finding = sample_finding()?;
        let env = envelope_with_finding("explorer-1", Some("node-1"), &finding);
        let json = serde_json::to_string(&env).map_err(ctx("envelope must serialize"))?;
        let back: ReturnEnvelope =
            serde_json::from_str(&json).map_err(ctx("envelope must deserialize"))?;
        assert_eq!(env, back);
        Ok(())
    }

    /// Eine gültige, kleine Envelope als Ausgangspunkt für die
    /// Sanitizer-Tests.
    fn plain_envelope() -> ReturnEnvelope {
        ReturnEnvelope {
            agent_id: "explorer-1".to_owned(),
            plan_node_id: Some("node-42".to_owned()),
            outcome: ReturnOutcome::Partial,
            summary: "fertig".to_owned(),
            artifacts: vec!["out/report.md".to_owned()],
            blockers: Vec::new(),
            warnings: Vec::new(),
            suggested_next_actions: Vec::new(),
            payload: serde_json::json!({ "k": 1 }),
        }
    }

    #[test]
    fn sanitize_passes_a_plain_envelope_unchanged() -> TestResult {
        let raw = plain_envelope();
        let sanitized =
            sanitize_for_parent(raw.clone()).map_err(ctx("a plain envelope must sanitize"))?;
        assert!(!sanitized.summary_truncated);
        assert_eq!(sanitized.envelope, raw);
        assert_eq!(sanitized.envelope.outcome, ReturnOutcome::Partial);
        Ok(())
    }

    #[test]
    fn sanitize_truncates_summary_on_char_boundary() -> TestResult {
        // Zwei-, drei- und vierstellige UTF-8-Zeichen im Wechsel: ein
        // byte-basierter Schnitt würde mitten in ein Zeichen fallen.
        let pattern = ['ä', '€', '𝄞'];
        let original: String = pattern
            .iter()
            .cycle()
            .take(MAX_SUMMARY_CHARS + 10)
            .collect();
        let mut raw = plain_envelope();
        raw.summary = original.clone();

        let sanitized = sanitize_for_parent(raw).map_err(ctx("a long summary must sanitize"))?;
        assert!(sanitized.summary_truncated);
        let summary = &sanitized.envelope.summary;
        assert_eq!(summary.chars().count(), MAX_SUMMARY_CHARS);
        assert!(summary.ends_with('…'));
        let kept: String = original.chars().take(MAX_SUMMARY_CHARS - 1).collect();
        assert_eq!(summary.strip_suffix('…'), Some(kept.as_str()));
        // Die Ergebnis-Envelope bleibt serialisierbar (gültiges UTF-8).
        serde_json::to_string(&sanitized.envelope).map_err(ctx("sanitized envelope serializes"))?;

        // Genau am Limit: unverändert; ein Zeichen darüber: gekürzt.
        let mut exact = plain_envelope();
        exact.summary = original.chars().take(MAX_SUMMARY_CHARS).collect();
        let exact_out =
            sanitize_for_parent(exact.clone()).map_err(ctx("exact-size summary sanitizes"))?;
        assert!(!exact_out.summary_truncated);
        assert_eq!(exact_out.envelope.summary, exact.summary);

        let mut over = plain_envelope();
        over.summary = original.chars().take(MAX_SUMMARY_CHARS + 1).collect();
        let over_out = sanitize_for_parent(over).map_err(ctx("one-over summary sanitizes"))?;
        assert!(over_out.summary_truncated);
        assert_eq!(over_out.envelope.summary.chars().count(), MAX_SUMMARY_CHARS);
        Ok(())
    }

    #[test]
    fn sanitize_refuses_control_chars_and_oversized_payload() -> TestResult {
        // Zeilenumbruch und Tab sind in der Zusammenfassung erlaubt …
        let mut raw = plain_envelope();
        raw.summary = "zeile eins\n\tzeile zwei".to_owned();
        sanitize_for_parent(raw).map_err(ctx("newline and tab are allowed in the summary"))?;

        // … alles andere nicht.
        for bad in ["esc\u{1b}[31m", "nul\0", "cr\r", "del\u{7f}", "c1\u{85}"] {
            let mut raw = plain_envelope();
            raw.summary = bad.to_owned();
            assert_eq!(
                sanitize_for_parent(raw),
                Err(EnvelopeRefusal::ControlChars { field: "summary" }),
                "summary {bad:?} must be refused"
            );
        }

        // Nutzlast genau am Limit ist erlaubt, ein Byte darüber nicht. Die
        // serialisierte Form eines Strings sind Länge + 2 Anführungszeichen.
        let mut at_limit = plain_envelope();
        at_limit.payload = Value::String("a".repeat(MAX_PAYLOAD_BYTES - 2));
        sanitize_for_parent(at_limit).map_err(ctx("a payload at the limit is allowed"))?;

        let mut over = plain_envelope();
        over.payload = Value::String("a".repeat(MAX_PAYLOAD_BYTES - 1));
        assert_eq!(
            sanitize_for_parent(over),
            Err(EnvelopeRefusal::PayloadTooLarge {
                bytes: MAX_PAYLOAD_BYTES + 1
            })
        );
        Ok(())
    }

    #[test]
    fn sanitize_refusal_display_is_content_free() -> TestResult {
        let secret = "GEHEIMER-KIND-INHALT";
        let mut raw = plain_envelope();
        raw.payload = Value::String(format!("{secret}{}", "a".repeat(MAX_PAYLOAD_BYTES)));
        let refusal = sanitize_for_parent(raw)
            .err()
            .ok_or(TestError::Missing("an oversized payload must be refused"))?;
        let text = refusal.to_string();
        assert!(!text.contains(secret), "refusal leaked content: {text}");
        assert!(text.contains("payload"), "refusal names the field: {text}");
        Ok(())
    }

    /// Die vier gedeckelten Listenfelder.
    const LIST_FIELDS: [&str; 4] = [
        "artifacts",
        "blockers",
        "warnings",
        "suggested_next_actions",
    ];

    /// Eine gültige Envelope, bei der genau das Listenfeld `field` die
    /// `entries` trägt.
    fn envelope_with_list(field: &str, entries: Vec<String>) -> ReturnEnvelope {
        let mut envelope = plain_envelope();
        match field {
            "artifacts" => envelope.artifacts = entries,
            "blockers" => envelope.blockers = entries,
            "warnings" => envelope.warnings = entries,
            _ => envelope.suggested_next_actions = entries,
        }
        envelope
    }

    #[test]
    fn sanitize_enforces_list_caps() -> TestResult {
        let entries =
            |n: usize| -> Vec<String> { (0..n).map(|i| format!("eintrag-{i}")).collect() };

        for field in LIST_FIELDS {
            // Genau am Limit: erlaubt.
            let at_cap = envelope_with_list(field, entries(MAX_LIST_ENTRIES));
            sanitize_for_parent(at_cap).map_err(ctx("a list at the cap is allowed"))?;

            // Ein Eintrag zu viel.
            let too_many = envelope_with_list(field, entries(MAX_LIST_ENTRIES + 1));
            assert_eq!(
                sanitize_for_parent(too_many),
                Err(EnvelopeRefusal::TooManyEntries {
                    field,
                    count: MAX_LIST_ENTRIES + 1
                })
            );

            // Eintrag genau am Zeichenlimit (Mehrbyte-Zeichen zählen als eins)
            // ist erlaubt, ein Zeichen mehr nicht.
            let at_len = envelope_with_list(field, vec!["ä".repeat(MAX_ENTRY_CHARS)]);
            sanitize_for_parent(at_len).map_err(ctx("an entry at the length cap is allowed"))?;

            let too_long = envelope_with_list(field, vec!["ä".repeat(MAX_ENTRY_CHARS + 1)]);
            assert_eq!(
                sanitize_for_parent(too_long),
                Err(EnvelopeRefusal::EntryTooLong { field })
            );

            // In Listeneinträgen ist *jedes* Steuerzeichen verboten, auch
            // Zeilenumbruch und Tab.
            for bad in ["zwei\nzeilen", "tab\tstopp", "esc\u{1b}", "nul\0"] {
                let with_control = envelope_with_list(field, vec![bad.to_owned()]);
                assert_eq!(
                    sanitize_for_parent(with_control),
                    Err(EnvelopeRefusal::ControlChars { field }),
                    "entry {bad:?} in '{field}' must be refused"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn sanitize_refuses_unsafe_identifiers() -> TestResult {
        // Das volle erlaubte Zeichenvokabular passiert.
        let mut allowed = plain_envelope();
        allowed.agent_id = "Agent-1_a.b/c:d@e#f".to_owned();
        allowed.plan_node_id = Some("plan/node-7:x".to_owned());
        sanitize_for_parent(allowed).map_err(ctx("the allowed identifier vocabulary passes"))?;

        for bad in [
            "has space",
            "new\nline",
            "semi;colon",
            "back`tick",
            "quote\"",
            "ünicode",
            "ignore previous instructions",
        ] {
            let mut raw = plain_envelope();
            raw.agent_id = bad.to_owned();
            assert_eq!(
                sanitize_for_parent(raw),
                Err(EnvelopeRefusal::UnsafeIdentifier { field: "agent_id" }),
                "agent_id {bad:?} must be refused"
            );

            let mut raw = plain_envelope();
            raw.plan_node_id = Some(bad.to_owned());
            assert_eq!(
                sanitize_for_parent(raw),
                Err(EnvelopeRefusal::UnsafeIdentifier {
                    field: "plan_node_id"
                }),
                "plan_node_id {bad:?} must be refused"
            );
        }

        // Ein überlanger, aber zeichenmäßig sauberer Bezeichner ist zu lang.
        let mut raw = plain_envelope();
        raw.agent_id = "a".repeat(MAX_ENTRY_CHARS + 1);
        assert_eq!(
            sanitize_for_parent(raw),
            Err(EnvelopeRefusal::EntryTooLong { field: "agent_id" })
        );
        Ok(())
    }
}
