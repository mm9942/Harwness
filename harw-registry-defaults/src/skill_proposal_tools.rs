//! `SkillProposalToolProvider` — Vorschlagswerkzeuge für Skills (Welle 4,
//! automatische Skill-Entwicklung), nach dem Vorbild von
//! [`crate::agent_definition_tools::AgentDefinitionToolProvider`].
//!
//! # Verantwortung
//! Ein Agent darf einen Skill (Manifest `skill.toml` + Anweisungstext
//! `instructions.md`) **vorschlagen**, aber nie selbst wirksam machen. Der
//! Provider stellt bis zu fünf Werkzeuge bereit:
//! - `skills.validate {skill_toml, instructions, evals?, benchmark?}`: prüft
//!   Manifest, Anweisungstext und den optionalen Eval-Plan. Schreibt nichts,
//!   **immer** registriert.
//! - `skills.list_proposals {}`: listet `<profil>/skills/.proposals/*`, markiert
//!   abgelaufene Vorschläge. Rein lesend, **immer** registriert.
//! - `skills.propose {skill_toml, instructions, evals?, benchmark?}`: validiert
//!   zwingend, berechnet das Werkzeug-/MCP-Delta gegen die Decke des Urhebers
//!   ([`SkillAuthorCeiling`]) und legt den Vorschlag unter
//!   `<profil>/skills/.proposals/<id>/{proposal.json, skill.toml,
//!   instructions.md}` ab. **Aktiviert nie**: das Live-Verzeichnis
//!   `<profil>/skills/<name>/` bleibt unberührt, kein Agent wird verändert.
//!   Nur mit gesetzter Decke registriert.
//! - `skills.commit_proposal {proposal_id, user_confirmed?}`: validiert erneut,
//!   rechnet das Delta gegen die Decke **des committenden Aufrufers** neu und
//!   schreibt danach `<profil>/skills/<name>/`. Freigabepflichtig (gehört in
//!   [`crate::ALWAYS_ASK_TOOLS`]); nur im
//!   [`DefinitionWriteMode::Commit`] und mit gesetzter Decke registriert.
//! - `skills.reject_proposal {proposal_id, reason}`: markiert einen Vorschlag
//!   als verworfen. Nur im [`DefinitionWriteMode::Commit`] und mit Decke.
//!
//! # Das Delta gegen die Urheber-Decke
//! Ein Skill verleiht selbst keine Rechte — seine `tools`/`mcps` sind
//! deklarativ und werden zur Laufzeit weiterhin mit Sandbox und Politik
//! geschnitten. Ein Skill, der Werkzeuge oder MCPs voraussetzt, die sein
//! Urheber selbst nicht hält, verlangt aber eine Prüfung durch den Menschen:
//! ein nicht-leeres [`SkillCapabilityDelta`] ergibt
//! [`ReviewLevel::UserRequired`] (Commit nur mit `user_confirmed = true`),
//! ein leeres [`ReviewLevel::Uia`].
//!
//! # Eval-Hook (Datenmodell, Läufer folgt später)
//! `proposal.json` kann einen Eval-Plan tragen: [`SkillEvalCase`] (realistische
//! Prompts mit [`SkillEvalAssertion`]s) und eine [`SkillBenchmark`]-
//! Zusammenfassung (Passrate/Dauer/Token je Konfiguration als Mittelwert ± σ,
//! Vergleich „mit Skill" gegen eine Baseline). Diese Datei implementiert
//! ausschließlich Datenmodell und Validierung ([`validate_eval_plan`]); ein
//! Läufer, der Paarläufe ausführt und Grader-Ergebnisse erzeugt, ist
//! Folgearbeit.
//!
//! # `user_confirmed` und die Freigabe-Kette
//! Wie bei `agents.commit_proposal`: `user_confirmed` ersetzt die
//! Freigabe-Prüfung des Harness nicht, sondern setzt sie voraus.
//! `skills.commit_proposal` gehört in [`crate::ALWAYS_ASK_TOOLS`]; der Aufruf
//! pausiert beim Nutzer, **bevor** dieser Code läuft.
//!
//! # Schlüsseltypen
//! - [`SkillProposalToolProvider`] — die Werkzeugfläche.
//! - [`SkillProposalStore`] — die gemeinsame Ablage, auch von den
//!   Operator-Kommandos `/skills proposals|review|accept|reject` genutzt.
//! - [`SkillCandidate`], [`SkillValidation`], [`validate_skill_candidate`].
//!
//! # Nebenläufigkeit
//! `Send + Sync`; alle Dateizugriffe sind synchrones, blockierendes I/O.
//!
//! # Fehler
//! Kein Werkzeug gibt `Err` an den Aufrufer zurück — Fehler erscheinen als
//! `ToolOutput::Json` (`{"ok": false, "errors": [...]}`) bzw.
//! `ToolOutput::Error`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use harw_agent_dsl::roles::AgentRoleId;
use harw_config::SkillToml;
use harw_extension_api::contributors::ToolProvider;
use harw_extension_api::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput,
    ToolSpec,
};
use harw_tools::args::parse_args;
use harw_tools::{AdditionalProperties, FunctionToolSpec, JsonSchema, JsonSchemaType};
use serde::{Deserialize, Serialize};

use crate::agent_definition_tools::DefinitionWriteMode;

// ---------------------------------------------------------------------------
// Konstanten
// ---------------------------------------------------------------------------

/// Name des Vorschlags-Unterverzeichnisses unter `<profil>/skills`. Der
/// führende Punkt hält es aus der Skill-Discovery heraus (dort wird nur
/// `skills/*/skill.toml` gelesen; `.proposals/skill.toml` existiert nie).
pub const SKILL_PROPOSALS_DIR_NAME: &str = ".proposals";

/// Dateiname des Skill-Manifests.
const SKILL_MANIFEST: &str = "skill.toml";

/// Dateiname des Anweisungstexts (einziger zulässiger `instructions_file`).
const INSTRUCTIONS_FILE: &str = "instructions.md";

/// Dateiname der Vorschlags-Metadaten.
const PROPOSAL_FILE: &str = "proposal.json";

/// Tage bis zum Ablauf eines Vorschlags (wie bei Agentendefinitionen).
const PROPOSAL_TTL_DAYS: i64 = 7;

/// Rechte-Bits der geschriebenen Dateien.
const SKILL_FILE_MODE: u32 = 0o644;

/// Obergrenze des Anweisungstexts (identisch zu `harw_catalog`).
pub const MAX_INSTRUCTIONS_BYTES: usize = 512 * 1024;

/// Empfohlene Höchstlänge des Anweisungstexts (Progressive Disclosure: Body
/// unter ~500 Zeilen, Details in Referenzdateien).
const RECOMMENDED_MAX_INSTRUCTION_LINES: usize = 500;

/// Höchstlänge eines Skill-Namens.
const MAX_SKILL_NAME_LEN: usize = 64;

/// Höchstlänge eines Werkzeug- oder MCP-Namens.
const MAX_CAPABILITY_NAME_LEN: usize = 128;

/// Höchstzahl an Eval-Fällen je Vorschlag.
pub const MAX_EVALS: usize = 50;

/// Höchstzahl an Assertions je Eval-Fall.
pub const MAX_ASSERTIONS_PER_EVAL: usize = 32;

/// Höchstlänge eines Eval-Prompts in Bytes.
const MAX_EVAL_PROMPT_BYTES: usize = 16 * 1024;

/// Empfohlene Mindestzahl an Eval-Fällen (2–3 realistische Prompts je Skill).
const RECOMMENDED_MIN_EVALS: usize = 2;

/// Höchstlänge eines in `proposal.json` abgelegten Diffs.
const MAX_DIFF_BYTES: usize = 64 * 1024;

/// Zähler für eindeutige Temp- und Vorschlagsnamen innerhalb eines Prozesses.
static COUNTER: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Zeit- und Dateihilfen
// ---------------------------------------------------------------------------

/// Der aktuelle Zeitpunkt (UTC).
fn now() -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc()
}

/// Formatiert `dt` als RFC 3339; ein (praktisch unerreichbarer)
/// Formatierungsfehler fällt auf einen Platzhalter zurück.
fn format_rfc3339(dt: time::OffsetDateTime) -> String {
    dt.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unbekannt".to_owned())
}

/// Ob ein RFC-3339-Zeitstempel in der Vergangenheit liegt. Ein nicht
/// parsbarer Zeitstempel gilt als abgelaufen (fail-closed: ein Vorschlag mit
/// manipuliertem Ablaufdatum lässt sich nicht mehr committen).
fn is_expired(rfc3339_timestamp: &str) -> bool {
    match time::OffsetDateTime::parse(
        rfc3339_timestamp,
        &time::format_description::well_known::Rfc3339,
    ) {
        Ok(expires_at) => expires_at < now(),
        Err(_) => true,
    }
}

/// Erzeugt eine zeitbasierte, slug-sichere Vorschlags-ID (`[a-z0-9-]`).
fn generate_proposal_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("skill-{millis:x}-{:x}-{counter:x}", std::process::id())
}

/// Ob `candidate` eine zulässige Vorschlags-ID ist (nur `[a-z0-9-]`, nicht
/// leer, begrenzte Länge) — verhindert Pfad-Traversal strukturell.
fn is_valid_proposal_id(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate.len() <= 128
        && candidate
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Ob `name` ein zulässiger Skill-Name ist: 1–64 Zeichen aus `[a-z0-9-]`,
/// beginnend mit Buchstabe oder Ziffer (derselbe Zeichensatz wie
/// `harw_catalog::SkillWorkspace`).
#[must_use]
pub fn is_valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SKILL_NAME_LEN
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Ob `name` ein zulässiger Werkzeug-/MCP-Name ist (`fs.read`, `docs`,
/// `mcp:docs/search` …): nicht leer, begrenzt, ohne Leer- und Steuerzeichen.
fn is_valid_capability_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_CAPABILITY_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/'))
}

/// Schreibt `content` atomar (Temp-Datei im Zielverzeichnis + `rename`) nach
/// `path`; legt das Elternverzeichnis bei Bedarf an.
///
/// # Errors
/// Jeder I/O-Fehler; ein angelegter Temp-Pfad wird beim Fehlschlag entfernt
/// (best effort).
fn write_atomic(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("Zielpfad hat kein Elternverzeichnis"))?;
    std::fs::create_dir_all(parent)?;
    let temp_path = parent.join(format!(
        ".skill-proposal-{}-{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(SKILL_FILE_MODE);
    }
    let write_result = options.open(&temp_path).and_then(|mut file| {
        std::io::Write::write_all(&mut file, content).and_then(|()| file.sync_all())
    });
    if let Err(source) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(source);
    }
    std::fs::rename(&temp_path, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp_path);
    })
}

/// Einfaches positionsweises Zeilen-Diff (wie bei Agentendefinitionen);
/// `old = None` ⇒ `"new file"`. Auf [`MAX_DIFF_BYTES`] gekürzt.
fn simple_line_diff(old: Option<&str>, new_content: &str) -> String {
    let Some(old) = old else {
        return "new file".to_owned();
    };
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();
    let mut out = String::new();
    for index in 0..old_lines.len().max(new_lines.len()) {
        match (old_lines.get(index), new_lines.get(index)) {
            (Some(o), Some(n)) if o == n => {}
            (Some(o), Some(n)) => {
                out.push('-');
                out.push_str(o);
                out.push_str("\n+");
                out.push_str(n);
                out.push('\n');
            }
            (Some(o), None) => {
                out.push('-');
                out.push_str(o);
                out.push('\n');
            }
            (None, Some(n)) => {
                out.push('+');
                out.push_str(n);
                out.push('\n');
            }
            (None, None) => {}
        }
        if out.len() > MAX_DIFF_BYTES {
            let mut cut = MAX_DIFF_BYTES;
            while !out.is_char_boundary(cut) {
                cut -= 1;
            }
            out.truncate(cut);
            out.push_str("\n… (Diff gekürzt)\n");
            return out;
        }
    }
    if out.is_empty() {
        "no changes".to_owned()
    } else {
        out
    }
}

