//! Ablage der Recherche-Ergebnisse als lesbare Markdown-Artefakte.
//!
//! # Verantwortungsbereich
//! Der [`FindingStore`] besitzt das Verzeichnis `<HARW_HOME>/plans` und legt
//! dort pro Plan und Frage genau eine Datei ab:
//! `<root>/<plan_id>/research/<question_id>.md`. Jede Datei besteht aus
//!
//! 1. YAML-Frontmatter (`question_id`, `confidence`, `produced_by`,
//!    `produced_at`, `evidence_count`) — maschinell auswertbar, ohne die Datei
//!    ganz zu parsen;
//! 2. einem Markdown-Körper für Menschen (Conclusion, Evidenzliste mit
//!    Locators, verifizierte Versionen, Constraints, Kompatibilitätsnotizen,
//!    offene Fragen);
//! 3. einem kanonischen JSON-Block zwischen zwei Sentinel-Kommentaren, aus dem
//!    [`FindingStore::read`] das [`ResearchFinding`] verlustfrei rekonstruiert.
//!
//! Punkt 3 ist der Grund, warum `read` kein Prosa-Parsing betreibt: die
//! Markdown-Darstellung ist für Menschen da und darf sich ändern, die
//! Rundreise hängt an der JSON-Kopie.
//!
//! # Untrusted Input
//! `plan_id` und `question_id` stammen aus Modell-Ausgaben. Beide werden über
//! [`sanitize_component`] auf ein einzelnes, harmloses Pfadsegment reduziert
//! (kein `/`, kein `..`, kein Nullbyte, keine Leerzeichen, Länge gedeckelt).
//! Ein Pfad kann den Store-Root damit nicht verlassen. Verändert die
//! Sanitisierung die Eingabe (Zeichen ersetzt, Dotfile-Präfix, Kürzung),
//! hängt sie einen BLAKE3-Hash-Suffix an, damit verschiedene Roh-IDs (z. B.
//! `a/b` und `a:b`) nicht auf dasselbe Artefakt fallen und sich gegenseitig
//! überschreiben.
//!
//! # Exportierte Typen
//! [`FindingStore`], [`finding_locator`], [`offset_from_timestamp`].
//!
//! # Concurrency
//! [`FindingStore`] ist ein unveränderlicher Werttyp (`Send + Sync`); alle
//! Methoden nehmen `&self`. Schreibvorgänge laufen über
//! `harw_knowledge::store::write_atomic` (Temp-Datei + `rename`), sind also
//! gegenüber gleichzeitigen Lesern atomar.
//!
//! # Fehler
//! [`PlanBridgeError::Io`], [`PlanBridgeError::Research`],
//! [`PlanBridgeError::Json`].
//!
//! # Examples
//! ```rust,no_run
//! use harw_plan_bridge::FindingStore;
//!
//! let store = FindingStore::new("/tmp/harw-home/plans");
//! match store.list("p-1") {
//!     Ok(question_ids) => println!("{} Findings abgelegt", question_ids.len()),
//!     Err(error) => eprintln!("Findings nicht lesbar: {error}"),
//! }
//! ```

use std::path::{Path, PathBuf};

use harw_plan::{EvidenceKind, EvidenceRef};
use harw_research::{Confidence, ResearchError, ResearchFinding, parse_finding, validate_finding};
use time::OffsetDateTime;

use crate::error::{PlanBridgeError, map_knowledge_error};

/// Obergrenze für ein einzelnes Pfadsegment nach der Sanitisierung
/// (schließt ein eventuelles Hash-Suffix, siehe [`hash_suffix`], mit ein).
const MAX_COMPONENT_LEN: usize = 128;

/// Ersatzname, wenn eine ID nach dem Trimmen leer ist.
const BLANK_COMPONENT: &str = "_";

/// Länge des Hash-Suffixes `-<16 Hex-Zeichen>`, das [`sanitize_component`]
/// bei verlustbehafteter Sanitisierung anhängt.
const HASH_SUFFIX_LEN: usize = 17;

/// Unterverzeichnis je Plan, in dem die Findings liegen.
const RESEARCH_DIR: &str = "research";

/// Beginn des kanonischen JSON-Blocks.
const JSON_BEGIN: &str = "<!-- harw-plan-bridge:finding-json:begin -->";

/// Ende des kanonischen JSON-Blocks.
const JSON_END: &str = "<!-- harw-plan-bridge:finding-json:end -->";

