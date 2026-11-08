//! Prüft das Verdict-Beispiel des gebündelten Skills `harw-dod` mit dem
//! echten Parser (PL-100 WP 1.4).
//!
//! # Zweck
//! Der Abschnitt „Verdict zurückgeben" in
//! `harw-home/assets/skills/harw-dod/instructions.md` zeigt einem
//! Triage-Kind ein vollständiges `harwness.security-verdict/v1`-Objekt. Das
//! Kind übernimmt die Form dieses Beispiels; bräche das Beispiel den
//! Vertrag, verwürfe der Rückkanal (`accept_security_verdict`) jedes so
//! geformte Verdict. Dieser Test bettet die Datei zur Bauzeit ein
//! (`include_str!`), schneidet den Abschnitt und darin den einzigen
//! `json`-Block heraus und reicht ihn an
//! [`harw_dod_signals::parse_and_validate_verdict`] — denselben Parser, den
//! der Rückkanal benutzt. Die Schlüsselprüfung ohne Parser liegt in
//! `harw-registry-defaults` (das Crate hängt nicht an `harw-dod-signals`).
//!
//! # Was geprüft wird
//! - Der Abschnitt steht genau einmal in der Datei und enthält genau einen
//!   `json`-Block.
//! - Der Block parst und validiert: Vertrags-ID, keine unbekannten Felder,
//!   64-stelliger Hex-Digest, nicht-leere `rationale` und `issued_by` — auch
//!   mit umschließendem `json`-Codezaun, wie ihn ein Modell oft liefert.
//! - Die Einstufung ist nie `confirmed`: nur ein Mensch bestätigt.
//! - `finding` hat die UUIDv4-Form der Befund-IDs, der Digest steht in
//!   Kleinbuchstaben im Beispiel und ist kein Null-Digest, und `issued_by`
//!   nennt eine der vier Triage-Rollen.
//!
//! # Nebenläufigkeit
//! Keine: reine Textverarbeitung über einer zur Bauzeit eingebetteten
//! Konstante, ohne I/O zur Laufzeit.
//!
//! # Examples
//! ```ignore
//! // Läuft als Integrationstest dieses Crates mit dem übrigen Testlauf.
//! ```

use harw_dod_signals::{SecurityVerdict, VerdictClassification, parse_and_validate_verdict};

harw_test_support::define_test_error!(pub(crate));

/// Der Text des gebündelten Skills `harw-dod`, zur Bauzeit eingebettet.
const INSTRUCTIONS: &str = include_str!("../../harw-home/assets/skills/harw-dod/instructions.md");

/// Die Überschrift des Abschnitts mit dem Verdict-Beispiel (mit den
/// umgebenden Zeilenumbrüchen, damit nur eine echte Überschrift trifft).
const SECTION_HEADING: &str = "\n## Verdict zurückgeben\n";

/// Der öffnende Codezaun des Beispiels.
const JSON_FENCE_OPEN: &str = "```json\n";

/// Der schließende Codezaun des Beispiels.
const JSON_FENCE_CLOSE: &str = "\n```";

/// Die vier Triage-Rollen, die als einzige ein Verdict abgeben
/// (`issued_by`). Als Literale, weil dieses Crate nicht an
/// `harw-registry-defaults` hängt.
const TRIAGE_ROLES: [&str; 4] = [
    "security-egress-triage",
    "security-baseline-triage",
    "security-structure-triage",
    "security-endpoint-triage",
];

/// Liefert den Abschnitt „Verdict zurückgeben" bis zur nächsten
/// `## `-Überschrift (oder bis zum Dateiende).
///
/// # Errors
/// [`TestError::Missing`], wenn die Überschrift fehlt.
fn verdict_section() -> TestResult<&'static str> {
    let (_, rest) = INSTRUCTIONS
        .split_once(SECTION_HEADING)
        .ok_or(TestError::Missing("Abschnitt „Verdict zurückgeben\""))?;
    Ok(rest
        .split_once("\n## ")
        .map_or(rest, |(section, _)| section))
}

/// Liefert den Inhalt des einzigen `json`-Codeblocks in `section`, ohne die
/// Codezäune.
///
/// # Errors
/// - [`TestError::Missing`], wenn kein vollständiger `json`-Block da ist.
/// - [`TestError::Unexpected`], wenn ein zweiter `json`-Block folgt.
fn json_example(section: &str) -> TestResult<&str> {
    let (_, after_open) = section
        .split_once(JSON_FENCE_OPEN)
        .ok_or(TestError::Missing("json-Block im Abschnitt"))?;
    let (body, after_close) = after_open
        .split_once(JSON_FENCE_CLOSE)
        .ok_or(TestError::Missing("Ende des json-Blocks"))?;
    if after_close.contains(JSON_FENCE_OPEN) {
        return Err(TestError::Unexpected(
            "der Abschnitt führt mehr als einen json-Block".to_owned(),
        ));
    }
    Ok(body)
}