/// Erkennt offensichtliche Geheimnisse (API-Schlüssel, PEM-Blöcke,
/// `api_key = …`). Liefert das gefundene Muster.
///
/// Anders als die breitere Prüfung der UIA-Bundles verlangt `sk-` hier
/// mindestens 16 folgende Schlüsselzeichen und keinen alphanumerischen
/// Vorgänger — sonst schlüge jedes Wort wie „risk-based" in einem
/// Anweisungstext an.
fn secret_pattern_in(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    if lower.contains("-----begin") {
        return Some("-----BEGIN");
    }
    if lower.contains("api_key =") || lower.contains("api_key=") {
        return Some("api_key =");
    }
    let bytes = lower.as_bytes();
    let mut search_from = 0;
    while let Some(offset) = lower[search_from..].find("sk-") {
        let start = search_from + offset;
        let preceded_by_word = start > 0 && bytes[start - 1].is_ascii_alphanumeric();
        let key_len = lower[start + 3..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .count();
        if !preceded_by_word && key_len >= 16 {
            return Some("sk-…");
        }
        search_from = start + 3;
    }
    None
}

/// Serde-Hilfe: `null` (strict-Schema-Form für ausgelassene Felder) wird zum
/// `Default` des Zieltyps.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Kebab-case-Name einer Rolle (`AgentRoleId` serialisiert `kebab-case`).
fn role_key(role: AgentRoleId) -> String {
    serde_json::to_value(role)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

// ---------------------------------------------------------------------------
// Eval-Datenmodell
// ---------------------------------------------------------------------------

/// Art einer Eval-Assertion.
///
/// `Grader` wird von einem (späteren) Grader-Agenten bewertet; die übrigen
/// Arten sind skriptbar prüfbar und brauchen einen `value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillAssertionKind {
    /// Urteil eines Grader-Agenten über `text` (Vorgabe).
    #[default]
    Grader,
    /// Die Ausgabe enthält `value` wörtlich.
    Contains,
    /// Die Ausgabe enthält `value` nicht.
    NotContains,
    /// Die Ausgabe passt auf den regulären Ausdruck `value`.
    Regex,
    /// Nach dem Lauf existiert die relative Datei `value` im Arbeitsbereich.
    FileExists,
}

/// Eine prüfbare Aussage über das Ergebnis eines Eval-Laufs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillEvalAssertion {
    /// Menschenlesbare Aussage; zugleich Schlüssel für Benchmark-Verweise.
    pub text: String,
    /// Prüfart; Vorgabe [`SkillAssertionKind::Grader`].
    #[serde(default, deserialize_with = "null_as_default")]
    pub kind: SkillAssertionKind,
    /// Vergleichswert/Muster/Pfad für die skriptbaren Arten.
    #[serde(default)]
    pub value: Option<String>,
}

/// Ein realistischer Test-Prompt für einen Skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillEvalCase {
    /// Eindeutige ID innerhalb des Plans (`[a-z0-9-]`).
    pub id: String,
    /// Der Prompt, so wie ein Nutzer ihn stellen würde.
    pub prompt: String,
    /// Optionale Beschreibung des erwarteten Ergebnisses.
    #[serde(default)]
    pub expected_output: Option<String>,
    /// Relative Eingabedateien des Falls (keine absoluten Pfade, kein `..`).
    #[serde(default, deserialize_with = "null_as_default")]
    pub files: Vec<String>,
    /// Prüfaussagen; dürfen anfangs fehlen („Assertions später").
    #[serde(default, deserialize_with = "null_as_default")]
    pub assertions: Vec<SkillEvalAssertion>,
}

/// Wogegen der Skill verglichen wurde.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkBaseline {
    /// Derselbe Prompt ohne den Skill.
    WithoutSkill,
    /// Derselbe Prompt mit der bisherigen Version des Skills.
    PreviousVersion,
}

/// Mittelwert und Standardabweichung einer Kennzahl über mehrere Läufe.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeanStddev {
    /// Mittelwert.
    pub mean: f64,
    /// Standardabweichung (≥ 0).
    pub stddev: f64,
}

/// Kennzahlen einer Konfiguration (mit Skill bzw. Baseline).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkStats {
    /// Anteil bestandener Assertions, `0.0..=1.0`.
    pub pass_rate: MeanStddev,
    /// Laufzeit in Sekunden.
    pub duration_seconds: MeanStddev,
    /// Verbrauchte Token.
    pub total_tokens: MeanStddev,
}

/// Zusammenfassung eines Paarlauf-Benchmarks (Skill gegen Baseline).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillBenchmark {
    /// Iterationsnummer (`iteration-N`), ≥ 1.
    pub iteration: u32,
    /// Läufe je Konfiguration und Prompt, ≥ 1.
    pub runs_per_configuration: u32,
    /// Art der Baseline.
    pub baseline: BenchmarkBaseline,
    /// Kennzahlen mit dem vorgeschlagenen Skill.
    pub with_skill: BenchmarkStats,
    /// Kennzahlen der Baseline.
    pub baseline_stats: BenchmarkStats,
    /// Assertions (per `text`), die in beiden Konfigurationen gleich
    /// ausfallen und daher nichts unterscheiden.
    #[serde(default, deserialize_with = "null_as_default")]
    pub non_discriminating_assertions: Vec<String>,
    /// Assertions (per `text`) mit schwankendem Ergebnis über die Läufe.
    #[serde(default, deserialize_with = "null_as_default")]
    pub flaky_assertions: Vec<String>,
    /// Freie Beobachtungen des Analysten.
    #[serde(default, deserialize_with = "null_as_default")]
    pub notes: Vec<String>,
}

/// Differenz „mit Skill − Baseline" der Mittelwerte.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkDelta {
    /// Passraten-Differenz (positiv = Skill besser).
    pub pass_rate: f64,
    /// Laufzeit-Differenz in Sekunden (positiv = Skill langsamer).
    pub duration_seconds: f64,
    /// Token-Differenz (positiv = Skill teurer).
    pub total_tokens: f64,
}

impl SkillBenchmark {
    /// Berechnet die Differenz der Mittelwerte (mit Skill − Baseline).
    #[must_use]
    pub fn delta(&self) -> BenchmarkDelta {
        BenchmarkDelta {
            pass_rate: self.with_skill.pass_rate.mean - self.baseline_stats.pass_rate.mean,
            duration_seconds: self.with_skill.duration_seconds.mean
                - self.baseline_stats.duration_seconds.mean,
            total_tokens: self.with_skill.total_tokens.mean - self.baseline_stats.total_tokens.mean,
        }
    }
}

/// Prüft eine Kennzahl auf Endlichkeit und Nicht-Negativität.
fn check_mean_stddev(label: &str, value: &MeanStddev, errors: &mut Vec<String>) {
    if !value.mean.is_finite() || !value.stddev.is_finite() {
        errors.push(format!("benchmark.{label}: Werte müssen endlich sein"));
        return;
    }
    if value.mean < 0.0 || value.stddev < 0.0 {
        errors.push(format!(
            "benchmark.{label}: Mittelwert und Standardabweichung dürfen nicht negativ sein"
        ));
    }
}

/// Prüft die Kennzahlen einer Konfiguration.
fn check_stats(label: &str, stats: &BenchmarkStats, errors: &mut Vec<String>) {
    check_mean_stddev(&format!("{label}.pass_rate"), &stats.pass_rate, errors);
    if stats.pass_rate.mean > 1.0 || stats.pass_rate.stddev > 1.0 {
        errors.push(format!(
            "benchmark.{label}.pass_rate: Passrate muss im Bereich 0.0..=1.0 liegen"
        ));
    }
    check_mean_stddev(
        &format!("{label}.duration_seconds"),
        &stats.duration_seconds,
        errors,
    );
    check_mean_stddev(
        &format!("{label}.total_tokens"),
        &stats.total_tokens,
        errors,
    );
}

/// Ob `path` ein relativer Pfad ohne `..`-, Wurzel- oder Präfix-Komponente ist.
fn is_contained_relative_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

/// Validiert einen Eval-Plan (Fälle + optionaler Benchmark).
///
/// # Beschreibung
/// Harte Fehler: zu viele Fälle/Assertions, doppelte oder ungültige IDs,
/// leere oder überlange Prompts, Pfad-Traversal in `files`, skriptbare
/// Assertions ohne `value`, ein Benchmark ohne Fälle, unplausible Kennzahlen
/// und Benchmark-Verweise auf unbekannte Assertions. Warnungen (nicht
/// blockierend): weniger als zwei Fälle, Fälle ohne Assertions.
///
/// # Arguments
/// - `evals` (`&[SkillEvalCase]`): die Fälle.
/// - `benchmark` (`Option<&SkillBenchmark>`): die optionale Zusammenfassung.
/// - `errors`/`warnings` (`&mut Vec<String>`): Sammelstellen.
pub fn validate_eval_plan(
    evals: &[SkillEvalCase],
    benchmark: Option<&SkillBenchmark>,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    if evals.len() > MAX_EVALS {
        errors.push(format!(
            "evals: höchstens {MAX_EVALS} Fälle erlaubt, {} angegeben",
            evals.len()
        ));
    }
    let mut ids = BTreeSet::new();
    let mut assertion_texts = BTreeSet::new();
    for case in evals {
        let label = format!("evals[{}]", case.id);
        if !is_valid_skill_name(&case.id) {
            errors.push(format!(
                "{label}: id muss aus [a-z0-9-] bestehen (1–{MAX_SKILL_NAME_LEN} Zeichen)"
            ));
        }
        if !ids.insert(case.id.as_str()) {
            errors.push(format!("{label}: doppelte id"));
        }
        if case.prompt.trim().is_empty() {
            errors.push(format!("{label}: prompt darf nicht leer sein"));
        } else if case.prompt.len() > MAX_EVAL_PROMPT_BYTES {
            errors.push(format!(
                "{label}: prompt überschreitet {MAX_EVAL_PROMPT_BYTES} Bytes"
            ));
        }
        if let Some(pattern) = secret_pattern_in(&case.prompt)
            .or_else(|| case.expected_output.as_deref().and_then(secret_pattern_in))
        {
            errors.push(format!(
                "{label}: enthält ein offensichtliches Geheimnis-Muster ('{pattern}')"
            ));
        }
        for file in &case.files {
            if !is_contained_relative_path(file) {
                errors.push(format!(
                    "{label}: Datei '{file}' muss ein relativer Pfad ohne '..' sein"
                ));
            }
        }
        if case.assertions.len() > MAX_ASSERTIONS_PER_EVAL {
            errors.push(format!(
                "{label}: höchstens {MAX_ASSERTIONS_PER_EVAL} Assertions erlaubt"
            ));
        }
        if case.assertions.is_empty() {
            warnings.push(format!(
                "{label}: noch ohne Assertions — ein Grader kann diesen Fall nicht bewerten"
            ));
        }
        for assertion in &case.assertions {
            if assertion.text.trim().is_empty() {
                errors.push(format!("{label}: Assertion-Text darf nicht leer sein"));
            }
            assertion_texts.insert(assertion.text.as_str());
            let value = assertion.value.as_deref().unwrap_or_default();
            match assertion.kind {
                SkillAssertionKind::Grader => {}
                SkillAssertionKind::Contains
                | SkillAssertionKind::NotContains
                | SkillAssertionKind::Regex => {
                    if value.is_empty() {
                        errors.push(format!(
                            "{label}: Assertion '{}' ({:?}) braucht einen value",
                            assertion.text, assertion.kind
                        ));
                    }
                }
                SkillAssertionKind::FileExists => {
                    if !is_contained_relative_path(value) {
                        errors.push(format!(
                            "{label}: Assertion '{}' (FileExists) braucht einen relativen \
                             Pfad ohne '..' als value",
                            assertion.text
                        ));
                    }
                }
            }
        }
    }
    if !evals.is_empty() && evals.len() < RECOMMENDED_MIN_EVALS {
        warnings.push(format!(
            "evals: empfohlen sind mindestens {RECOMMENDED_MIN_EVALS} realistische Prompts"
        ));
    }

    let Some(benchmark) = benchmark else {
        return;
    };
    if evals.is_empty() {
        errors.push("benchmark: ohne evals ist ein Benchmark nicht nachvollziehbar".to_owned());
    }
    if benchmark.iteration == 0 {
        errors.push("benchmark.iteration muss ≥ 1 sein".to_owned());
    }
    if benchmark.runs_per_configuration == 0 {
        errors.push("benchmark.runs_per_configuration muss ≥ 1 sein".to_owned());
    }
    check_stats("with_skill", &benchmark.with_skill, errors);
    check_stats("baseline_stats", &benchmark.baseline_stats, errors);
    for (field, referenced) in [
        (
            "non_discriminating_assertions",
            &benchmark.non_discriminating_assertions,
        ),
        ("flaky_assertions", &benchmark.flaky_assertions),
    ] {
        for text in referenced {
            if !assertion_texts.contains(text.as_str()) {
                errors.push(format!(
                    "benchmark.{field}: '{text}' ist keine Assertion eines Eval-Falls"
                ));
            }
        }
    }
    if benchmark.runs_per_configuration == 1 {
        warnings.push(
            "benchmark: nur ein Lauf je Konfiguration — Standardabweichungen sind nicht \
             aussagekräftig"
                .to_owned(),
        );
    }
}