/// Reduziert eine Modell-gelieferte ID auf ein einzelnes, harmloses Pfadsegment.
///
/// # Description
/// Drei Regeln, damit das Ergebnis prüfbar bleibt:
///
/// 1. Jedes Zeichen außerhalb von `[A-Za-z0-9._-]` — insbesondere `/`, `\`,
///    `:` und Nullbytes — wird durch `-` ersetzt. Damit kann das Ergebnis
///    keinen Verzeichnistrenner mehr enthalten und ist immer *ein* Segment.
/// 2. Beginnt das Ergebnis mit `.`, wird ein `_` vorangestellt. Damit sind
///    weder `.` noch `..` (Traversal) noch ein verstecktes Dotfile
///    darstellbar; ein `..` *innerhalb* eines längeren Namens ist harmlos,
///    weil nur ein vollständiges `..`-Segment traversiert.
/// 3. Regel 1 und 2 sind für sich verlustbehaftet: `a/b`, `a:b` und `a b`
///    würden ohne Gegenmaßnahme alle zu `a-b`, ebenso würde die gekappte
///    Version einer langen ID mit der gekappten Version einer anderen langen
///    ID zusammenfallen, sobald beide in den ersten [`MAX_COMPONENT_LEN`]
///    Zeichen übereinstimmen. Verändert eine der ersten beiden Regeln die
///    Eingabe tatsächlich (Zeichen ersetzt, Dotfile-Präfix eingefügt) oder
///    würde die Eingabe ungekappt die Länge überschreiten, wird das Ergebnis
///    auf Platz für ein Suffix gekürzt und um `-<16 Hex-Zeichen aus
///    BLAKE3(trimmed)>` ([`hash_suffix`]) ergänzt. Nicht verlustbehaftete
///    Eingaben (bereits ein gültiges, kurzes Segment) bleiben unverändert und
///    bekommen kein Suffix — sie sind schon injektiv.
///
/// Damit ist die gesamte Funktion injektiv genug für den Anwendungsfall:
/// zwei verschiedene Eingaben, die dieselbe Zeichen-Ersetzung und Kürzung
/// durchlaufen, unterscheiden sich im BLAKE3-Digest (praktisch kollisionsfrei)
/// und damit im Suffix. Die Gesamtlänge bleibt in jedem Fall auf
/// [`MAX_COMPONENT_LEN`] gedeckelt — auch mit Suffix, das Präfix wird also
/// vor dem Kürzen und nicht danach angehängt. Die Funktion ist total: eine
/// leere oder nur aus Leerzeichen bestehende Eingabe ergibt
/// [`BLANK_COMPONENT`].
///
/// # Arguments
/// - `raw` (`&str`): die ungeprüfte ID aus einer Modell-Ausgabe.
///
/// # Returns
/// Ein nicht-leeres Pfadsegment ohne Separatoren, das weder `.` noch `..` ist
/// und höchstens [`MAX_COMPONENT_LEN`] Zeichen lang ist.
fn sanitize_component(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return BLANK_COMPONENT.to_owned();
    }

    let mapped: String = trimmed
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '-'
            }
        })
        .collect();

    let prefixed = if mapped.starts_with('.') {
        format!("_{mapped}")
    } else {
        mapped
    };

    // Verlustbehaftet, wenn Zeichen ersetzt oder ein Dotfile-Präfix eingefügt
    // wurden, oder wenn die Länge ungekappt über der Obergrenze liegt.
    let is_lossy = prefixed != trimmed || prefixed.chars().count() > MAX_COMPONENT_LEN;

    if !is_lossy {
        // `prefixed == trimmed` und innerhalb der Obergrenze: schon ein
        // gültiges, eindeutiges Segment, kein Suffix nötig.
        return prefixed;
    }

    let suffix = hash_suffix(trimmed);
    let budget = MAX_COMPONENT_LEN.saturating_sub(suffix.len());
    let mut out: String = prefixed.chars().take(budget).collect();
    out.push_str(&suffix);

    // Invariante: `budget >= 0` und `suffix` nicht leer, also enthält `out`
    // mindestens `suffix`. Der Zweig ist reine Absicherung gegen künftige
    // Änderungen an `HASH_SUFFIX_LEN`/`MAX_COMPONENT_LEN`.
    if out.is_empty() {
        return BLANK_COMPONENT.to_owned();
    }
    out
}

/// Bildet das Hash-Suffix `-<16 Hex-Zeichen>` für eine verlustbehaftet
/// sanitisierte Komponente.
///
/// # Description
/// Nutzt [`harw_types::ContentDigest`] (BLAKE3) über die *getrimmte*
/// Rohkomponente — nicht über das schon zeichen-ersetzte Ergebnis —, damit
/// zwei Eingaben, die auf dieselbe Ersetzung abbilden (`a/b`, `a:b`, `a b`),
/// unterschiedliche Suffixe erhalten. Die ersten 8 der 32 Digest-Bytes
/// (2^64 Möglichkeiten) reichen aus, um Kollisionen im Alltag praktisch
/// auszuschließen; alle 32 Bytes wären reiner Platzverbrauch im Dateinamen.
///
/// # Arguments
/// - `trimmed` (`&str`): die getrimmte (aber noch nicht zeichen-ersetzte)
///   Roheingabe.
///
/// # Returns
/// Einen 17 Zeichen langen String der Form `-0123456789abcdef`
/// ([`HASH_SUFFIX_LEN`] Zeichen).
fn hash_suffix(trimmed: &str) -> String {
    let digest = harw_types::ContentDigest::of(trimmed.as_bytes());
    let bytes = digest.as_bytes();
    let mut head = [0_u8; 8];
    head.copy_from_slice(&bytes[..8]);
    let suffix = format!("-{:016x}", u64::from_be_bytes(head));
    debug_assert_eq!(suffix.len(), HASH_SUFFIX_LEN);
    suffix
}

/// Baut den relativen Lokator eines Finding-Artefakts.
///
/// # Description
/// Der Lokator ist genau der Pfad relativ zum Store-Root, unter dem
/// [`FindingStore::write`] die Datei ablegt. Er ist rein berechenbar (keine
/// I/O), damit der [`crate::controller::PlanController`] denselben Lokator
/// bilden kann, ohne den Store zu kennen.
///
/// # Arguments
/// - `plan_id` (`&str`): Plan-Bezeichner (wird sanitisiert).
/// - `question_id` (`&str`): Frage-Bezeichner (wird sanitisiert).
///
/// # Returns
/// `"<plan_id>/research/<question_id>.md"` mit sanitisierten Segmenten. War
/// die Sanitisierung eines Segments verlustbehaftet, trägt das jeweilige
/// Segment zusätzlich einen Hash-Suffix (siehe [`sanitize_component`]); der
/// genaue Wert ist nicht Teil der öffentlichen Schnittstelle, nur seine Form
/// (`-` gefolgt von 16 Hex-Zeichen, direkt vor dem nächsten Trenner bzw. vor
/// der `.md`-Endung).
///
/// # Examples
/// ```rust
/// use harw_plan_bridge::finding_locator;
/// let locator = finding_locator("p-1", "../../etc/passwd");
/// assert!(locator.starts_with("p-1/research/_..-..-etc-passwd-"));
/// assert!(locator.ends_with(".md"));
/// ```
#[must_use]
pub fn finding_locator(plan_id: &str, question_id: &str) -> String {
    format!(
        "{}/{RESEARCH_DIR}/{}.md",
        sanitize_component(plan_id),
        sanitize_component(question_id)
    )
}