/// Ob `text` die Form einer UUIDv4 in Kleinbuchstaben hat (Gruppen
/// 8-4-4-4-12, Version `4`, Variante `8`/`9`/`a`/`b`).
fn is_lowercase_uuid_v4(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    let lengths: Vec<usize> = groups.iter().map(|group| group.len()).collect();
    let lowercase_hex = |group: &&str| {
        group
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    };
    lengths == [8, 4, 4, 4, 12]
        && groups.iter().all(lowercase_hex)
        && groups.get(2).is_some_and(|group| group.starts_with('4'))
        && groups.get(3).is_some_and(|group| {
            group
                .chars()
                .next()
                .is_some_and(|c| matches!(c, '8' | '9' | 'a' | 'b'))
        })
}

/// Das Beispiel parst und validiert mit dem echten Parser, ist nie
/// `confirmed` und hält die Form der Felder ein, die der Empfänger bindet.
#[test]
fn harw_dod_verdict_example_parses_with_the_real_parser_and_is_never_confirmed() -> TestResult {
    assert_eq!(
        INSTRUCTIONS.matches(SECTION_HEADING).count(),
        1,
        "der Abschnitt steht genau einmal im Skill"
    );
    let example = json_example(verdict_section()?)?;
    let verdict = parse_and_validate_verdict(example).map_err(ctx(
        "das Beispiel ist ein gültiges harwness.security-verdict/v1",
    ))?;

    assert_eq!(verdict.contract(), SecurityVerdict::CONTRACT_ID);
    assert_ne!(
        verdict.classification(),
        VerdictClassification::Confirmed,
        "ein Modell gibt nie confirmed zurück"
    );
    assert!(
        is_lowercase_uuid_v4(verdict.finding().as_str()),
        "finding hat die UUIDv4-Form einer Befund-ID"
    );
    let digest = verdict.bound_evidence().to_string();
    assert!(
        example.contains(&digest),
        "bound_evidence steht unverändert in Kleinbuchstaben im Beispiel"
    );
    assert!(
        digest.chars().any(|c| c != '0'),
        "bound_evidence ist kein Null-Digest"
    );
    assert!(
        TRIAGE_ROLES.contains(&verdict.issued_by()),
        "issued_by nennt eine Triage-Rolle"
    );
    assert!(!verdict.rationale().trim().is_empty());
    Ok(())
}

/// Ein Modell liefert das Objekt oft in einem `json`-Codezaun; auch diese
/// Form akzeptiert der Parser und ergibt dasselbe Verdict.
#[test]
fn harw_dod_verdict_example_is_accepted_inside_a_json_fence() -> TestResult {
    let example = json_example(verdict_section()?)?;
    let bare = parse_and_validate_verdict(example).map_err(ctx("ohne Codezaun"))?;
    let fenced =
        parse_and_validate_verdict(&format!("{JSON_FENCE_OPEN}{example}{JSON_FENCE_CLOSE}"))
            .map_err(ctx("mit Codezaun"))?;
    assert_eq!(fenced.finding().as_str(), bare.finding().as_str());
    assert_eq!(fenced.bound_evidence(), bare.bound_evidence());
    assert_eq!(fenced.classification(), bare.classification());
    assert_eq!(fenced.issued_by(), bare.issued_by());
    Ok(())
}

/// Gegenprobe zur Form-Hilfe: eine Nicht-UUID und eine UUID mit falscher
/// Version werden erkannt (sonst bewiese die Prüfung oben nichts).
#[test]
fn uuid_v4_shape_check_rejects_other_shapes() {
    assert!(is_lowercase_uuid_v4("3f2b9c1e-8a4d-4f6e-9b21-7c0d5e8a1f42"));
    assert!(!is_lowercase_uuid_v4(
        "3f2b9c1e-8a4d-1f6e-9b21-7c0d5e8a1f42"
    ));
    assert!(!is_lowercase_uuid_v4(
        "3F2B9C1E-8A4D-4F6E-9B21-7C0D5E8A1F42"
    ));
    assert!(!is_lowercase_uuid_v4("finding-1"));
}