// ---------------------------------------------------------------------------
// Kandidat und Validierung
// ---------------------------------------------------------------------------

/// Ein Skill-Kandidat, wie ihn `skills.validate`/`skills.propose` erhalten.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SkillCandidate {
    /// Quelltext des Manifests (`SkillToml`).
    pub skill_toml: String,
    /// Inhalt von `instructions.md`.
    pub instructions: String,
    /// Optionaler Eval-Plan.
    pub evals: Vec<SkillEvalCase>,
    /// Optionale Benchmark-Zusammenfassung.
    pub benchmark: Option<SkillBenchmark>,
}

/// Ergebnis von [`validate_skill_candidate`].
#[derive(Debug, Clone)]
pub struct SkillValidation {
    /// Das geparste Manifest, sofern es parsbar war.
    pub skill: Option<SkillToml>,
    /// Blockierende Fehler.
    pub errors: Vec<String>,
    /// Nicht blockierende Hinweise.
    pub warnings: Vec<String>,
}

impl SkillValidation {
    /// Das Manifest, wenn die Validierung fehlerfrei war.
    #[must_use]
    pub fn accepted(&self) -> Option<&SkillToml> {
        if self.errors.is_empty() {
            self.skill.as_ref()
        } else {
            None
        }
    }
}

/// Prüft eine Namensliste (Werkzeuge oder MCPs) auf Form und Duplikate.
fn check_capability_list(field: &str, names: &[String], errors: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    for name in names {
        if !is_valid_capability_name(name) {
            errors.push(format!("{field}: '{name}' ist kein gültiger Name"));
        }
        if !seen.insert(name.as_str()) {
            errors.push(format!("{field}: '{name}' ist doppelt angegeben"));
        }
    }
}

/// Validiert Manifest, Anweisungstext und Eval-Plan eines Kandidaten.
///
/// # Beschreibung
/// Das Manifest muss als [`SkillToml`] parsen (unbekannte Felder werden
/// abgelehnt), einen gültigen Namen und eine nicht-leere Beschreibung tragen
/// (die Beschreibung entscheidet über das Auslösen des Skills);
/// `instructions_file` darf nur fehlen oder `"instructions.md"` sein. Der
/// Anweisungstext darf nicht leer sein und höchstens
/// [`MAX_INSTRUCTIONS_BYTES`] groß sein. Kein Feld darf ein offensichtliches
/// Geheimnis enthalten. Danach [`validate_eval_plan`].
///
/// # Returns
/// Eine [`SkillValidation`]; [`SkillValidation::accepted`] liefert das
/// Manifest nur bei fehlerfreier Prüfung.
#[must_use]
pub fn validate_skill_candidate(candidate: &SkillCandidate) -> SkillValidation {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let skill = match toml::from_str::<SkillToml>(&candidate.skill_toml) {
        Ok(skill) => Some(skill),
        Err(error) => {
            errors.push(format!(
                "skill_toml ist kein gültiges Skill-Manifest: {error}"
            ));
            None
        }
    };
    if let Some(skill) = &skill {
        if !is_valid_skill_name(&skill.name) {
            errors.push(format!(
                "name '{}' muss aus [a-z0-9-] bestehen, mit Buchstabe/Ziffer beginnen und \
                 höchstens {MAX_SKILL_NAME_LEN} Zeichen lang sein",
                skill.name
            ));
        }
        if skill.description.trim().is_empty() {
            errors.push(
                "description darf nicht leer sein — sie entscheidet, wann der Skill greift"
                    .to_owned(),
            );
        } else if skill.description.trim().len() < 20 {
            warnings.push(
                "description ist sehr kurz — konkrete Auslöser (wann, wofür) verbessern die \
                 Trefferquote"
                    .to_owned(),
            );
        }
        match skill.instructions_file.as_deref() {
            None | Some(INSTRUCTIONS_FILE) => {}
            Some(other) => errors.push(format!(
                "instructions_file muss fehlen oder \"{INSTRUCTIONS_FILE}\" sein, nicht '{other}'"
            )),
        }
        check_capability_list("tools", &skill.tools, &mut errors);
        check_capability_list("mcps", &skill.mcps, &mut errors);
    }
    if candidate.instructions.trim().is_empty() {
        errors.push("instructions dürfen nicht leer sein".to_owned());
    } else if candidate.instructions.len() > MAX_INSTRUCTIONS_BYTES {
        errors.push(format!(
            "instructions überschreiten {MAX_INSTRUCTIONS_BYTES} Bytes ({})",
            candidate.instructions.len()
        ));
    } else if candidate.instructions.lines().count() > RECOMMENDED_MAX_INSTRUCTION_LINES {
        warnings.push(format!(
            "instructions haben mehr als {RECOMMENDED_MAX_INSTRUCTION_LINES} Zeilen — Details \
             besser in Referenzdateien auslagern"
        ));
    }
    for (field, content) in [
        ("skill_toml", candidate.skill_toml.as_str()),
        ("instructions", candidate.instructions.as_str()),
    ] {
        if let Some(pattern) = secret_pattern_in(content) {
            errors.push(format!(
                "Feld '{field}' enthält ein offensichtliches Geheimnis-Muster ('{pattern}') \
                 und wird abgelehnt"
            ));
        }
    }
    validate_eval_plan(
        &candidate.evals,
        candidate.benchmark.as_ref(),
        &mut errors,
        &mut warnings,
    );
    SkillValidation {
        skill,
        errors,
        warnings,
    }
}

// ---------------------------------------------------------------------------
// Decke, Delta, Prüfstufe
// ---------------------------------------------------------------------------

/// Die Werkzeuge und MCPs, die der Urheber (bzw. Committer) eines Skills
/// selbst hält.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillAuthorCeiling {
    /// Organisatorische Rolle des Urhebers.
    pub role: AgentRoleId,
    /// Werkzeugnamen, die der Urheber sieht/benutzen darf.
    pub tools: BTreeSet<String>,
    /// MCP-Namen, die der Urheber benutzen darf.
    pub mcps: BTreeSet<String>,
}

/// Was ein Skill über die Decke seines Urhebers hinaus voraussetzt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCapabilityDelta {
    /// Werkzeuge, die der Skill nennt, der Urheber aber nicht hält.
    #[serde(default, deserialize_with = "null_as_default")]
    pub added_tools: Vec<String>,
    /// MCPs, die der Skill nennt, der Urheber aber nicht hält.
    #[serde(default, deserialize_with = "null_as_default")]
    pub added_mcps: Vec<String>,
}

impl SkillCapabilityDelta {
    /// Berechnet das Delta von `skill` gegen `ceiling` (sortiert, ohne
    /// Duplikate).
    #[must_use]
    pub fn compute(skill: &SkillToml, ceiling: &SkillAuthorCeiling) -> Self {
        let added = |names: &[String], held: &BTreeSet<String>| -> Vec<String> {
            names
                .iter()
                .filter(|name| !held.contains(*name))
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        Self {
            added_tools: added(&skill.tools, &ceiling.tools),
            added_mcps: added(&skill.mcps, &ceiling.mcps),
        }
    }

    /// `true`, wenn der Skill nichts über die Decke hinaus voraussetzt.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added_tools.is_empty() && self.added_mcps.is_empty()
    }
}

/// Wer einen Vorschlag prüfen muss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewLevel {
    /// Die UIA darf committen (leeres Delta).
    Uia,
    /// Nur mit ausdrücklicher Nutzerbestätigung (nicht-leeres Delta).
    UserRequired,
}

impl ReviewLevel {
    /// Leitet die Prüfstufe aus einem Delta ab.
    #[must_use]
    pub fn for_delta(delta: &SkillCapabilityDelta) -> Self {
        if delta.is_empty() {
            Self::Uia
        } else {
            Self::UserRequired
        }
    }

    /// Der Schlüssel, wie er in `proposal.json` steht.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uia => "uia",
            Self::UserRequired => "user_required",
        }
    }
}

/// Lebenszyklus eines Vorschlags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillProposalStatus {
    /// Abgelegt, noch nicht entschieden.
    PendingReview,
    /// Ins Live-Verzeichnis übernommen.
    Committed,
    /// Verworfen.
    Rejected,
}

impl SkillProposalStatus {
    /// Der Schlüssel, wie er in `proposal.json` steht.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PendingReview => "pending_review",
            Self::Committed => "committed",
            Self::Rejected => "rejected",
        }
    }
}

/// Inhalt von `proposal.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillProposalMeta {
    /// Die Vorschlags-ID (= Verzeichnisname).
    pub proposal_id: String,
    /// Immer `"skill"`.
    pub kind: String,
    /// Skill-Name aus dem Manifest.
    pub name: String,
    /// Beschreibung aus dem Manifest.
    pub description: String,
    /// Anlagezeitpunkt (RFC 3339).
    pub created_at: String,
    /// Ablaufzeitpunkt (RFC 3339).
    pub expires_at: String,
    /// Status.
    pub status: SkillProposalStatus,
    /// Prüfstufe zum Zeitpunkt des Ablegens.
    pub review_level: ReviewLevel,
    /// Rolle des Urhebers (kebab-case).
    pub author_role: Option<String>,
    /// Delta gegen die Decke des Urhebers.
    pub capability_delta: SkillCapabilityDelta,
    /// Ob ein gleichnamiger Skill im Profil bereits existiert (Update).
    pub replaces_existing: bool,
    /// Zeilen-Diff des Manifests gegen den bestehenden Skill.
    pub manifest_diff: String,
    /// Zeilen-Diff des Anweisungstexts gegen den bestehenden Skill.
    pub instructions_diff: String,
    /// Validierungshinweise zum Zeitpunkt des Ablegens.
    #[serde(default, deserialize_with = "null_as_default")]
    pub warnings: Vec<String>,
    /// Eval-Plan (Eval-Hook; Läufer folgt später).
    #[serde(default, deserialize_with = "null_as_default")]
    pub evals: Vec<SkillEvalCase>,
    /// Benchmark-Zusammenfassung (Eval-Hook).
    #[serde(default)]
    pub benchmark: Option<SkillBenchmark>,
    /// Commit-Zeitpunkt.
    #[serde(default)]
    pub committed_at: Option<String>,
    /// `"uia"` (Werkzeug) oder `"operator"` (`/skills accept`).
    #[serde(default)]
    pub committed_by: Option<String>,
    /// Zielverzeichnis des Commits.
    #[serde(default)]
    pub committed_path: Option<String>,
    /// Ob beim Commit eine Nutzerbestätigung vorlag.
    #[serde(default)]
    pub user_confirmed: Option<bool>,
    /// Ablehnungszeitpunkt.
    #[serde(default)]
    pub rejected_at: Option<String>,
    /// Ablehnungsgrund.
    #[serde(default)]
    pub reason: Option<String>,
}