/// Konvertiert einen `jiff::Timestamp` in ein `time::OffsetDateTime` (UTC).
///
/// # Description
/// `harw-research` datiert Findings mit `jiff`, `harw-plan` datiert Evidenz mit
/// `time`. Dies ist die einzige Konvertierungsstelle im Crate. Ein Zeitpunkt
/// außerhalb des von `time` darstellbaren Bereichs fällt auf
/// `OffsetDateTime::UNIX_EPOCH` zurück, statt zu panicken — ein
/// unrepräsentierbarer Zeitstempel darf keinen Prozess beenden.
///
/// # Arguments
/// - `timestamp` (`jiff::Timestamp`): der Quellzeitpunkt.
///
/// # Returns
/// Das äquivalente `OffsetDateTime` in UTC.
#[must_use]
pub fn offset_from_timestamp(timestamp: jiff::Timestamp) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(timestamp.as_nanosecond())
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

/// Serialisiert eine [`Confidence`] als stabilen Frontmatter-Wert.
fn confidence_label(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::Low => "low",
        Confidence::Medium => "medium",
        Confidence::High => "high",
        Confidence::Verified => "verified",
    }
}

/// Maskiert einen Wert für die Verwendung in einfach gequoteten YAML-Skalaren.
fn yaml_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Dateiablage für Recherche-Ergebnisse unterhalb von `<HARW_HOME>/plans`.
///
/// # Description
/// Ein reiner Pfadhalter — der Store hält keinen Zustand und keine offenen
/// Handles. Alle Methoden leiten den Zielpfad deterministisch aus `plan_id`
/// und `question_id` ab (siehe [`finding_locator`]).
///
/// # Concurrency
/// `Send + Sync`; gleichzeitige Schreiber auf verschiedene Fragen stören sich
/// nicht, gleichzeitige Schreiber auf dieselbe Frage gewinnen atomar
/// (letzter `rename` gewinnt, nie eine halb geschriebene Datei).
#[derive(Debug, Clone)]
pub struct FindingStore {
    /// Wurzelverzeichnis, üblicherweise `<HARW_HOME>/plans`.
    root: PathBuf,
}

impl FindingStore {
    /// Erzeugt einen Store über dem angegebenen Plan-Verzeichnis.
    ///
    /// # Arguments
    /// - `plans_dir` (`impl Into<PathBuf>`): Wurzel, üblicherweise
    ///   `<HARW_HOME>/plans`. Das Verzeichnis muss nicht existieren; es wird
    ///   beim ersten Schreiben angelegt.
    ///
    /// # Returns
    /// Einen einsatzbereiten [`FindingStore`].
    ///
    /// # Examples
    /// ```rust
    /// use harw_plan_bridge::FindingStore;
    /// let store = FindingStore::new("/tmp/plans");
    /// assert_eq!(store.root().to_string_lossy(), "/tmp/plans");
    /// ```
    pub fn new(plans_dir: impl Into<PathBuf>) -> Self {
        Self {
            root: plans_dir.into(),
        }
    }

    /// Erzeugt einen Store über `<home>/plans`.
    ///
    /// # Description
    /// Bequemer Einstieg für Composition-Roots, die bereits ein
    /// HARW-Home-Verzeichnis aufgelöst haben. Nutzt
    /// `harw_home::paths::plans_dir`, damit die Layout-Entscheidung an genau
    /// einer Stelle lebt.
    ///
    /// # Arguments
    /// - `home` (`&Path`): das HARW-Home-Verzeichnis.
    ///
    /// # Returns
    /// Einen [`FindingStore`] über `<home>/plans`.
    #[must_use]
    pub fn from_home(home: &Path) -> Self {
        Self::new(harw_home::paths::plans_dir(home))
    }

    /// Gibt das Wurzelverzeichnis des Stores zurück.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absoluter Pfad des Artefakts einer Frage.
    ///
    /// # Arguments
    /// - `plan_id` (`&str`): Plan-Bezeichner (wird sanitisiert).
    /// - `question_id` (`&str`): Frage-Bezeichner (wird sanitisiert).
    ///
    /// # Returns
    /// `<root>/<plan_id>/research/<question_id>.md`.
    #[must_use]
    pub fn path_for(&self, plan_id: &str, question_id: &str) -> PathBuf {
        self.root
            .join(sanitize_component(plan_id))
            .join(RESEARCH_DIR)
            .join(format!("{}.md", sanitize_component(question_id)))
    }