impl SkillProposalMeta {
    /// Kurzer Eval-Zustand: `"none"`, `"defined"` oder `"benchmarked"`.
    #[must_use]
    pub fn eval_status(&self) -> &'static str {
        match (self.evals.is_empty(), self.benchmark.is_some()) {
            (true, _) => "none",
            (false, false) => "defined",
            (false, true) => "benchmarked",
        }
    }
}

// ---------------------------------------------------------------------------
// Ablage
// ---------------------------------------------------------------------------

/// Fehler der [`SkillProposalStore`]-Operationen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillProposalError {
    /// Der Kandidat besteht die Validierung nicht.
    Invalid(Vec<String>),
    /// Die Vorschlags-ID ist syntaktisch unzulässig.
    InvalidId(String),
    /// Kein Vorschlag mit dieser ID.
    NotFound(String),
    /// Der Vorschlag ist abgelaufen.
    Expired(String),
    /// Zustandskonflikt (falscher Status, Namenskonflikt, Manipulation).
    Conflict(String),
    /// Commit verlangt eine Nutzerbestätigung.
    RequiresUserConfirmation {
        /// Das neu berechnete Delta.
        delta: SkillCapabilityDelta,
    },
    /// Lese-/Schreibfehler.
    Io(String),
}

impl fmt::Display for SkillProposalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(errors) => write!(f, "Skill-Kandidat ungültig: {}", errors.join("; ")),
            Self::InvalidId(id) => write!(f, "'{id}' ist keine gültige proposal_id"),
            Self::NotFound(id) => write!(f, "kein Skill-Vorschlag '{id}'"),
            Self::Expired(id) => write!(f, "Skill-Vorschlag '{id}' ist abgelaufen"),
            Self::Conflict(reason) => write!(f, "{reason}"),
            Self::RequiresUserConfirmation { delta } => write!(
                f,
                "review_level ist \"user_required\" (zusätzliche Werkzeuge: [{}], MCPs: [{}]) \
                 — user_confirmed = true ist Pflicht",
                delta.added_tools.join(", "),
                delta.added_mcps.join(", ")
            ),
            Self::Io(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for SkillProposalError {}

/// Ein gelesener Vorschlag samt Dateien.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedSkillProposal {
    /// Metadaten.
    pub meta: SkillProposalMeta,
    /// Vorgeschlagenes Manifest.
    pub skill_toml: String,
    /// Vorgeschlagener Anweisungstext.
    pub instructions: String,
    /// Ob der Vorschlag abgelaufen ist.
    pub expired: bool,
}

/// Ein Eintrag von [`SkillProposalStore::list`].
#[derive(Debug, Clone, PartialEq)]
pub enum SkillProposalListing {
    /// Ein lesbarer Vorschlag.
    Proposal {
        /// Metadaten (geboxt: die Variante wäre sonst ~600 Byte größer).
        meta: Box<SkillProposalMeta>,
        /// Ob er abgelaufen ist.
        expired: bool,
    },
    /// Ein Verzeichnis ohne lesbares `proposal.json`.
    Broken {
        /// Verzeichnisname.
        proposal_id: String,
        /// Grund.
        error: String,
    },
}

/// Wer einen Commit auslöst.
#[derive(Debug, Clone, Copy)]
pub enum CommitAuthority<'a> {
    /// Ein Agent über `skills.commit_proposal`: Delta wird gegen **seine**
    /// Decke neu berechnet.
    Ceiling {
        /// Die Decke des committenden Aufrufers.
        ceiling: &'a SkillAuthorCeiling,
        /// Ob eine Nutzerbestätigung vorliegt.
        user_confirmed: bool,
    },
    /// Der Mensch über `/skills accept` — die Operator-Entscheidung ist die
    /// Nutzerbestätigung selbst.
    Operator,
}

/// Ergebnis eines erfolgreichen Commits.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillCommitOutcome {
    /// Aktualisierte Metadaten.
    pub meta: SkillProposalMeta,
    /// Das geschriebene Skill-Verzeichnis.
    pub skill_dir: PathBuf,
    /// Fehler beim Status-Update nach bereits erfolgtem Schreiben.
    pub status_update_error: Option<String>,
}

/// Ein im Profil bereits vorhandener gleichnamiger Skill.
struct ExistingSkill {
    manifest_source: String,
    instructions: Option<String>,
}

/// Die Ablage der Skill-Vorschläge eines Profils.
///
/// # Beschreibung
/// `profile_skills_dir` ist `<profil>/skills`; Vorschläge liegen unter
/// `<profil>/skills/.proposals/<id>/`, ein Commit schreibt
/// `<profil>/skills/<name>/{instructions.md, skill.toml}` (Anweisungen zuerst,
/// damit die Discovery nie ein Manifest ohne Anweisungstext sieht).
///
/// # Nebenläufigkeit
/// `Send + Sync`; hält nur einen Pfad. Parallele Commits desselben Vorschlags
/// sind nicht gesperrt (die Freigabekette serialisiert sie in der Praxis).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillProposalStore {
    profile_skills_dir: PathBuf,
}

impl SkillProposalStore {
    /// Erstellt die Ablage für `<profil>/skills`.
    #[must_use]
    pub fn new(profile_skills_dir: PathBuf) -> Self {
        Self { profile_skills_dir }
    }

    /// Das Skill-Verzeichnis des Profils.
    #[must_use]
    pub fn profile_skills_dir(&self) -> &Path {
        &self.profile_skills_dir
    }

    /// Das Vorschlags-Wurzelverzeichnis.
    #[must_use]
    pub fn proposals_root(&self) -> PathBuf {
        self.profile_skills_dir.join(SKILL_PROPOSALS_DIR_NAME)
    }

    fn proposal_dir(&self, proposal_id: &str) -> PathBuf {
        self.proposals_root().join(proposal_id)
    }

    fn read_meta(&self, proposal_id: &str) -> Result<SkillProposalMeta, SkillProposalError> {
        let path = self.proposal_dir(proposal_id).join(PROPOSAL_FILE);
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(SkillProposalError::NotFound(proposal_id.to_owned()));
            }
            Err(error) => {
                return Err(SkillProposalError::Io(format!(
                    "{} nicht lesbar: {error}",
                    path.display()
                )));
            }
        };
        let meta: SkillProposalMeta = serde_json::from_str(&source).map_err(|error| {
            SkillProposalError::Io(format!(
                "{} ist kein gültiges Vorschlags-JSON: {error}",
                path.display()
            ))
        })?;
        if meta.proposal_id != proposal_id || meta.kind != "skill" {
            return Err(SkillProposalError::Conflict(format!(
                "{} gehört nicht zum Skill-Vorschlag '{proposal_id}'",
                path.display()
            )));
        }
        Ok(meta)
    }

    fn write_meta(&self, meta: &SkillProposalMeta) -> Result<(), SkillProposalError> {
        let bytes = serde_json::to_vec_pretty(meta).map_err(|error| {
            SkillProposalError::Io(format!(
                "Vorschlags-Metadaten nicht serialisierbar: {error}"
            ))
        })?;
        let path = self.proposal_dir(&meta.proposal_id).join(PROPOSAL_FILE);
        write_atomic(&path, &bytes).map_err(|error| {
            SkillProposalError::Io(format!("{} nicht schreibbar: {error}", path.display()))
        })
    }

    /// Sucht einen gleichnamigen Skill direkt im Profil.
    ///
    /// # Errors
    /// [`SkillProposalError::Conflict`], wenn der Name bereits unter einem
    /// **anderen** Verzeichnisnamen definiert ist (ein zweites Manifest würde
    /// die Discovery mehrdeutig machen) oder `<name>/skill.toml` einen anderen
    /// Namen trägt bzw. unlesbar ist (fail-closed: nie stillschweigend
    /// überschreiben); [`SkillProposalError::Io`] bei Lesefehlern.
    fn existing_skill(&self, name: &str) -> Result<Option<ExistingSkill>, SkillProposalError> {
        let expected = self.profile_skills_dir.join(name);
        let entries = match std::fs::read_dir(&self.profile_skills_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(SkillProposalError::Io(format!(
                    "{} nicht lesbar: {error}",
                    self.profile_skills_dir.display()
                )));
            }
        };
        let mut found = None;
        for entry in entries {
            let entry = entry.map_err(|error| {
                SkillProposalError::Io(format!(
                    "{} nicht lesbar: {error}",
                    self.profile_skills_dir.display()
                ))
            })?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                continue;
            }
            let manifest = path.join(SKILL_MANIFEST);
            let source = match std::fs::read_to_string(&manifest) {
                Ok(source) => source,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(SkillProposalError::Io(format!(
                        "{} nicht lesbar: {error}",
                        manifest.display()
                    )));
                }
            };
            let parsed = match toml::from_str::<SkillToml>(&source) {
                Ok(parsed) => parsed,
                Err(error) if path == expected => {
                    return Err(SkillProposalError::Conflict(format!(
                        "vorhandenes Manifest {} ist nicht parsbar und wird nicht \
                         überschrieben: {error}",
                        manifest.display()
                    )));
                }
                Err(_) => continue,
            };
            if parsed.name == name {
                if path != expected {
                    return Err(SkillProposalError::Conflict(format!(
                        "Skill '{name}' ist bereits in {} definiert (abweichender \
                         Verzeichnisname) — ein zweites Manifest wird nicht angelegt",
                        path.display()
                    )));
                }
                let instructions = match parsed.instructions_file.as_deref() {
                    None | Some(INSTRUCTIONS_FILE) => {
                        std::fs::read_to_string(path.join(INSTRUCTIONS_FILE)).ok()
                    }
                    Some(_) => None,
                };
                found = Some(ExistingSkill {
                    manifest_source: source,
                    instructions,
                });
            } else if path == expected {
                return Err(SkillProposalError::Conflict(format!(
                    "{} trägt name '{}' statt '{name}' — wird nicht überschrieben",
                    manifest.display(),
                    parsed.name
                )));
            }
        }
        Ok(found)
    }

    /// Legt einen Vorschlag ab. Aktiviert nie etwas.
    ///
    /// # Arguments
    /// - `candidate` (`&SkillCandidate`): Manifest, Anweisungen, Eval-Plan.
    /// - `ceiling` (`&SkillAuthorCeiling`): die Decke des Urhebers.
    ///
    /// # Returns
    /// Die geschriebenen Metadaten.
    ///
    /// # Errors
    /// [`SkillProposalError::Invalid`] bei Validierungsfehlern (dann wird
    /// nichts geschrieben), [`SkillProposalError::Conflict`] bei einem
    /// Namenskonflikt im Profil, [`SkillProposalError::Io`] bei Schreibfehlern.
    pub fn propose(
        &self,
        candidate: &SkillCandidate,
        ceiling: &SkillAuthorCeiling,
    ) -> Result<SkillProposalMeta, SkillProposalError> {
        let validation = validate_skill_candidate(candidate);
        let Some(skill) = validation.accepted().cloned() else {
            return Err(SkillProposalError::Invalid(validation.errors));
        };
        let existing = self.existing_skill(&skill.name)?;
        let delta = SkillCapabilityDelta::compute(&skill, ceiling);
        let review_level = ReviewLevel::for_delta(&delta);
        let proposal_id = generate_proposal_id();
        let dir = self.proposal_dir(&proposal_id);
        for (file, content) in [
            (INSTRUCTIONS_FILE, candidate.instructions.as_str()),
            (SKILL_MANIFEST, candidate.skill_toml.as_str()),
        ] {
            write_atomic(&dir.join(file), content.as_bytes()).map_err(|error| {
                SkillProposalError::Io(format!("Vorschlag nicht schreibbar ({file}): {error}"))
            })?;
        }
        let created_at = now();
        let expires_at = created_at + time::Duration::days(PROPOSAL_TTL_DAYS);
        let meta = SkillProposalMeta {
            proposal_id,
            kind: "skill".to_owned(),
            name: skill.name.clone(),
            description: skill.description.clone(),
            created_at: format_rfc3339(created_at),
            expires_at: format_rfc3339(expires_at),
            status: SkillProposalStatus::PendingReview,
            review_level,
            author_role: Some(role_key(ceiling.role)),
            capability_delta: delta,
            replaces_existing: existing.is_some(),
            manifest_diff: simple_line_diff(
                existing
                    .as_ref()
                    .map(|existing| existing.manifest_source.as_str()),
                &candidate.skill_toml,
            ),
            instructions_diff: simple_line_diff(
                existing
                    .as_ref()
                    .and_then(|existing| existing.instructions.as_deref()),
                &candidate.instructions,
            ),
            warnings: validation.warnings,
            evals: candidate.evals.clone(),
            benchmark: candidate.benchmark.clone(),
            committed_at: None,
            committed_by: None,
            committed_path: None,
            user_confirmed: None,
            rejected_at: None,
            reason: None,
        };
        self.write_meta(&meta)?;
        Ok(meta)
    }

    /// Listet alle Vorschläge, sortiert nach ID.
    ///
    /// # Errors
    /// [`SkillProposalError::Io`], wenn das Vorschlagsverzeichnis existiert,
    /// aber nicht lesbar ist.
    pub fn list(&self) -> Result<Vec<SkillProposalListing>, SkillProposalError> {
        let root = self.proposals_root();
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(SkillProposalError::Io(format!(
                    "{} nicht lesbar: {error}",
                    root.display()
                )));
            }
        };
        let mut listings = Vec::new();
        for entry in entries.flatten() {
            if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                continue;
            }
            let proposal_id = entry.file_name().to_string_lossy().into_owned();
            if !is_valid_proposal_id(&proposal_id) {
                continue;
            }
            match self.read_meta(&proposal_id) {
                Ok(meta) => {
                    let expired = is_expired(&meta.expires_at);
                    listings.push(SkillProposalListing::Proposal {
                        meta: Box::new(meta),
                        expired,
                    });
                }
                Err(error) => listings.push(SkillProposalListing::Broken {
                    proposal_id,
                    error: error.to_string(),
                }),
            }
        }
        listings.sort_by(|left, right| listing_id(left).cmp(listing_id(right)));
        Ok(listings)
    }

    /// Liest einen Vorschlag samt Manifest und Anweisungstext.
    ///
    /// # Errors
    /// [`SkillProposalError::InvalidId`], [`SkillProposalError::NotFound`],
    /// [`SkillProposalError::Conflict`] (fremdes `proposal.json`),
    /// [`SkillProposalError::Io`].
    pub fn load(&self, proposal_id: &str) -> Result<LoadedSkillProposal, SkillProposalError> {
        if !is_valid_proposal_id(proposal_id) {
            return Err(SkillProposalError::InvalidId(proposal_id.to_owned()));
        }
        let meta = self.read_meta(proposal_id)?;
        let dir = self.proposal_dir(proposal_id);
        let read = |file: &str| {
            std::fs::read_to_string(dir.join(file)).map_err(|error| {
                SkillProposalError::Io(format!(
                    "{file} von Vorschlag '{proposal_id}' nicht lesbar: {error}"
                ))
            })
        };
        let skill_toml = read(SKILL_MANIFEST)?;
        let instructions = read(INSTRUCTIONS_FILE)?;
        let expired = is_expired(&meta.expires_at);
        Ok(LoadedSkillProposal {
            meta,
            skill_toml,
            instructions,
            expired,
        })
    }

    /// Übernimmt einen offenen, nicht abgelaufenen Vorschlag ins Profil.
    ///
    /// # Beschreibung
    /// Validiert Manifest, Anweisungen und Eval-Plan erneut. Mit
    /// [`CommitAuthority::Ceiling`] wird das Delta gegen die Decke des
    /// Committers neu berechnet; bei [`ReviewLevel::UserRequired`] ohne
    /// `user_confirmed` wird abgelehnt. Schreibt `instructions.md`, dann
    /// `skill.toml`, danach den Status `committed`. Kein Agent wird verändert.
    ///
    /// # Errors
    /// Alle Varianten von [`SkillProposalError`] außer `Invalid` bei gültigem
    /// Kandidaten.
    pub fn commit(
        &self,
        proposal_id: &str,
        authority: CommitAuthority<'_>,
    ) -> Result<SkillCommitOutcome, SkillProposalError> {
        let loaded = self.load(proposal_id)?;
        if loaded.meta.status != SkillProposalStatus::PendingReview {
            return Err(SkillProposalError::Conflict(format!(
                "Skill-Vorschlag '{proposal_id}' hat status '{}', erwartet 'pending_review'",
                loaded.meta.status.as_str()
            )));
        }
        if loaded.expired {
            return Err(SkillProposalError::Expired(proposal_id.to_owned()));
        }
        let candidate = SkillCandidate {
            skill_toml: loaded.skill_toml.clone(),
            instructions: loaded.instructions.clone(),
            evals: loaded.meta.evals.clone(),
            benchmark: loaded.meta.benchmark.clone(),
        };
        let validation = validate_skill_candidate(&candidate);
        let Some(skill) = validation.accepted().cloned() else {
            return Err(SkillProposalError::Invalid(validation.errors));
        };
        if skill.name != loaded.meta.name {
            return Err(SkillProposalError::Conflict(format!(
                "Manifest von '{proposal_id}' trägt name '{}', proposal.json aber '{}' — \
                 Vorschlag wurde verändert",
                skill.name, loaded.meta.name
            )));
        }
        let (review_level, delta, user_confirmed, committed_by) = match authority {
            CommitAuthority::Ceiling {
                ceiling,
                user_confirmed,
            } => {
                let delta = SkillCapabilityDelta::compute(&skill, ceiling);
                let level = ReviewLevel::for_delta(&delta);
                if level == ReviewLevel::UserRequired && !user_confirmed {
                    return Err(SkillProposalError::RequiresUserConfirmation { delta });
                }
                (level, delta, user_confirmed, "uia")
            }
            CommitAuthority::Operator => (
                loaded.meta.review_level,
                loaded.meta.capability_delta.clone(),
                true,
                "operator",
            ),
        };
        // Namenskonflikte erneut prüfen: seit dem Ablegen kann ein
        // gleichnamiger Skill unter anderem Verzeichnisnamen entstanden sein.
        let _ = self.existing_skill(&skill.name)?;
        let skill_dir = self.profile_skills_dir.join(&skill.name);
        for (file, content) in [
            (INSTRUCTIONS_FILE, candidate.instructions.as_str()),
            (SKILL_MANIFEST, candidate.skill_toml.as_str()),
        ] {
            write_atomic(&skill_dir.join(file), content.as_bytes()).map_err(|error| {
                SkillProposalError::Io(format!(
                    "{} nicht schreibbar: {error}",
                    skill_dir.join(file).display()
                ))
            })?;
        }
        let mut meta = loaded.meta;
        meta.status = SkillProposalStatus::Committed;
        meta.review_level = review_level;
        meta.capability_delta = delta;
        meta.committed_at = Some(format_rfc3339(now()));
        meta.committed_by = Some(committed_by.to_owned());
        meta.committed_path = Some(skill_dir.display().to_string());
        meta.user_confirmed = Some(user_confirmed);
        // Das Ziel ist bereits geschrieben; ein Fehler beim Status-Update ist
        // nicht mehr rückgängig zu machen und wird nur gemeldet.
        let status_update_error = self.write_meta(&meta).err().map(|error| {
            format!("Skill geschrieben, aber Vorschlags-Status nicht aktualisierbar: {error}")
        });
        Ok(SkillCommitOutcome {
            meta,
            skill_dir,
            status_update_error,
        })
    }

    /// Verwirft einen offenen Vorschlag mit Begründung.
    ///
    /// # Errors
    /// [`SkillProposalError::InvalidId`], [`SkillProposalError::NotFound`],
    /// [`SkillProposalError::Conflict`] (nicht offen, leere Begründung),
    /// [`SkillProposalError::Io`].
    pub fn reject(
        &self,
        proposal_id: &str,
        reason: &str,
    ) -> Result<SkillProposalMeta, SkillProposalError> {
        if !is_valid_proposal_id(proposal_id) {
            return Err(SkillProposalError::InvalidId(proposal_id.to_owned()));
        }
        if reason.trim().is_empty() {
            return Err(SkillProposalError::Conflict(
                "eine Ablehnung braucht eine Begründung".to_owned(),
            ));
        }
        let mut meta = self.read_meta(proposal_id)?;
        if meta.status != SkillProposalStatus::PendingReview {
            return Err(SkillProposalError::Conflict(format!(
                "Skill-Vorschlag '{proposal_id}' hat status '{}', erwartet 'pending_review'",
                meta.status.as_str()
            )));
        }
        meta.status = SkillProposalStatus::Rejected;
        meta.rejected_at = Some(format_rfc3339(now()));
        meta.reason = Some(reason.to_owned());
        self.write_meta(&meta)?;
        Ok(meta)
    }
}

/// Sortierschlüssel eines Listeneintrags.
fn listing_id(listing: &SkillProposalListing) -> &str {
    match listing {
        SkillProposalListing::Proposal { meta, .. } => &meta.proposal_id,
        SkillProposalListing::Broken { proposal_id, .. } => proposal_id,
    }
}

// ---------------------------------------------------------------------------
// JSON-Schema-Bausteine
// ---------------------------------------------------------------------------

fn typed(schema_type: JsonSchemaType, description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(schema_type),
        description: Some(description.to_owned()),
        ..Default::default()
    }
}

fn array_of(items: JsonSchema, description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::Array),
        description: Some(description.to_owned()),
        items: Some(Box::new(items)),
        ..Default::default()
    }
}

fn object(properties: Vec<(&str, JsonSchema)>, required: &[&str], description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        description: (!description.is_empty()).then(|| description.to_owned()),
        properties: Some(
            properties
                .into_iter()
                .map(|(name, schema)| (name.to_owned(), schema))
                .collect::<BTreeMap<_, _>>(),
        ),
        required: Some(required.iter().map(|name| (*name).to_owned()).collect()),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..Default::default()
    }
}

fn mean_stddev_schema(description: &str) -> JsonSchema {
    object(
        vec![
            ("mean", typed(JsonSchemaType::Number, "Mittelwert.")),
            (
                "stddev",
                typed(JsonSchemaType::Number, "Standardabweichung (≥ 0)."),
            ),
        ],
        &["mean", "stddev"],
        description,
    )
}

fn stats_schema(description: &str) -> JsonSchema {
    object(
        vec![
            (
                "pass_rate",
                mean_stddev_schema("Anteil bestandener Assertions, 0.0..=1.0."),
            ),
            (
                "duration_seconds",
                mean_stddev_schema("Laufzeit in Sekunden."),
            ),
            ("total_tokens", mean_stddev_schema("Verbrauchte Token.")),
        ],
        &["pass_rate", "duration_seconds", "total_tokens"],
        description,
    )
}

fn evals_schema() -> JsonSchema {
    let mut kind = typed(
        JsonSchemaType::String,
        "Prüfart: grader (Vorgabe), contains, not_contains, regex, file_exists.",
    );
    kind.enum_values = Some(
        ["grader", "contains", "not_contains", "regex", "file_exists"]
            .into_iter()
            .map(serde_json::Value::from)
            .collect(),
    );
    let assertion = object(
        vec![
            (
                "text",
                typed(JsonSchemaType::String, "Menschenlesbare Prüfaussage."),
            ),
            ("kind", kind),
            (
                "value",
                typed(
                    JsonSchemaType::String,
                    "Vergleichswert/Muster/relativer Pfad für skriptbare Arten.",
                ),
            ),
        ],
        &["text"],
        "",
    );
    let case = object(
        vec![
            (
                "id",
                typed(JsonSchemaType::String, "Eindeutige ID ([a-z0-9-])."),
            ),
            (
                "prompt",
                typed(
                    JsonSchemaType::String,
                    "Realistischer Nutzer-Prompt, der den Skill auslösen soll.",
                ),
            ),
            (
                "expected_output",
                typed(
                    JsonSchemaType::String,
                    "Beschreibung des erwarteten Ergebnisses.",
                ),
            ),
            (
                "files",
                array_of(
                    typed(JsonSchemaType::String, "Relativer Pfad."),
                    "Eingabedateien des Falls.",
                ),
            ),
            (
                "assertions",
                array_of(assertion, "Prüfaussagen (dürfen anfangs fehlen)."),
            ),
        ],
        &["id", "prompt"],
        "",
    );
    array_of(
        case,
        "Optionaler Eval-Plan: 2–3 realistische Prompts mit Assertions.",
    )
}