    /// Schreibt ein Finding als Markdown-Artefakt (atomar).
    ///
    /// # Description
    /// Validiert das Finding zunächst über
    /// [`harw_research::validate_finding`] — ein Artefakt, das den
    /// Recherche-Vertrag verletzt, wird gar nicht erst abgelegt. Danach wird
    /// das Dokument (Frontmatter + Prosa + kanonisches JSON) gerendert und
    /// über `harw_knowledge::store::write_atomic` per Temp-Datei und `rename`
    /// eingesetzt.
    ///
    /// # Arguments
    /// - `plan_id` (`&str`): Plan, zu dem das Finding gehört (wird sanitisiert).
    /// - `finding` (`&ResearchFinding`): das abzulegende Ergebnis.
    ///
    /// # Returns
    /// Den absoluten Pfad der geschriebenen Datei.
    ///
    /// # Errors
    /// - [`PlanBridgeError::Research`]: wenn `validate_finding` das Finding
    ///   ablehnt (leere Pflichtfelder, fehlende Evidenz bei Confidence
    ///   `>= Medium`).
    /// - [`PlanBridgeError::Json`]: wenn das Finding nicht serialisierbar ist.
    /// - [`PlanBridgeError::Io`]: bei Fehlern beim Anlegen des Verzeichnisses
    ///   oder beim atomaren Ersetzen der Datei.
    ///
    /// # Concurrency
    /// Sicher aus mehreren Threads; der `rename` am Ende ist atomar.
    pub fn write(
        &self,
        plan_id: &str,
        finding: &ResearchFinding,
    ) -> Result<PathBuf, PlanBridgeError> {
        validate_finding(finding)?;

        let path = self.path_for(plan_id, finding.question_id.as_str());
        let document = render_document(finding)?;
        harw_knowledge::store::write_atomic(&path, &document).map_err(map_knowledge_error)?;

        tracing::debug!(
            plan_id = plan_id,
            question_id = %finding.question_id,
            path = %path.display(),
            "Finding-Artefakt geschrieben"
        );
        Ok(path)
    }

    /// Liest ein zuvor geschriebenes Finding zurück.
    ///
    /// # Description
    /// Rekonstruiert das [`ResearchFinding`] aus dem kanonischen JSON-Block
    /// des Artefakts, nicht aus der Prosa. Das Ergebnis ist bitgenau das
    /// Objekt, das [`FindingStore::write`] übergeben bekam.
    ///
    /// # Arguments
    /// - `plan_id` (`&str`): Plan-Bezeichner (wird sanitisiert).
    /// - `question_id` (`&str`): Frage-Bezeichner (wird sanitisiert).
    ///
    /// # Returns
    /// Das rekonstruierte [`ResearchFinding`].
    ///
    /// # Errors
    /// - [`PlanBridgeError::Io`]: wenn die Datei fehlt oder nicht lesbar ist.
    /// - [`PlanBridgeError::Research`]: wenn die Sentinel-Marken des
    ///   kanonischen Blocks fehlen (`ResearchError::SchemaMismatch`) oder das
    ///   JSON den Recherche-Vertrag verletzt.
    pub fn read(
        &self,
        plan_id: &str,
        question_id: &str,
    ) -> Result<ResearchFinding, PlanBridgeError> {
        let path = self.path_for(plan_id, question_id);
        let raw = std::fs::read_to_string(&path)?;
        let block = extract_canonical_block(&raw, &path)?;
        let finding = parse_finding(block)?;
        Ok(finding)
    }

    /// Listet die Frage-Bezeichner aller abgelegten Findings eines Plans.
    ///
    /// # Description
    /// Liest `<root>/<plan_id>/research/` und liefert die Dateinamen ohne die
    /// `.md`-Endung. Symlinks und Unterverzeichnisse werden übersprungen — ein
    /// untergeschobener Link soll den Store nicht nach außen führen. Existiert
    /// das Verzeichnis nicht, ist das Ergebnis leer (kein Fehler): ein Plan
    /// ohne Recherche ist ein gültiger Zustand.
    ///
    /// # Arguments
    /// - `plan_id` (`&str`): Plan-Bezeichner (wird sanitisiert).
    ///
    /// # Returns
    /// Die Frage-Bezeichner, aufsteigend sortiert (deterministisch).
    ///
    /// # Errors
    /// - [`PlanBridgeError::Io`]: wenn das Verzeichnis existiert, aber nicht
    ///   gelesen werden kann.
    pub fn list(&self, plan_id: &str) -> Result<Vec<String>, PlanBridgeError> {
        let dir = self
            .root
            .join(sanitize_component(plan_id))
            .join(RESEARCH_DIR);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }

        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                ids.push(stem.to_owned());
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Erzeugt den [`EvidenceRef`], der an den Plan-Knoten gehängt wird.
    ///
    /// # Description
    /// `kind` ist [`EvidenceKind::Finding`], `locator` der relative Pfad des
    /// Artefakts (siehe [`finding_locator`]) und `attached_at` der
    /// Produktionszeitpunkt des Findings — nicht die aktuelle Systemzeit,
    /// damit die TTL-Prüfung in `harw_plan::graph::missing_explorations` das
    /// tatsächliche Alter der Recherche sieht.
    ///
    /// # Arguments
    /// - `plan_id` (`&str`): Plan-Bezeichner (wird sanitisiert).
    /// - `finding` (`&ResearchFinding`): das belegende Ergebnis.
    /// - `actor` (`&str`): Akteur, der die Evidenz anhängt (runtime-gesetzt,
    ///   nicht aus der Modell-Ausgabe).
    ///
    /// # Returns
    /// Den fertigen [`EvidenceRef`].
    ///
    /// # Concurrency
    /// Rein funktional, keine I/O.
    #[must_use]
    pub fn evidence_for(
        &self,
        plan_id: &str,
        finding: &ResearchFinding,
        actor: &str,
    ) -> EvidenceRef {
        evidence_for_finding(plan_id, finding, actor)
    }
}