fn benchmark_schema() -> JsonSchema {
    let mut baseline = typed(
        JsonSchemaType::String,
        "Baseline: without_skill oder previous_version.",
    );
    baseline.enum_values = Some(vec![
        serde_json::Value::from("without_skill"),
        serde_json::Value::from("previous_version"),
    ]);
    let texts = |description: &str| {
        array_of(
            typed(JsonSchemaType::String, "Assertion-Text."),
            description,
        )
    };
    object(
        vec![
            (
                "iteration",
                typed(JsonSchemaType::Integer, "Iteration (≥ 1)."),
            ),
            (
                "runs_per_configuration",
                typed(JsonSchemaType::Integer, "Läufe je Konfiguration (≥ 1)."),
            ),
            ("baseline", baseline),
            ("with_skill", stats_schema("Kennzahlen mit dem Skill.")),
            ("baseline_stats", stats_schema("Kennzahlen der Baseline.")),
            (
                "non_discriminating_assertions",
                texts("Assertions ohne Unterscheidungskraft."),
            ),
            (
                "flaky_assertions",
                texts("Assertions mit schwankendem Ergebnis."),
            ),
            (
                "notes",
                array_of(
                    typed(JsonSchemaType::String, "Beobachtung."),
                    "Analysten-Notizen.",
                ),
            ),
        ],
        &[
            "iteration",
            "runs_per_configuration",
            "baseline",
            "with_skill",
            "baseline_stats",
        ],
        "Optionale Benchmark-Zusammenfassung (mit Skill gegen Baseline).",
    )
}

fn candidate_properties() -> Vec<(&'static str, JsonSchema)> {
    vec![
        (
            "skill_toml",
            typed(
                JsonSchemaType::String,
                "Skill-Manifest (TOML): name, description, optional enabled, tools, mcps; \
                 instructions_file fehlt oder ist \"instructions.md\".",
            ),
        ),
        (
            "instructions",
            typed(JsonSchemaType::String, "Inhalt von instructions.md."),
        ),
        ("evals", evals_schema()),
        ("benchmark", benchmark_schema()),
    ]
}

fn function_spec(name: &str, description: &str, parameters: JsonSchema) -> ToolSpec {
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(name),
        description: description.to_owned(),
        parameters,
        strict: true,
    })
}

fn skills_validate_spec() -> ToolSpec {
    function_spec(
        "skills.validate",
        "Validiert einen Skill-Kandidaten (Manifest, instructions, optionaler Eval-Plan und \
         Benchmark). Schreibt nichts. Gibt {ok, name?, errors[], warnings[], eval_status, \
         benchmark_delta?} zurück.",
        object(candidate_properties(), &["skill_toml", "instructions"], ""),
    )
}

fn skills_propose_spec() -> ToolSpec {
    function_spec(
        "skills.propose",
        "Legt einen validierten Skill als Vorschlag unter <profil>/skills/.proposals/<id>/ ab \
         und berechnet das Werkzeug-/MCP-Delta gegen deine eigenen Rechte. Aktiviert nie: \
         weder das Live-Skill-Verzeichnis noch ein Agent werden verändert.",
        object(candidate_properties(), &["skill_toml", "instructions"], ""),
    )
}

fn skills_list_proposals_spec() -> ToolSpec {
    function_spec(
        "skills.list_proposals",
        "Listet alle Skill-Vorschläge mit proposal_id, name, status, review_level, \
         capability_delta, eval_status, created_at, expires_at und expired. Rein lesend.",
        object(Vec::new(), &[], ""),
    )
}

fn skills_commit_proposal_spec() -> ToolSpec {
    function_spec(
        "skills.commit_proposal",
        "Validiert einen offenen, nicht abgelaufenen Skill-Vorschlag erneut, berechnet das \
         Delta gegen die Rechte DIESES Aufrufers neu und schreibt ihn nach \
         <profil>/skills/<name>/. Bei review_level \"user_required\" ist user_confirmed = \
         true Pflicht. Weist den Skill keinem Agenten zu. Freigabepflichtig.",
        object(
            vec![
                (
                    "proposal_id",
                    typed(JsonSchemaType::String, "ID aus skills.list_proposals."),
                ),
                (
                    "user_confirmed",
                    typed(
                        JsonSchemaType::Boolean,
                        "Pflicht (true) bei review_level \"user_required\". Ersetzt die \
                         Freigabe-Kette des Harness nicht, sondern setzt sie voraus.",
                    ),
                ),
            ],
            &["proposal_id"],
            "",
        ),
    )
}

fn skills_reject_proposal_spec() -> ToolSpec {
    function_spec(
        "skills.reject_proposal",
        "Markiert einen offenen Skill-Vorschlag als 'rejected' mit Begründung. Schreibt \
         nichts an ein Ziel.",
        object(
            vec![
                (
                    "proposal_id",
                    typed(JsonSchemaType::String, "ID des abzulehnenden Vorschlags."),
                ),
                (
                    "reason",
                    typed(JsonSchemaType::String, "Begründung der Ablehnung."),
                ),
            ],
            &["proposal_id", "reason"],
            "",
        ),
    )
}

// ---------------------------------------------------------------------------
// Ausführende
// ---------------------------------------------------------------------------

/// Argumente von `skills.validate`/`skills.propose`.
#[derive(Debug, Deserialize)]
struct CandidateArgs {
    skill_toml: String,
    instructions: String,
    #[serde(default, deserialize_with = "null_as_default")]
    evals: Vec<SkillEvalCase>,
    #[serde(default)]
    benchmark: Option<SkillBenchmark>,
}

impl CandidateArgs {
    fn into_candidate(self) -> SkillCandidate {
        SkillCandidate {
            skill_toml: self.skill_toml,
            instructions: self.instructions,
            evals: self.evals,
            benchmark: self.benchmark,
        }
    }
}

/// Argumente von `skills.commit_proposal`.
#[derive(Debug, Deserialize)]
struct CommitArgs {
    proposal_id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    user_confirmed: bool,
}

/// Argumente von `skills.reject_proposal`.
#[derive(Debug, Deserialize)]
struct RejectArgs {
    proposal_id: String,
    reason: String,
}

/// JSON-Wert eines serialisierbaren Datums, `null` bei (unerreichbarem)
/// Serialisierungsfehler.
fn to_json<T: Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

/// Hinweis nach einem Commit: committet heißt nicht zugewiesen.
fn activation_hint(name: &str) -> String {
    format!(
        "Skill '{name}' liegt jetzt im Profil, ist aber keinem Agenten zugewiesen. Wirksam \
         wird er erst, wenn ein Agent ihn in agents/<agent>/agent.toml unter `skills` \
         führt (nächstes Config-Laden)."
    )
}

struct SkillsValidateExecutor;

impl SkillsValidateExecutor {
    fn run(call: &ToolCall) -> ToolOutput {
        let args: CandidateArgs = match parse_args("skills.validate", &call.arguments) {
            Ok(args) => args,
            Err(output) => return output,
        };
        let candidate = args.into_candidate();
        let validation = validate_skill_candidate(&candidate);
        let accepted = validation.accepted().is_some();
        ToolOutput::json(serde_json::json!({
            "ok": accepted,
            "name": validation.skill.as_ref().map(|skill| skill.name.clone()),
            "errors": validation.errors,
            "warnings": validation.warnings,
            "eval_count": candidate.evals.len(),
            "benchmark_delta": candidate.benchmark.as_ref().map(|benchmark| to_json(&benchmark.delta())),
        }))
    }
}

impl ToolExecutor for SkillsValidateExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move { Ok(Self::run(call)) })
    }
}

struct SkillsProposeExecutor {
    store: Option<SkillProposalStore>,
    ceiling: SkillAuthorCeiling,
}

impl SkillsProposeExecutor {
    fn run(&self, call: &ToolCall) -> ToolOutput {
        let args: CandidateArgs = match parse_args("skills.propose", &call.arguments) {
            Ok(args) => args,
            Err(output) => return output,
        };
        let Some(store) = &self.store else {
            return ToolOutput::error(
                "skills.propose: kein Profil-Skill-Verzeichnis konfiguriert".to_owned(),
            );
        };
        match store.propose(&args.into_candidate(), &self.ceiling) {
            Ok(meta) => ToolOutput::json(serde_json::json!({
                "ok": true,
                "proposed": true,
                "activated": false,
                "proposal_id": meta.proposal_id,
                "name": meta.name,
                "review_level": meta.review_level.as_str(),
                "requires_user_approval": meta.review_level == ReviewLevel::UserRequired,
                "capability_delta": to_json(&meta.capability_delta),
                "replaces_existing": meta.replaces_existing,
                "eval_status": meta.eval_status(),
                "warnings": meta.warnings,
                "expires_at": meta.expires_at,
            })),
            Err(SkillProposalError::Invalid(errors)) => ToolOutput::json(serde_json::json!({
                "ok": false,
                "proposed": false,
                "errors": errors,
            })),
            Err(error) => ToolOutput::json(serde_json::json!({
                "ok": false,
                "proposed": false,
                "errors": [error.to_string()],
            })),
        }
    }
}

impl ToolExecutor for SkillsProposeExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move { Ok(self.run(call)) })
    }
}

struct SkillsListProposalsExecutor {
    store: Option<SkillProposalStore>,
}

impl SkillsListProposalsExecutor {
    fn run(&self) -> ToolOutput {
        let Some(store) = &self.store else {
            return ToolOutput::json(serde_json::json!({ "ok": true, "proposals": [] }));
        };
        let listings = match store.list() {
            Ok(listings) => listings,
            Err(error) => return ToolOutput::error(format!("skills.list_proposals: {error}")),
        };
        let proposals: Vec<serde_json::Value> = listings
            .into_iter()
            .map(|listing| match listing {
                SkillProposalListing::Proposal { meta, expired } => serde_json::json!({
                    "proposal_id": meta.proposal_id,
                    "name": meta.name,
                    "description": meta.description,
                    "status": meta.status.as_str(),
                    "review_level": meta.review_level.as_str(),
                    "capability_delta": to_json(&meta.capability_delta),
                    "replaces_existing": meta.replaces_existing,
                    "eval_status": meta.eval_status(),
                    "created_at": meta.created_at,
                    "expires_at": meta.expires_at,
                    "expired": expired,
                }),
                SkillProposalListing::Broken { proposal_id, error } => serde_json::json!({
                    "proposal_id": proposal_id,
                    "error": error,
                }),
            })
            .collect();
        ToolOutput::json(serde_json::json!({ "ok": true, "proposals": proposals }))
    }
}

impl ToolExecutor for SkillsListProposalsExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        _call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move { Ok(self.run()) })
    }
}

struct SkillsCommitProposalExecutor {
    store: Option<SkillProposalStore>,
    ceiling: SkillAuthorCeiling,
}