/// Freie Variante von [`FindingStore::evidence_for`] für reine Aufrufer.
///
/// # Description
/// Der [`crate::controller::PlanController`] darf keine I/O machen und damit
/// auch keinen [`FindingStore`] halten. Er benutzt deshalb diese Funktion —
/// identisches Ergebnis, kein Store nötig.
///
/// # Arguments
/// - `plan_id` (`&str`): Plan-Bezeichner (wird sanitisiert).
/// - `finding` (`&ResearchFinding`): das belegende Ergebnis.
/// - `actor` (`&str`): Akteur, der die Evidenz anhängt.
///
/// # Returns
/// Den [`EvidenceRef`] mit `kind = Finding`.
#[must_use]
pub fn evidence_for_finding(plan_id: &str, finding: &ResearchFinding, actor: &str) -> EvidenceRef {
    // `render_document` liefert dieselben Bytes, die `FindingStore::write`
    // unter `locator` ablegt (deterministisch aus `finding`, kein
    // Wanduhr-/Zufallsanteil). Der Digest macht den Lokator prüfbar: ein
    // Verifizierer kann später erkennen, ob die Datei unter dem Lokator noch
    // zu diesem Finding gehört, statt sich blind auf den Pfad zu verlassen.
    // Schlägt die Serialisierung ausnahmsweise fehl, bleibt der Nachweis
    // trotzdem gültig, nur ohne Digest — fail closed statt eines falschen.
    let digest = render_document(finding)
        .ok()
        .map(|document| harw_types::ContentDigest::of(document.as_bytes()));

    EvidenceRef {
        kind: EvidenceKind::Finding,
        locator: finding_locator(plan_id, finding.question_id.as_str()),
        attached_at: offset_from_timestamp(finding.produced_at),
        actor: actor.to_owned(),
        digest,
    }
}

/// Rendert das vollständige Markdown-Dokument eines Findings.
///
/// # Errors
/// [`PlanBridgeError::Json`], wenn das Finding nicht serialisierbar ist.
fn render_document(finding: &ResearchFinding) -> Result<String, PlanBridgeError> {
    let canonical = serde_json::to_string_pretty(finding)?;
    let mut out = String::with_capacity(canonical.len() * 2);

    // ── Frontmatter ──────────────────────────────────────────────────────
    out.push_str("---\n");
    out.push_str("question_id: ");
    out.push_str(&yaml_quote(finding.question_id.as_str()));
    out.push('\n');
    out.push_str("confidence: ");
    out.push_str(confidence_label(finding.confidence));
    out.push('\n');
    out.push_str("produced_by: ");
    out.push_str(&yaml_quote(&finding.produced_by));
    out.push('\n');
    out.push_str("produced_at: ");
    out.push_str(&yaml_quote(&finding.produced_at.to_string()));
    out.push('\n');
    out.push_str(&format!("evidence_count: {}\n", finding.evidence.len()));
    out.push_str("---\n\n");

    // ── Körper ───────────────────────────────────────────────────────────
    out.push_str(&format!("# Finding {}\n\n", finding.question_id));
    out.push_str("## Conclusion\n\n");
    out.push_str(finding.conclusion.trim());
    out.push_str("\n\n");

    push_section(
        &mut out,
        "Evidenz",
        finding.evidence.iter().map(|source| {
            let excerpt = source.excerpt.trim();
            if excerpt.is_empty() {
                format!("`{}` (abgerufen {})", source.locator, source.retrieved_at)
            } else {
                format!(
                    "`{}` (abgerufen {}) — {excerpt}",
                    source.locator, source.retrieved_at
                )
            }
        }),
    );

    push_section(
        &mut out,
        "Verifizierte Versionen",
        finding.verified_versions.iter().map(|version| {
            format!(
                "`{} = \"{}\"` (geprüft gegen {})",
                version.package, version.version, version.verified_against
            )
        }),
    );

    push_section(&mut out, "Constraints", finding.constraints.iter().cloned());
    push_section(
        &mut out,
        "Kompatibilität",
        finding.compatibility_notes.iter().cloned(),
    );
    push_section(
        &mut out,
        "Offene Fragen",
        finding.unresolved_questions.iter().cloned(),
    );

    // ── Kanonischer Block ────────────────────────────────────────────────
    out.push_str("## Kanonisch\n\n");
    out.push_str(JSON_BEGIN);
    out.push_str("\n```json\n");
    out.push_str(&canonical);
    out.push_str("\n```\n");
    out.push_str(JSON_END);
    out.push('\n');

    Ok(out)
}

/// Hängt eine Markdown-Liste an, sofern sie mindestens einen Eintrag hat.
fn push_section(out: &mut String, heading: &str, items: impl Iterator<Item = String>) {
    let entries: Vec<String> = items.collect();
    if entries.is_empty() {
        return;
    }
    out.push_str(&format!("## {heading}\n\n"));
    for entry in entries {
        out.push_str("- ");
        out.push_str(entry.trim());
        out.push('\n');
    }
    out.push('\n');
}

/// Schneidet den kanonischen JSON-Block aus einem Artefakt heraus.
///
/// # Errors
/// [`PlanBridgeError::Research`] mit `SchemaMismatch`, wenn eine Sentinel-Marke
/// fehlt oder in falscher Reihenfolge steht.
fn extract_canonical_block<'a>(raw: &'a str, path: &Path) -> Result<&'a str, PlanBridgeError> {
    let schema_error =
        |reason: String| PlanBridgeError::Research(ResearchError::SchemaMismatch { reason });

    let start = raw.find(JSON_BEGIN).ok_or_else(|| {
        schema_error(format!(
            "Finding-Artefakt '{}' hat keinen kanonischen JSON-Block",
            path.display()
        ))
    })?;
    let after_begin = start + JSON_BEGIN.len();
    let end = raw[after_begin..]
        .find(JSON_END)
        .map(|offset| after_begin + offset)
        .ok_or_else(|| {
            schema_error(format!(
                "Finding-Artefakt '{}' hat einen unbeendeten kanonischen JSON-Block",
                path.display()
            ))
        })?;

    Ok(raw[after_begin..end].trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_research::{QuestionId, SourceClass, SourceReference, VersionReference};

    fn timestamp() -> jiff::Timestamp {
        match "2026-08-27T10:00:00Z".parse::<jiff::Timestamp>() {
            Ok(ts) => ts,
            // Ein festes Literal kann nicht scheitern; der Zweig hält den Test
            // frei von `unwrap()`.
            Err(_) => jiff::Timestamp::UNIX_EPOCH,
        }
    }

    fn finding(question_id: &str) -> ResearchFinding {
        ResearchFinding {
            question_id: QuestionId::new(question_id),
            likelihood: None,
            confidence_rationale: String::new(),
            hypotheses: vec![],
            key_assumptions: vec![],
            indicators: vec![],
            dissent: vec![],
            conclusion: "jiff 0.2.32 ist die aktuelle Version.".to_owned(),
            evidence: vec![SourceReference {
                kind: SourceClass::CargoRegistrySource,
                reliability: None,
                credibility: None,
                derived_from: None,
                locator: "crates.io/crates/jiff".to_owned(),
                retrieved_at: timestamp(),
                digest: None,
                excerpt: "version = \"0.2.32\"".to_owned(),
            }],
            verified_versions: vec![VersionReference {
                package: "jiff".to_owned(),
                ecosystem: "cargo".to_owned(),
                version: "0.2.32".to_owned(),
                msrv: Some("1.85".to_owned()),
                features: vec!["serde".to_owned()],
                verified_against: "crates.io".to_owned(),
            }],
            constraints: vec!["MSRV 1.85".to_owned()],
            compatibility_notes: vec!["nicht mit chrono mischen".to_owned()],
            unresolved_questions: vec![],
            confidence: Confidence::High,
            produced_by: "explorer-1".to_owned(),
            produced_at: timestamp(),
        }
    }

    fn temp_store(label: &str) -> TestResult<(FindingStore, tempfile::TempDir)> {
        let dir = tempfile::tempdir().map_err(|error| {
            TestError::Unexpected(format!("Temp-Verzeichnis für '{label}' anlegen: {error}"))
        })?;
        Ok((FindingStore::new(dir.path().join("plans")), dir))
    }

    /// Prüft, dass `sanitized` aus `expected_prefix` (der zeichen-ersetzten,
    /// noch nicht gekürzten Komponente) gefolgt von einem Hash-Suffix
    /// `-<16 Hex-Zeichen>` besteht — dem Format aus [`hash_suffix`], das
    /// [`sanitize_component`] bei verlustbehafteter Sanitisierung anhängt.
    /// Der genaue Hash-Wert ist bewusst nicht Teil der Prüfung: er hängt vom
    /// BLAKE3-Digest der Eingabe ab und ist kein von diesem Modul
    /// garantierter Vertrag, nur seine Form.
    fn assert_lossy_result(sanitized: &str, expected_prefix: &str) -> TestResult {
        let rest = sanitized.strip_prefix(expected_prefix).ok_or_else(|| {
            TestError::Unexpected(format!(
                "'{sanitized}' beginnt nicht mit erwartetem Präfix '{expected_prefix}'"
            ))
        })?;
        let suffix = rest.strip_prefix('-').ok_or_else(|| {
            TestError::Unexpected(format!(
                "'{sanitized}' hat nach '{expected_prefix}' kein '-'-Hash-Suffix"
            ))
        })?;
        if suffix.len() != 16 || !suffix.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Err(TestError::Unexpected(format!(
                "Suffix '{suffix}' in '{sanitized}' ist kein 16-stelliger Hex-Wert"
            )));
        }
        Ok(())
    }

    #[test]
    fn test_sanitize_component_strips_separators_and_traversal() -> TestResult {
        // Unveränderte, bereits gültige Segmente bleiben identisch — nichts
        // war verlustbehaftet, also gibt es kein Hash-Suffix.
        assert_eq!(sanitize_component("   "), BLANK_COMPONENT);
        assert_eq!(sanitize_component(""), BLANK_COMPONENT);
        assert_eq!(sanitize_component("q-1"), "q-1");
        assert_eq!(sanitize_component("q_1.v2"), "q_1.v2");

        // Zeichen-Ersetzung und/oder eingefügtes Dotfile-Präfix sind
        // verlustbehaftet: das Ergebnis trägt zusätzlich den Hash-Suffix aus
        // `hash_suffix`, damit unterschiedliche Rohwerte, die auf dieselbe
        // Zeichen-Ersetzung abbilden (siehe
        // `test_sanitize_component_disambiguates_same_character_mapping`),
        // trotzdem unterscheidbar bleiben.
        assert_lossy_result(&sanitize_component("../../etc/passwd"), "_..-..-etc-passwd")?;
        assert_lossy_result(&sanitize_component("a/b"), "a-b")?;
        assert_lossy_result(&sanitize_component("a\\b"), "a-b")?;
        // Führendes `_`, weil die Dotfile-Regel auch hier greift: `%` wird zu
        // `-`, der Rest beginnt mit `..` — und ein Ergebnis, das mit `.` anfängt,
        // wäre ein Dotfile.
        assert_lossy_result(&sanitize_component("..%2f..%2fetc"), "_..-2f..-2fetc")?;
        Ok(())
    }

    #[test]
    fn test_sanitize_component_disambiguates_same_character_mapping() -> TestResult {
        // Vor dem Befund landeten `a/b`, `a:b` und `a b` alle bei `a-b` und
        // teilten sich damit eine Datei (letzter `write` gewinnt, ohne dass
        // die ältere Frage das merkt). Das Hash-Suffix macht sie wieder
        // unterscheidbar.
        let slash = sanitize_component("a/b");
        let colon = sanitize_component("a:b");
        let space = sanitize_component("a b");

        assert_ne!(slash, colon, "'a/b' und 'a:b' kollidieren weiterhin");
        assert_ne!(slash, space, "'a/b' und 'a b' kollidieren weiterhin");
        assert_ne!(colon, space, "'a:b' und 'a b' kollidieren weiterhin");

        // Alle drei ersetzen ihr Trennzeichen durch `-` und tragen deshalb
        // denselben (nicht gekürzten) Präfix `a-b`.
        assert_lossy_result(&slash, "a-b")?;
        assert_lossy_result(&colon, "a-b")?;
        assert_lossy_result(&space, "a-b")?;

        // Deterministisch: dieselbe Eingabe liefert immer dasselbe Ergebnis.
        assert_eq!(slash, sanitize_component("a/b"));
        Ok(())
    }

    #[test]
    fn test_sanitize_component_never_yields_dot_or_dotdot_or_dotfile() {
        // `..%2f..%2fetc` und `../x` stehen hier, weil genau sie in
        // `test_sanitize_component_strips_separators_and_traversal` eine
        // veraltete Erwartung hatten. Die Regel gehört in **diesen** Test —
        // eine feste Erwartung dort veraltet, eine Invariante hier nicht.
        for hostile in [
            ".",
            "..",
            ".ssh",
            "...",
            " .. ",
            "..%2f..%2fetc",
            "../x",
            "..\\x",
            "./.",
        ] {
            let sanitized = sanitize_component(hostile);
            assert_ne!(sanitized, ".", "'{hostile}' wurde zu '.'");
            assert_ne!(sanitized, "..", "'{hostile}' wurde zu '..'");
            assert!(
                !sanitized.starts_with('.'),
                "'{hostile}' wurde zum Dotfile '{sanitized}'"
            );
        }
    }

    #[test]
    fn test_sanitize_component_caps_length() {
        let long = "x".repeat(MAX_COMPONENT_LEN * 3);
        // Der Cap gilt für das *Gesamtergebnis*, Hash-Suffix eingeschlossen —
        // sonst könnte `list()` Stems liefern, die länger als
        // `MAX_COMPONENT_LEN` sind (der ursprüngliche Befund).
        assert_eq!(sanitize_component(&long).len(), MAX_COMPONENT_LEN);
    }

    #[test]
    fn test_sanitize_component_removes_nul_and_control_bytes() -> TestResult {
        assert_lossy_result(&sanitize_component("a\0b\nc"), "a-b-c")?;
        Ok(())
    }

    #[test]
    fn test_path_for_never_escapes_the_root() -> TestResult {
        let store = FindingStore::new("/srv/plans");
        let path = store.path_for("../../root", "../../../etc/shadow");

        assert!(
            path.starts_with("/srv/plans"),
            "Pfad entkommt dem Root: {}",
            path.display()
        );
        // Entscheidend ist nicht die Abwesenheit der Zeichenfolge "..",
        // sondern die Abwesenheit einer `..`-*Komponente*: nur die
        // traversiert.
        assert!(
            !path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir)),
            "Traversal-Komponente übrig: {}",
            path.display()
        );
        // Und zwischen Root und Datei liegen exakt drei Segmente:
        // <plan_id>/research/<question_id>.md
        let relative = path
            .strip_prefix("/srv/plans")
            .map_err(ctx("strip_prefix schlug fehl"))?;
        assert_eq!(relative.components().count(), 3);
        Ok(())
    }

    #[test]
    fn test_write_then_read_roundtrips_the_finding() -> TestResult {
        let (store, _dir) = temp_store("roundtrip")?;
        let original = finding("q-1");

        let path = store
            .write("p-1", &original)
            .map_err(ctx("write schlug fehl"))?;
        assert!(path.exists());

        let restored = store.read("p-1", "q-1").map_err(ctx("read schlug fehl"))?;
        assert_eq!(restored, original);
        Ok(())
    }

    #[test]
    fn test_write_with_malicious_ids_stays_inside_the_root() -> TestResult {
        let (store, _dir) = temp_store("malicious")?;
        let mut hostile = finding("../../../etc/passwd");
        hostile.question_id = QuestionId::new("../../../etc/passwd");

        let path = store
            .write("../../outside", &hostile)
            .map_err(ctx("write schlug fehl"))?;
        assert!(
            path.starts_with(store.root()),
            "Artefakt liegt außerhalb des Roots: {}",
            path.display()
        );

        // Und es ist unter denselben (sanitisierten) IDs wieder lesbar.
        let restored = store
            .read("../../outside", "../../../etc/passwd")
            .map_err(ctx("read schlug fehl"))?;
        assert_eq!(restored.question_id, hostile.question_id);
        Ok(())
    }

    #[test]
    fn test_write_keeps_distinct_question_ids_apart_after_sanitization() -> TestResult {
        // Befund: `a/b` und `a:b` sanitisierten beide zu `a-b` und teilten
        // sich eine Datei — der zweite `write` überschrieb den ersten, ohne
        // dass jemand es merkte. Mit dem Hash-Suffix landen sie in
        // verschiedenen Dateien.
        let (store, _dir) = temp_store("distinct-ids")?;
        let mut slash = finding("a/b");
        slash.conclusion = "Antwort für a/b".to_owned();
        let mut colon = finding("a:b");
        colon.conclusion = "Antwort für a:b".to_owned();

        let path_slash = store
            .write("p-1", &slash)
            .map_err(ctx("write 'a/b' schlug fehl"))?;
        let path_colon = store
            .write("p-1", &colon)
            .map_err(ctx("write 'a:b' schlug fehl"))?;
        assert_ne!(
            path_slash, path_colon,
            "'a/b' und 'a:b' landen in derselben Datei"
        );

        let restored_slash = store
            .read("p-1", "a/b")
            .map_err(ctx("read 'a/b' schlug fehl"))?;
        let restored_colon = store
            .read("p-1", "a:b")
            .map_err(ctx("read 'a:b' schlug fehl"))?;
        assert_eq!(restored_slash.conclusion, "Antwort für a/b");
        assert_eq!(restored_colon.conclusion, "Antwort für a:b");
        Ok(())
    }

    #[test]
    fn test_long_dotfile_question_id_roundtrips_through_list_and_read() -> TestResult {
        // Befund: eine lange, mit `.` beginnende ID wurde zu einem 129
        // Zeichen langen Dateinamen (Kürzung auf 128 vor dem `_`-Präfix statt
        // danach). `list()` liefert diesen Stem unverändert zurück; `read()`
        // sanitisierte ihn ein zweites Mal und landete wegen der erneuten
        // Kürzung bei einem anderen (kürzeren) Namen — `NotFound`.
        let (store, _dir) = temp_store("long-dotfile")?;
        let question_id = format!(".{}", "x".repeat(200));
        let mut source = finding(&question_id);
        source.conclusion = "Antwort für die lange Dotfile-ID".to_owned();
        store.write("p-1", &source).map_err(ctx("write schlug fehl"))?;

        let ids = store.list("p-1").map_err(ctx("list schlug fehl"))?;
        assert_eq!(ids.len(), 1, "erwartet genau einen Stem, bekommen: {ids:?}");
        let stem = ids.first().ok_or(TestError::Missing("Stem aus list()"))?;
        assert!(
            stem.len() <= MAX_COMPONENT_LEN,
            "Stem '{stem}' überschreitet die Obergrenze"
        );

        // `sanitize_component` ist idempotent: der von `list()` gelieferte
        // Stem sanitisiert sich selbst zu sich selbst und ist deshalb über
        // `read()` wieder auflösbar, ohne dass er zuvor ein weiteres Mal
        // gekürzt oder umbenannt würde.
        let restored_via_stem = store
            .read("p-1", stem.as_str())
            .map_err(ctx("read über list()-Stem schlug fehl"))?;
        assert_eq!(restored_via_stem.conclusion, "Antwort für die lange Dotfile-ID");

        // Und natürlich auch über die ursprüngliche, unsanitisierte ID.
        let restored_via_original_id = store
            .read("p-1", &question_id)
            .map_err(ctx("read über Original-ID schlug fehl"))?;
        assert_eq!(restored_via_original_id, restored_via_stem);
        Ok(())
    }

    #[test]
    fn test_document_contains_frontmatter_and_readable_body() -> TestResult {
        let document = render_document(&finding("q-1")).map_err(ctx("render schlug fehl"))?;
        assert!(document.starts_with("---\n"));
        assert!(document.contains("question_id: 'q-1'"));
        assert!(document.contains("confidence: high"));
        assert!(document.contains("evidence_count: 1"));
        assert!(document.contains("## Conclusion"));
        assert!(document.contains("crates.io/crates/jiff"));
        assert!(document.contains("## Constraints"));
        assert!(document.contains(JSON_BEGIN));
        assert!(document.contains(JSON_END));
        Ok(())
    }

    #[test]
    fn test_read_reports_schema_mismatch_without_canonical_block() -> TestResult {
        let (store, _dir) = temp_store("broken")?;
        let path = store.path_for("p-1", "q-broken");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(ctx("Verzeichnis anlegen"))?;
        }
        std::fs::write(&path, "---\nquestion_id: 'q'\n---\n\nnur Prosa\n")
            .map_err(ctx("Datei schreiben"))?;

        match store.read("p-1", "q-broken") {
            Err(PlanBridgeError::Research(ResearchError::SchemaMismatch { .. })) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet SchemaMismatch, bekommen: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_write_rejects_a_finding_that_violates_the_contract() -> TestResult {
        let (store, _dir) = temp_store("invalid")?;
        let mut invalid = finding("q-1");
        invalid.conclusion = "   ".to_owned();

        match store.write("p-1", &invalid) {
            Err(PlanBridgeError::Research(ResearchError::EmptyField { field })) => {
                assert_eq!(field, "conclusion");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet EmptyField, bekommen: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_list_is_sorted_and_empty_for_an_unknown_plan() -> TestResult {
        let (store, _dir) = temp_store("list")?;
        let ids = store.list("p-unknown").map_err(ctx("list schlug fehl"))?;
        assert!(ids.is_empty());

        for id in ["q-c", "q-a", "q-b"] {
            store
                .write("p-1", &finding(id))
                .map_err(ctx("write schlug fehl"))?;
        }
        let ids = store.list("p-1").map_err(ctx("list schlug fehl"))?;
        assert_eq!(ids, vec!["q-a", "q-b", "q-c"]);
        Ok(())
    }

    #[test]
    fn test_evidence_for_uses_the_relative_locator_and_production_time() -> TestResult {
        let (store, _dir) = temp_store("evidence")?;
        let source = finding("q-1");
        let evidence = store.evidence_for("p-1", &source, "runtime");

        assert_eq!(evidence.kind, EvidenceKind::Finding);
        assert_eq!(evidence.locator, "p-1/research/q-1.md");
        assert_eq!(evidence.actor, "runtime");
        assert_eq!(evidence.attached_at, offset_from_timestamp(timestamp()));

        // Der Digest deckt den Inhalt ab, auf den `locator` zeigt — nicht
        // nur den Pfad. `render_document` ist deterministisch in `finding`,
        // also identisch zu den Bytes, die `FindingStore::write` ablegen
        // würde.
        let document = render_document(&source).map_err(ctx("render schlug fehl"))?;
        assert_eq!(
            evidence.digest,
            Some(harw_types::ContentDigest::of(document.as_bytes()))
        );
        Ok(())
    }

    #[test]
    fn test_finding_locator_matches_the_store_path() {
        let store = FindingStore::new("/srv/plans");
        let path = store.path_for("p-1", "q-1");
        let relative = finding_locator("p-1", "q-1");
        assert!(
            path.ends_with(&relative),
            "{} vs {relative}",
            path.display()
        );
    }

    #[test]
    fn test_offset_from_timestamp_matches_unix_seconds() {
        let converted = offset_from_timestamp(timestamp());
        assert_eq!(converted.unix_timestamp(), timestamp().as_second());
    }
}