impl SkillsCommitProposalExecutor {
    fn run(&self, call: &ToolCall) -> ToolOutput {
        let args: CommitArgs = match parse_args("skills.commit_proposal", &call.arguments) {
            Ok(args) => args,
            Err(output) => return output,
        };
        let Some(store) = &self.store else {
            return ToolOutput::error(
                "skills.commit_proposal: kein Profil-Skill-Verzeichnis konfiguriert".to_owned(),
            );
        };
        let authority = CommitAuthority::Ceiling {
            ceiling: &self.ceiling,
            user_confirmed: args.user_confirmed,
        };
        match store.commit(&args.proposal_id, authority) {
            Ok(outcome) => ToolOutput::json(serde_json::json!({
                "ok": true,
                "written": true,
                "activated": false,
                "proposal_id": outcome.meta.proposal_id,
                "name": outcome.meta.name,
                "path": outcome.skill_dir.display().to_string(),
                "review_level": outcome.meta.review_level.as_str(),
                "requires_user_approval": outcome.meta.review_level == ReviewLevel::UserRequired,
                "activation_hint": activation_hint(&outcome.meta.name),
                "errors": outcome.status_update_error.into_iter().collect::<Vec<_>>(),
            })),
            Err(SkillProposalError::RequiresUserConfirmation { delta }) => {
                ToolOutput::json(serde_json::json!({
                    "ok": false,
                    "written": false,
                    "review_level": ReviewLevel::UserRequired.as_str(),
                    "requires_user_approval": true,
                    "capability_delta": to_json(&delta),
                    "errors": ["review_level ist \"user_required\" — user_confirmed = true ist \
                                Pflicht (siehe Freigabe-Kette in der Moduldoku)"],
                }))
            }
            Err(SkillProposalError::Invalid(errors)) => ToolOutput::json(serde_json::json!({
                "ok": false,
                "written": false,
                "errors": errors,
            })),
            Err(error) => ToolOutput::json(serde_json::json!({
                "ok": false,
                "written": false,
                "errors": [error.to_string()],
            })),
        }
    }
}

impl ToolExecutor for SkillsCommitProposalExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move { Ok(self.run(call)) })
    }
}

struct SkillsRejectProposalExecutor {
    store: Option<SkillProposalStore>,
}

impl SkillsRejectProposalExecutor {
    fn run(&self, call: &ToolCall) -> ToolOutput {
        let args: RejectArgs = match parse_args("skills.reject_proposal", &call.arguments) {
            Ok(args) => args,
            Err(output) => return output,
        };
        let Some(store) = &self.store else {
            return ToolOutput::error(
                "skills.reject_proposal: kein Profil-Skill-Verzeichnis konfiguriert".to_owned(),
            );
        };
        match store.reject(&args.proposal_id, &args.reason) {
            Ok(meta) => ToolOutput::json(serde_json::json!({
                "ok": true,
                "proposal_id": meta.proposal_id,
                "status": meta.status.as_str(),
            })),
            Err(error) => ToolOutput::error(format!("skills.reject_proposal: {error}")),
        }
    }
}

impl ToolExecutor for SkillsRejectProposalExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move { Ok(self.run(call)) })
    }
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Die lesenden Werkzeuge (immer registriert).
pub const SKILL_PROPOSAL_READ_TOOLS: &[&str] = &["skills.validate", "skills.list_proposals"];

/// Das ablegende Werkzeug (nur mit Decke).
pub const SKILL_PROPOSAL_PROPOSE_TOOLS: &[&str] = &["skills.propose"];

/// Die entscheidenden Werkzeuge (nur Commit-Modus + Decke).
pub const SKILL_PROPOSAL_DECIDE_TOOLS: &[&str] =
    &["skills.commit_proposal", "skills.reject_proposal"];

/// Der Provider für die Skill-Vorschlagswerkzeuge.
///
/// # Beschreibung
/// - Ohne Decke (`ceiling = None`) fail-closed nur `skills.validate` und
///   `skills.list_proposals`.
/// - Mit Decke zusätzlich `skills.propose`.
/// - Im [`DefinitionWriteMode::Commit`] mit Decke zusätzlich
///   `skills.commit_proposal`/`skills.reject_proposal`.
///
/// # Nebenläufigkeit
/// `Send + Sync`; unveränderlich nach der Konstruktion.
#[derive(Debug, Clone)]
pub struct SkillProposalToolProvider {
    store: Option<SkillProposalStore>,
    mode: DefinitionWriteMode,
    ceiling: Option<SkillAuthorCeiling>,
}

impl SkillProposalToolProvider {
    /// Erstellt den Provider.
    ///
    /// # Arguments
    /// - `profile_skills_dir` (`Option<PathBuf>`): `<profil>/skills`; `None`,
    ///   wenn kein Profil bekannt ist (dann scheitern schreibende Aufrufe mit
    ///   einer Fehlermeldung, die Liste bleibt leer).
    /// - `mode` ([`DefinitionWriteMode`]): ob Commit/Reject registriert werden.
    /// - `ceiling` (`Option<SkillAuthorCeiling>`): Werkzeuge/MCPs des
    ///   Aufrufers; `None` ⇒ fail-closed nur lesend.
    #[must_use]
    pub fn new(
        profile_skills_dir: Option<PathBuf>,
        mode: DefinitionWriteMode,
        ceiling: Option<SkillAuthorCeiling>,
    ) -> Self {
        Self {
            store: profile_skills_dir.map(SkillProposalStore::new),
            mode,
            ceiling,
        }
    }

    fn decides(&self) -> bool {
        self.ceiling.is_some() && self.mode == DefinitionWriteMode::Commit
    }
}

impl ToolProvider for SkillProposalToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        let mut tools = vec![skills_validate_spec(), skills_list_proposals_spec()];
        if self.ceiling.is_some() {
            tools.push(skills_propose_spec());
        }
        if self.decides() {
            tools.push(skills_commit_proposal_spec());
            tools.push(skills_reject_proposal_spec());
        }
        tools
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        match name.as_str() {
            "skills.validate" => Some(Arc::new(SkillsValidateExecutor)),
            "skills.list_proposals" => Some(Arc::new(SkillsListProposalsExecutor {
                store: self.store.clone(),
            })),
            "skills.propose" => self.ceiling.clone().map(|ceiling| {
                Arc::new(SkillsProposeExecutor {
                    store: self.store.clone(),
                    ceiling,
                }) as Arc<dyn ToolExecutor>
            }),
            "skills.commit_proposal" if self.decides() => self.ceiling.clone().map(|ceiling| {
                Arc::new(SkillsCommitProposalExecutor {
                    store: self.store.clone(),
                    ceiling,
                }) as Arc<dyn ToolExecutor>
            }),
            "skills.reject_proposal" if self.decides() => {
                Some(Arc::new(SkillsRejectProposalExecutor {
                    store: self.store.clone(),
                }))
            }
            _ => None,
        }
    }

    /// Nur die lesenden Werkzeuge sind kommutativ.
    fn parallel_safe(&self, name: &ToolName) -> bool {
        SKILL_PROPOSAL_READ_TOOLS.contains(&name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    const GOOD_TOML: &str = "name = \"release-notes\"\ndescription = \"Schreibt Release-Notes \
                             aus Commit-Historie, wenn nach einem Changelog gefragt wird\"\n\
                             tools = [\"fs.read\", \"shell.exec\"]\n";

    fn ceiling(tools: &[&str], mcps: &[&str]) -> SkillAuthorCeiling {
        SkillAuthorCeiling {
            role: AgentRoleId::UserInterface,
            tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
            mcps: mcps.iter().map(|mcp| (*mcp).to_owned()).collect(),
        }
    }

    fn eval_case(id: &str) -> SkillEvalCase {
        SkillEvalCase {
            id: id.to_owned(),
            prompt: format!("Schreib mir Release-Notes für {id}"),
            expected_output: None,
            files: Vec::new(),
            assertions: vec![SkillEvalAssertion {
                text: "nennt jede Änderung".to_owned(),
                kind: SkillAssertionKind::Grader,
                value: None,
            }],
        }
    }

    fn stats(pass_rate: f64) -> BenchmarkStats {
        BenchmarkStats {
            pass_rate: MeanStddev {
                mean: pass_rate,
                stddev: 0.1,
            },
            duration_seconds: MeanStddev {
                mean: 30.0,
                stddev: 2.0,
            },
            total_tokens: MeanStddev {
                mean: 1_000.0,
                stddev: 50.0,
            },
        }
    }

    fn benchmark() -> SkillBenchmark {
        SkillBenchmark {
            iteration: 1,
            runs_per_configuration: 3,
            baseline: BenchmarkBaseline::WithoutSkill,
            with_skill: stats(0.9),
            baseline_stats: stats(0.4),
            non_discriminating_assertions: Vec::new(),
            flaky_assertions: vec!["nennt jede Änderung".to_owned()],
            notes: Vec::new(),
        }
    }

    fn candidate() -> SkillCandidate {
        SkillCandidate {
            skill_toml: GOOD_TOML.to_owned(),
            instructions: "# Release-Notes\nFasse Commits nach Bereichen zusammen.\n".to_owned(),
            evals: vec![eval_case("basic"), eval_case("breaking")],
            benchmark: Some(benchmark()),
        }
    }

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: Default::default(),
            name: ToolName::new(name),
            arguments,
        }
    }

    fn json_of(output: ToolOutput) -> TestResult<serde_json::Value> {
        match output {
            ToolOutput::Json { content } => Ok(content),
            other => Err(TestError::Unexpected(format!(
                "expected JSON, got {other:?}"
            ))),
        }
    }

    fn only_proposal(store: &SkillProposalStore) -> TestResult<SkillProposalMeta> {
        let mut listings = store.list().map_err(ctx("list"))?;
        match (listings.pop(), listings.is_empty()) {
            (Some(SkillProposalListing::Proposal { meta, .. }), true) => Ok(*meta),
            other => Err(TestError::Unexpected(format!(
                "expected exactly one proposal, got {other:?}"
            ))),
        }
    }

    #[test]
    fn validate_accepts_a_complete_candidate() {
        let validation = validate_skill_candidate(&candidate());
        assert!(validation.errors.is_empty(), "{:?}", validation.errors);
        assert_eq!(
            validation.accepted().map(|skill| skill.name.as_str()),
            Some("release-notes")
        );
    }

    #[test]
    fn validate_rejects_bad_manifests() {
        for (toml, needle) in [
            (
                "name = \"Bad Name\"\ndescription = \"x\"\n",
                "name 'Bad Name'",
            ),
            ("name = \"ok\"\n", "description"),
            (
                "name = \"ok\"\ndescription = \"d\"\ninstructions_file = \"../x.md\"\n",
                "instructions_file",
            ),
            (
                "name = \"ok\"\ndescription = \"d\"\ntools = [\"fs.read\", \"fs.read\"]\n",
                "doppelt",
            ),
            (
                "name = \"ok\"\ndescription = \"d\"\nsurprise = 1\n",
                "skill_toml",
            ),
        ] {
            let validation = validate_skill_candidate(&SkillCandidate {
                skill_toml: toml.to_owned(),
                instructions: "body".to_owned(),
                ..SkillCandidate::default()
            });
            assert!(
                validation.errors.iter().any(|error| error.contains(needle)),
                "{toml}: expected an error containing {needle}, got {:?}",
                validation.errors
            );
            assert!(validation.accepted().is_none());
        }
    }

    #[test]
    fn validate_rejects_secrets_but_not_ordinary_words() {
        let mut secret = candidate();
        secret.instructions = "Nutze sk-abcdefghijklmnopqrstuvwx als Schlüssel".to_owned();
        assert!(
            validate_skill_candidate(&secret)
                .errors
                .iter()
                .any(|error| error.contains("Geheimnis"))
        );
        let mut harmless = candidate();
        harmless.instructions = "Eine risk-based Prüfung, task-orientiert.".to_owned();
        assert!(validate_skill_candidate(&harmless).errors.is_empty());
    }

    #[test]
    fn eval_plan_validation_reports_errors_and_warnings() {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        let mut duplicate = eval_case("same");
        duplicate.assertions.push(SkillEvalAssertion {
            text: "enthält Überschrift".to_owned(),
            kind: SkillAssertionKind::Contains,
            value: None,
        });
        duplicate.files.push("../escape.txt".to_owned());
        let mut empty = eval_case("same");
        empty.prompt = "  ".to_owned();
        empty.assertions.clear();
        let mut bad_benchmark = benchmark();
        bad_benchmark.with_skill.pass_rate.mean = 1.5;
        bad_benchmark.runs_per_configuration = 0;
        bad_benchmark.flaky_assertions = vec!["gibt es nicht".to_owned()];
        validate_eval_plan(
            &[duplicate, empty],
            Some(&bad_benchmark),
            &mut errors,
            &mut warnings,
        );
        for needle in [
            "doppelte id",
            "prompt darf nicht leer",
            "braucht einen value",
            "../escape.txt",
            "0.0..=1.0",
            "runs_per_configuration",
            "gibt es nicht",
        ] {
            assert!(
                errors.iter().any(|error| error.contains(needle)),
                "missing {needle} in {errors:?}"
            );
        }
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("ohne Assertions"))
        );
    }

    #[test]
    fn benchmark_without_evals_is_rejected_and_delta_is_computed() {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        validate_eval_plan(&[], Some(&benchmark()), &mut errors, &mut warnings);
        assert!(errors.iter().any(|error| error.contains("ohne evals")));
        let delta = benchmark().delta();
        assert!((delta.pass_rate - 0.5).abs() < 1e-9);
        assert!(delta.duration_seconds.abs() < 1e-9);
    }

    #[test]
    fn capability_delta_lists_only_missing_capabilities() -> TestResult {
        let skill: SkillToml = toml::from_str(
            "name = \"x\"\ndescription = \"d\"\ntools = [\"fs.read\", \"shell.exec\"]\n\
             mcps = [\"docs\"]\n",
        )
        .map_err(ctx("parse skill"))?;
        let delta = SkillCapabilityDelta::compute(&skill, &ceiling(&["fs.read"], &[]));
        assert_eq!(delta.added_tools, vec!["shell.exec".to_owned()]);
        assert_eq!(delta.added_mcps, vec!["docs".to_owned()]);
        assert_eq!(ReviewLevel::for_delta(&delta), ReviewLevel::UserRequired);
        let none =
            SkillCapabilityDelta::compute(&skill, &ceiling(&["fs.read", "shell.exec"], &["docs"]));
        assert!(none.is_empty());
        assert_eq!(ReviewLevel::for_delta(&none), ReviewLevel::Uia);
        Ok(())
    }

    #[test]
    fn propose_writes_only_the_proposal_directory() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let skills = temp.path().join("skills");
        let store = SkillProposalStore::new(skills.clone());
        let meta = store
            .propose(&candidate(), &ceiling(&["fs.read"], &[]))
            .map_err(ctx("propose"))?;
        assert_eq!(meta.status, SkillProposalStatus::PendingReview);
        assert_eq!(meta.review_level, ReviewLevel::UserRequired);
        assert_eq!(
            meta.capability_delta.added_tools,
            vec!["shell.exec".to_owned()]
        );
        assert_eq!(meta.eval_status(), "benchmarked");
        assert_eq!(meta.author_role.as_deref(), Some("user-interface"));
        assert!(!meta.replaces_existing);
        let dir = skills
            .join(SKILL_PROPOSALS_DIR_NAME)
            .join(&meta.proposal_id);
        for file in [PROPOSAL_FILE, SKILL_MANIFEST, INSTRUCTIONS_FILE] {
            assert!(dir.join(file).is_file(), "{file} fehlt");
        }
        assert!(
            !skills.join("release-notes").exists(),
            "propose darf nie aktivieren"
        );
        let loaded = store.load(&meta.proposal_id).map_err(ctx("load"))?;
        assert_eq!(loaded.meta.proposal_id, meta.proposal_id);
        assert_eq!(loaded.meta.evals, meta.evals);
        assert_eq!(loaded.skill_toml, GOOD_TOML);
        assert!(!loaded.expired);
        Ok(())
    }

    #[test]
    fn propose_rejects_invalid_candidate_without_writing() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = SkillProposalStore::new(temp.path().join("skills"));
        let mut invalid = candidate();
        invalid.instructions.clear();
        assert!(matches!(
            store.propose(&invalid, &ceiling(&[], &[])),
            Err(SkillProposalError::Invalid(_))
        ));
        assert!(!store.proposals_root().exists());
        Ok(())
    }

    #[test]
    fn commit_requires_user_confirmation_for_a_non_empty_delta() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let skills = temp.path().join("skills");
        let store = SkillProposalStore::new(skills.clone());
        let author = ceiling(&["fs.read"], &[]);
        let meta = store
            .propose(&candidate(), &author)
            .map_err(ctx("propose"))?;
        let refused = store.commit(
            &meta.proposal_id,
            CommitAuthority::Ceiling {
                ceiling: &author,
                user_confirmed: false,
            },
        );
        assert!(matches!(
            refused,
            Err(SkillProposalError::RequiresUserConfirmation { .. })
        ));
        assert!(!skills.join("release-notes").exists());

        let outcome = store
            .commit(
                &meta.proposal_id,
                CommitAuthority::Ceiling {
                    ceiling: &author,
                    user_confirmed: true,
                },
            )
            .map_err(ctx("commit"))?;
        assert_eq!(outcome.skill_dir, skills.join("release-notes"));
        assert!(outcome.status_update_error.is_none());
        assert_eq!(outcome.meta.status, SkillProposalStatus::Committed);
        assert_eq!(outcome.meta.committed_by.as_deref(), Some("uia"));
        let written = std::fs::read_to_string(outcome.skill_dir.join(SKILL_MANIFEST))
            .map_err(ctx("read committed manifest"))?;
        assert_eq!(written, GOOD_TOML);
        assert!(outcome.skill_dir.join(INSTRUCTIONS_FILE).is_file());

        assert!(matches!(
            store.commit(&meta.proposal_id, CommitAuthority::Operator),
            Err(SkillProposalError::Conflict(_))
        ));
        Ok(())
    }

    #[test]
    fn operator_commit_and_update_diff() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = SkillProposalStore::new(temp.path().join("skills"));
        let author = ceiling(&["fs.read", "shell.exec"], &[]);
        let first = store
            .propose(&candidate(), &author)
            .map_err(ctx("propose"))?;
        assert_eq!(first.review_level, ReviewLevel::Uia);
        store
            .commit(&first.proposal_id, CommitAuthority::Operator)
            .map_err(ctx("operator commit"))?;

        let mut update = candidate();
        update.instructions.push_str("Neue Zeile.\n");
        let second = store
            .propose(&update, &author)
            .map_err(ctx("propose update"))?;
        assert!(second.replaces_existing);
        assert_eq!(second.manifest_diff, "no changes");
        assert!(second.instructions_diff.contains("+Neue Zeile."));
        Ok(())
    }

    #[test]
    fn reject_marks_rejected_and_blocks_commit() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = SkillProposalStore::new(temp.path().join("skills"));
        let meta = store
            .propose(&candidate(), &ceiling(&[], &[]))
            .map_err(ctx("propose"))?;
        assert!(matches!(
            store.reject(&meta.proposal_id, "  "),
            Err(SkillProposalError::Conflict(_))
        ));
        let rejected = store
            .reject(&meta.proposal_id, "zu allgemein")
            .map_err(ctx("reject"))?;
        assert_eq!(rejected.status, SkillProposalStatus::Rejected);
        assert_eq!(
            only_proposal(&store)?.reason.as_deref(),
            Some("zu allgemein")
        );
        assert!(matches!(
            store.commit(&meta.proposal_id, CommitAuthority::Operator),
            Err(SkillProposalError::Conflict(_))
        ));
        Ok(())
    }

    #[test]
    fn expired_or_unknown_proposals_cannot_be_committed() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = SkillProposalStore::new(temp.path().join("skills"));
        let mut meta = store
            .propose(&candidate(), &ceiling(&[], &[]))
            .map_err(ctx("propose"))?;
        meta.expires_at = "2000-01-01T00:00:00Z".to_owned();
        store.write_meta(&meta).map_err(ctx("backdate"))?;
        assert!(matches!(
            store.commit(&meta.proposal_id, CommitAuthority::Operator),
            Err(SkillProposalError::Expired(_))
        ));
        assert!(matches!(
            store.commit("../escape", CommitAuthority::Operator),
            Err(SkillProposalError::InvalidId(_))
        ));
        assert!(matches!(
            store.commit("does-not-exist", CommitAuthority::Operator),
            Err(SkillProposalError::NotFound(_))
        ));
        Ok(())
    }

    #[test]
    fn propose_refuses_a_second_directory_for_the_same_skill_name() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let skills = temp.path().join("skills");
        let other = skills.join("renamed-dir");
        std::fs::create_dir_all(&other).map_err(ctx("mkdir"))?;
        std::fs::write(other.join(SKILL_MANIFEST), GOOD_TOML).map_err(ctx("write"))?;
        let store = SkillProposalStore::new(skills);
        assert!(matches!(
            store.propose(&candidate(), &ceiling(&[], &[])),
            Err(SkillProposalError::Conflict(_))
        ));
        Ok(())
    }

    #[test]
    fn provider_gates_tools_by_ceiling_and_mode() {
        let names = |provider: &SkillProposalToolProvider| -> Vec<String> {
            provider
                .tools()
                .iter()
                .map(|spec| spec.name().to_string())
                .collect()
        };
        let read_only = SkillProposalToolProvider::new(None, DefinitionWriteMode::Commit, None);
        assert_eq!(
            names(&read_only),
            ["skills.validate", "skills.list_proposals"]
        );
        assert!(
            read_only
                .executor(&ToolName::new("skills.commit_proposal"))
                .is_none()
        );
        let proposer = SkillProposalToolProvider::new(
            None,
            DefinitionWriteMode::ProposalOnly,
            Some(ceiling(&[], &[])),
        );
        assert_eq!(
            names(&proposer),
            ["skills.validate", "skills.list_proposals", "skills.propose"]
        );
        assert!(
            proposer
                .executor(&ToolName::new("skills.reject_proposal"))
                .is_none()
        );
        let decider = SkillProposalToolProvider::new(
            None,
            DefinitionWriteMode::Commit,
            Some(ceiling(&[], &[])),
        );
        assert_eq!(names(&decider).len(), 5);
        assert!(decider.parallel_safe(&ToolName::new("skills.validate")));
        assert!(!decider.parallel_safe(&ToolName::new("skills.commit_proposal")));
    }

    #[test]
    fn tool_round_trip_accepts_strict_mode_nulls() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let author = ceiling(&["fs.read", "shell.exec"], &[]);
        let propose = SkillsProposeExecutor {
            store: Some(SkillProposalStore::new(temp.path().join("skills"))),
            ceiling: author.clone(),
        };
        let proposed = json_of(propose.run(&call(
            "skills.propose",
            serde_json::json!({
                "skill_toml": GOOD_TOML,
                "instructions": "Fasse Commits zusammen.",
                "evals": [{
                    "id": "basic",
                    "prompt": "Schreib Release-Notes",
                    "expected_output": null,
                    "files": null,
                    "assertions": [{ "text": "hat Überschrift", "kind": null, "value": null }],
                }],
                "benchmark": null,
            }),
        )))?;
        assert_eq!(proposed["ok"], serde_json::json!(true), "{proposed}");
        assert_eq!(proposed["activated"], serde_json::json!(false));
        assert_eq!(proposed["eval_status"], serde_json::json!("defined"));
        let proposal_id = proposed["proposal_id"]
            .as_str()
            .ok_or(TestError::Missing("proposal_id"))?
            .to_owned();

        let validate = json_of(SkillsValidateExecutor::run(&call(
            "skills.validate",
            serde_json::json!({ "skill_toml": "name = \"x\"", "instructions": "" }),
        )))?;
        assert_eq!(validate["ok"], serde_json::json!(false));

        let commit = SkillsCommitProposalExecutor {
            store: Some(SkillProposalStore::new(temp.path().join("skills"))),
            ceiling: author,
        };
        let committed = json_of(commit.run(&call(
            "skills.commit_proposal",
            serde_json::json!({ "proposal_id": proposal_id, "user_confirmed": null }),
        )))?;
        assert_eq!(committed["written"], serde_json::json!(true), "{committed}");
        assert_eq!(committed["activated"], serde_json::json!(false));
        assert!(
            temp.path()
                .join("skills/release-notes/skill.toml")
                .is_file()
        );
        Ok(())
    }
}
