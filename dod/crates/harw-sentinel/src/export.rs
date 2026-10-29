//! Optionaler JSON-Lines-Export zertifizierter Befunde für `harw-security-hub`.
//!
//! # Wozu
//! [`crate::findings::report_findings`] meldet jeden `Finding<RuleChecked>`
//! über `tracing` und einen Telemetriezähler. `harw-security-hub`
//! (`harw-security-hub/src/findings.rs`) korreliert DoD-Befunde aber aus einer
//! append-only JSON-Lines-Datei. Dieses Modul schreibt genau diese Datei —
//! **nur**, wenn `--findings-export <ABSOLUTER-PFAD>` gesetzt ist. Ohne das
//! Flag existiert kein [`FindingsExporter`], und es wird nichts geschrieben.
//!
//! # Zeilenformat (Vertrag mit dem Hub)
//! Eine Zeile je Befund, genau diese sechs Felder:
//!
//! ```json
//! {"finding_id":"…","host":"host-a","rule_id":"structure-drift","severity":"medium","summary":"…","observed_at":"2026-09-27T10:00:00Z"}
//! ```
//!
//! ## Schema version 2 (additive, PL-97 D1)
//! Every line written by this binary now also carries
//! `"schema_version": 2` ([`EXPORT_SCHEMA_VERSION`]) and, when the finding
//! can be attributed to a cgroup, an optional `"scope"` (the absolute cgroup
//! path, see [`sanitize_scope`]). Both fields are additive: a line without
//! `schema_version` is a version-1 line with exactly the six fields above,
//! and every known reader (`harw-security-hub`'s `DodFinding`, harw's
//! `security_signals` context source) ignores unknown fields, so v1 and v2
//! lines parse alike. Today no finding carries a cgroup attribution yet
//! (`Finding<RuleChecked>` has no actor; the id-to-path resolver is PL-97
//! E4), so `scope` is omitted from every line until that lands.
//!
//! Die Feldtypen sind dieselben, die der Hub deserialisiert:
//! [`harw_types::FindingId`], [`harw_types::HostId`],
//! [`harw_types::ImpactSeverity`] (snake_case) und `jiff::Timestamp`.
//! `harw_dod_signals::Severity` wird über [`impact_severity`] erschöpfend auf
//! `ImpactSeverity` abgebildet — keine String-Umwandlung dazwischen.
//!
//! # Keine Geheimnisse, bereinigter Freitext
//! Exportiert werden ausschließlich die sechs Felder oben (plus
//! `schema_version` und das optionale `scope`) — kein Beleg,
//! keine Rohereignisse, keine Konfiguration. Der einzige Freitext,
//! `summary`, wird von [`sanitize_summary`] bereinigt (Steuerzeichen und
//! bidirektionale Formatzeichen durch Leerzeichen ersetzt) und auf
//! [`MAX_SUMMARY_BYTES`] Bytes an einer Zeichengrenze gekürzt.
//!
//! # Datei
//! - geöffnet mit `O_APPEND | O_CREAT`, neu angelegt mit Modus
//!   [`EXPORT_FILE_MODE`] (`0640`); ein bestehender, breiter lesbarer
//!   Modus wird auf höchstens `0640` verengt,
//! - kein Symlink, keine Nicht-Datei am Zielpfad (sonst Schreibfehler),
//! - jede Zeile geht in einem einzigen `write_all` hinaus,
//! - Rotation: würde eine Zeile die Datei über [`MAX_EXPORT_BYTES`]
//!   (16 MiB) wachsen lassen, wird sie nach `<pfad>.1` umbenannt (ein
//!   älteres `.1` wird ersetzt) und neu angelegt.
//!
//! # Fehler
//! Ein Schreib-, Öffnungs- oder Rotationsfehler bricht den Sentinel nie ab:
//! er wird gezählt ([`FindingsExporter::failures`] und der Zähler
//! [`FINDINGS_EXPORT_FAILURE_TOTAL`]) und per `tracing::warn!` gemeldet; die
//! nächste Zeile versucht die Datei neu zu öffnen.
//!
//! # Nebenläufigkeit
//! [`FindingsExporter`] lebt ausschließlich im Sammelthread (`&mut self`),
//! wie `crate::poll_once`.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use harw_dod_rules::{Finding, RuleChecked};
use harw_dod_signals::Severity;
use harw_observe::{MetricValue, TelemetrySink};
use harw_types::{FindingId, HostId, ImpactSeverity};
use jiff::Timestamp;
use serde::Serialize;

/// Größenschwelle, ab der die Exportdatei nach `<pfad>.1` rotiert wird.
pub const MAX_EXPORT_BYTES: u64 = 16 * 1024 * 1024;

/// Längste exportierte `summary` in Bytes (UTF-8, an Zeichengrenze gekürzt).
///
/// Gleich `harw_security_hub::findings::FindingLimits::max_summary_chars`
/// (512) — in Bytes gemessen ist das eine strengere Grenze.
pub const MAX_SUMMARY_BYTES: usize = 512;

/// Modus einer neu angelegten Exportdatei: Eigentümer lesen/schreiben,
/// Gruppe lesen (der Hub liest über Gruppenmitgliedschaft), sonst nichts.
pub const EXPORT_FILE_MODE: u32 = 0o640;

/// Kernel-Hostname, Vorgabe für das `host`-Feld ohne `--findings-host`.
pub const HOSTNAME_PATH: &str = "/proc/sys/kernel/hostname";

/// Schema version written into every export line (`schema_version`).
///
/// Version 1 is the original six-field line without this field; version 2
/// adds `schema_version` itself and the optional `scope`. Both changes are
/// additive (see the module documentation).
pub const EXPORT_SCHEMA_VERSION: u32 = 2;

/// Longest exported `scope` (cgroup path) in bytes; a longer value is
/// omitted rather than truncated, because a cut path would name a
/// different cgroup.
pub const MAX_SCOPE_BYTES: usize = 4096;

harw_macros::metrics! {
    /// Wie oft eine Befundzeile seit Prozessstart nicht in die
    /// JSON-Lines-Exportdatei geschrieben werden konnte (Öffnen, Rotation,
    /// Kodierung oder Schreiben). Erwartet: `0`.
    FINDINGS_EXPORT_FAILURE_TOTAL: counter, unit = count, labels = [], cardinality = single,
        name = "harw_sentinel_findings_export_failure_total";
}

/// Eine Exportzeile — die ersten sechs Feldnamen und -typen spiegeln exakt
/// `harw_security_hub::findings::DodFinding`; `schema_version` und `scope`
/// sind die additiven Felder von Version 2 (siehe Moduldoku).
#[derive(Debug, Serialize)]
struct ExportLine<'a> {
    schema_version: u32,
    finding_id: &'a FindingId,
    host: &'a HostId,
    rule_id: &'a str,
    severity: ImpactSeverity,
    summary: String,
    observed_at: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<&'a str>,
}

/// Bildet die DoD-Schwere erschöpfend auf die hub-seitige Skala ab.
///
/// Erschöpfendes `match` ohne Wildcard: eine neue Variante bricht den Build,
/// statt still falsch exportiert zu werden.
#[must_use]
pub const fn impact_severity(severity: Severity) -> ImpactSeverity {
    match severity {
        Severity::Info => ImpactSeverity::Info,
        Severity::Low => ImpactSeverity::Low,
        Severity::Medium => ImpactSeverity::Medium,
        Severity::High => ImpactSeverity::High,
        Severity::Critical => ImpactSeverity::Critical,
    }
}

/// `true` für Zeichen, die in einer exportierten Zusammenfassung nichts
/// verloren haben: Unicode-Steuerzeichen (`Cc`) sowie Zeichen, die die
/// Darstellung umordnen oder verstecken (Bidi-Overrides/-Isolates,
/// Nullbreiten, BOM, Zeilen-/Absatztrenner).
fn is_unsafe_char(ch: char) -> bool {
    ch.is_control()
        || matches!(
            ch,
            '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

/// Bereinigt eine Befund-Zusammenfassung für den Export.
///
/// # Description
/// Ersetzt jedes Zeichen aus [`is_unsafe_char`] durch ein Leerzeichen und
/// kürzt das Ergebnis auf höchstens [`MAX_SUMMARY_BYTES`] Bytes, ohne ein
/// UTF-8-Zeichen zu zerschneiden.
#[must_use]
pub fn sanitize_summary(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_SUMMARY_BYTES));
    for ch in raw.chars() {
        let ch = if is_unsafe_char(ch) { ' ' } else { ch };
        if out.len() + ch.len_utf8() > MAX_SUMMARY_BYTES {
            break;
        }
        out.push(ch);
    }
    out
}

/// Validates an optional `scope` (cgroup path) for the export.
///
/// # Description
/// Returns the value unchanged when it is non-empty, absolute, at most
/// [`MAX_SCOPE_BYTES`] bytes long and free of the characters rejected by
/// [`is_unsafe_char`]. Anything else yields `None`: the line is then written
/// without `scope` instead of carrying a rewritten or truncated path that
/// would name a different cgroup.
#[must_use]
pub fn sanitize_scope(raw: Option<&str>) -> Option<&str> {
    let scope = raw?;
    let acceptable = scope.starts_with('/')
        && scope.len() <= MAX_SCOPE_BYTES
        && !scope.chars().any(is_unsafe_char);
    acceptable.then_some(scope)
}

/// Kodiert eine Exportzeile inklusive abschließendem `\n`.
///
/// `scope` is the optional cgroup attribution of version 2; it passes
/// through [`sanitize_scope`] and is omitted when absent or unacceptable.
///
/// # Errors
/// `serde_json::Error`, falls die Serialisierung scheitert (für diese
/// Feldtypen praktisch unerreichbar, aber nicht als Panik behandelt).
pub fn encode_line(
    finding_id: &FindingId,
    host: &HostId,
    rule_id: &str,
    severity: Severity,
    summary: &str,
    observed_at: Timestamp,
    scope: Option<&str>,
) -> Result<String, serde_json::Error> {
    let line = ExportLine {
        schema_version: EXPORT_SCHEMA_VERSION,
        finding_id,
        host,
        rule_id,
        severity: impact_severity(severity),
        summary: sanitize_summary(summary),
        observed_at,
        scope: sanitize_scope(scope),
    };
    let mut encoded = serde_json::to_string(&line)?;
    encoded.push('\n');
    Ok(encoded)
}

/// Ermittelt das `host`-Feld: `explicit` (aus `--findings-host`) oder, ohne
/// Angabe, den Kernel-Hostnamen aus `hostname_path`.
///
/// # Returns
/// `None`, wenn weder eine Angabe vorliegt noch der Hostname lesbar und
/// nicht leer ist — der Export bleibt dann aus (siehe `crate::run`).
#[must_use]
pub fn resolve_host(explicit: Option<&HostId>, hostname_path: &Path) -> Option<HostId> {
    if let Some(host) = explicit {
        return Some(host.clone());
    }
    let raw = fs::read_to_string(hostname_path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().any(is_unsafe_char) {
        return None;
    }
    HostId::try_from_str(trimmed).ok()
}

/// Pfad der rotierten Vorgängerdatei: `<pfad>.1`.
fn rotated_path(path: &Path) -> PathBuf {
    generation_path(path, 1)
}

/// `<pfad>.<generation>`.
fn generation_path(path: &Path, generation: u32) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(format!(".{generation}"));
    PathBuf::from(name)
}

/// Öffnet die Exportdatei append-only; legt sie mit [`EXPORT_FILE_MODE`] an.
fn open_export_file(path: &Path) -> io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "findings export path exists but is not a regular file",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(EXPORT_FILE_MODE);
    }
    let file = options.open(path)?;
    restrict_mode(&file)?;
    Ok(file)
}

/// Verengt den Modus einer bereits bestehenden Datei auf höchstens `0640`.
#[cfg(unix)]
fn restrict_mode(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = file.metadata()?.permissions().mode() & 0o7777;
    if mode & !EXPORT_FILE_MODE != 0 {
        file.set_permissions(fs::Permissions::from_mode(mode & EXPORT_FILE_MODE))?;
    }
    Ok(())
}

/// Siehe die Unix-Fassung; ohne Unix-Modusbits ein No-Op.
#[cfg(not(unix))]
fn restrict_mode(_file: &File) -> io::Result<()> {
    Ok(())
}

/// Append-only JSON-Lines-Schreiber für zertifizierte Befunde.
///
/// # Description
/// Hält die offene Datei, den `host`-Wert jeder Zeile und die Zähler für
/// geschriebene und gescheiterte Zeilen. Siehe Moduldoku für Format,
/// Rotation und Fehlerverhalten.
#[derive(Debug)]
pub struct FindingsExporter {
    path: PathBuf,
    rotated: PathBuf,
    /// Anzahl aufbewahrter rotierter Generationen (`<pfad>.1` … `<pfad>.N`).
    keep: u32,
    host: HostId,
    max_bytes: u64,
    file: Option<File>,
    written: u64,
    failures: u64,
}

impl FindingsExporter {
    /// Baut einen Exporter für `path` mit der Rotationsschwelle
    /// [`MAX_EXPORT_BYTES`]. Öffnet noch nichts — siehe [`Self::prepare`].
    #[must_use]
    pub fn new(path: PathBuf, host: HostId) -> Self {
        Self::with_max_bytes(path, host, MAX_EXPORT_BYTES)
    }

    /// Wie [`Self::new`], mit frei wählbarer Rotationsschwelle (Tests).
    #[must_use]
    pub fn with_max_bytes(path: PathBuf, host: HostId, max_bytes: u64) -> Self {
        let rotated = rotated_path(&path);
        Self {
            path,
            rotated,
            keep: 1,
            host,
            max_bytes,
            file: None,
            written: 0,
            failures: 0,
        }
    }

    /// Bewahrt `keep` rotierte Generationen auf (`<pfad>.1` neueste …
    /// `<pfad>.<keep>` älteste; Standard 1, mindestens 1). Die älteste fällt
    /// bei der nächsten Rotation heraus — der Platzbedarf ist damit durch
    /// `(keep + 1) × max_bytes` begrenzt.
    #[must_use]
    pub fn with_keep(mut self, keep: u32) -> Self {
        self.keep = keep.max(1);
        self
    }

    /// Zielpfad der Exportdatei.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Verzeichnis der Exportdatei — muss für Rotation und Neuanlage
    /// beschreibbar bleiben (Landlock-Regel in `crate::run`).
    #[must_use]
    pub fn directory(&self) -> Option<&Path> {
        self.path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
    }

    /// Anzahl erfolgreich geschriebener Zeilen seit Prozessstart.
    #[must_use]
    pub const fn written(&self) -> u64 {
        self.written
    }

    /// Anzahl gescheiterter Zeilen (bzw. Öffnungsversuche) seit Prozessstart.
    #[must_use]
    pub const fn failures(&self) -> u64 {
        self.failures
    }

    /// Öffnet die Datei vorab (vor der Landlock-Selbstbeschränkung), damit
    /// ein Konfigurationsfehler schon beim Start im Log steht.
    ///
    /// Ein Fehlschlag wird gezählt und geloggt, nie zurückgegeben.
    pub fn prepare(&mut self, sink: &dyn TelemetrySink) {
        let opened = self.ensure_open().map(|_| ());
        if let Err(error) = opened {
            self.record_failure(sink, &error, "open");
        }
    }

    /// Schreibt eine Zeile für `finding`.
    ///
    /// # Returns
    /// `true`, wenn die Zeile geschrieben wurde; `false` nach einem
    /// gezählten und geloggten Fehler.
    pub fn export(&mut self, sink: &dyn TelemetrySink, finding: &Finding<RuleChecked>) -> bool {
        // `Finding<RuleChecked>` trägt (noch) keine cgroup-Zuordnung; `scope`
        // bleibt deshalb leer, bis PL-97 E4 den id→Pfad-Auflöser liefert.
        self.export_fields(
            sink,
            finding.id(),
            finding.rule_id(),
            finding.severity(),
            finding.summary(),
            finding.observed_at(),
            None,
        )
    }

    /// Wie [`Self::export`], aus Einzelfeldern — der eigentliche Schreibweg.
    ///
    /// `scope` is the optional cgroup attribution (schema version 2).
    #[allow(clippy::too_many_arguments)]
    pub fn export_fields(
        &mut self,
        sink: &dyn TelemetrySink,
        finding_id: &FindingId,
        rule_id: &str,
        severity: Severity,
        summary: &str,
        observed_at: Timestamp,
        scope: Option<&str>,
    ) -> bool {
        let line = match encode_line(
            finding_id,
            &self.host,
            rule_id,
            severity,
            summary,
            observed_at,
            scope,
        ) {
            Ok(line) => line,
            Err(error) => {
                self.record_failure(sink, &error, "encode");
                return false;
            }
        };
        match self.append(line.as_bytes()) {
            Ok(()) => {
                self.written = self.written.saturating_add(1);
                true
            }
            Err(error) => {
                self.record_failure(sink, &error, "write");
                false
            }
        }
    }

    fn ensure_open(&mut self) -> io::Result<&mut File> {
        let file = match self.file.take() {
            Some(file) => file,
            None => open_export_file(&self.path)?,
        };
        Ok(self.file.insert(file))
    }

    fn append(&mut self, line: &[u8]) -> io::Result<()> {
        let len = u64::try_from(line.len()).unwrap_or(u64::MAX);
        let size = self.ensure_open()?.metadata()?.len();
        if size > 0 && size.saturating_add(len) > self.max_bytes {
            self.rotate()?;
        }
        let result = self.ensure_open()?.write_all(line);
        if result.is_err() {
            // Beim nächsten Mal frisch öffnen statt auf einem kaputten
            // Deskriptor weiterzuschreiben.
            self.file = None;
        }
        result
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        // Ältere Generationen nach hinten schieben (`.N-1` → `.N`, …,
        // `.1` → `.2`); die älteste wird dabei ersetzt. Fehlende Stufen sind
        // kein Fehler.
        for generation in (1..self.keep).rev() {
            let from = generation_path(&self.path, generation);
            let to = generation_path(&self.path, generation + 1);
            match fs::rename(&from, &to) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        match fs::rename(&self.path, &self.rotated) {
            Ok(()) => {
                tracing::info!(
                    path = %self.path.display(),
                    rotated = %self.rotated.display(),
                    keep = self.keep,
                    written = self.written(),
                    "findings export rotated"
                );
                Ok(())
            }
            // Extern entfernt: nichts zu rotieren, einfach neu anlegen.
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn record_failure(
        &mut self,
        sink: &dyn TelemetrySink,
        error: &dyn std::fmt::Display,
        stage: &'static str,
    ) {
        self.failures = self.failures.saturating_add(1);
        sink.record(&FINDINGS_EXPORT_FAILURE_TOTAL, MetricValue::Count(1), &[]);
        tracing::warn!(
            error = %error,
            stage,
            path = %self.path.display(),
            failures = self.failures(),
            "findings export failed; the finding is still logged and counted"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_observe::NullSink;
    use serde::Deserialize;

    /// Spiegel von `harw_security_hub::findings::DodFinding` — derselbe
    /// Feldsatz, dieselben Typen, und wie dort **ohne**
    /// `deny_unknown_fields`: der Hub ignoriert die additiven Felder von
    /// Version 2. Nur lokaler Rauchtest; verbindlich sind die Tests der
    /// echten Leser gegen [`V2_FIXTURE_LINE`].
    #[derive(Debug, Deserialize)]
    struct HubDodFinding {
        finding_id: FindingId,
        host: HostId,
        rule_id: String,
        severity: ImpactSeverity,
        summary: String,
        observed_at: Timestamp,
    }

    /// Spiegel der Zeile, die `harw-runtime/src/security_signals.rs`
    /// (`Line`) liest — drei Strings, unbekannte Felder werden ignoriert.
    #[derive(Debug, Deserialize)]
    struct SecuritySignalsLine {
        rule_id: String,
        severity: String,
        observed_at: String,
    }

    /// Strenger Spiegel einer Version-2-Zeile: `deny_unknown_fields`, damit
    /// der Export **genau** diese Felder trägt und keines mehr.
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ExportLineV2 {
        #[serde(default = "schema_v1")]
        schema_version: u32,
        finding_id: FindingId,
        host: HostId,
        rule_id: String,
        severity: ImpactSeverity,
        summary: String,
        observed_at: Timestamp,
        #[serde(default)]
        scope: Option<String>,
    }

    /// A line without `schema_version` is a version-1 line.
    const fn schema_v1() -> u32 {
        1
    }

    /// Eine Version-1-Zeile, wie sie vor PL-97 D1 geschrieben wurde.
    const V1_LINE: &str = r#"{"finding_id":"f-1","host":"host-a","rule_id":"structure-drift","severity":"high","summary":"s","observed_at":"2026-09-27T10:00:00Z"}"#;

    /// Canonical version-2 fixture line: byte-for-byte what [`encode_line`]
    /// writes for this finding (pinned by
    /// `test_v2_fixture_line_is_byte_identical_to_encode_line`). The
    /// reader-side tests in `harw-security-hub` (`findings::DodFinding`,
    /// tail reader) and `harw-runtime` (`security_signals::count_recent`)
    /// copy exactly this line, so a change to the encoder or to a reader
    /// (`deny_unknown_fields`, a new required field) breaks a test on the
    /// side that changed.
    const V2_FIXTURE_LINE: &str = r#"{"schema_version":2,"finding_id":"f-1","host":"host-a","rule_id":"structure-drift","severity":"high","summary":"s","observed_at":"2026-09-27T10:00:00Z","scope":"/sys/fs/cgroup/harw.slice"}"#;

    fn host() -> TestResult<HostId> {
        HostId::try_from_str("host-a").map_err(ctx("host id"))
    }

    fn finding_id() -> TestResult<FindingId> {
        FindingId::try_from_str("f-1").map_err(ctx("finding id"))
    }

    fn read_lines(path: &Path) -> TestResult<Vec<String>> {
        let text = fs::read_to_string(path).map_err(ctx("read export"))?;
        Ok(text.lines().map(str::to_owned).collect())
    }

    #[test]
    fn test_encoded_line_parses_as_the_hub_record() -> TestResult {
        let observed_at: Timestamp = "2026-09-27T10:00:00Z".parse().map_err(ctx("timestamp"))?;
        let line = encode_line(
            &finding_id()?,
            &host()?,
            "structure-drift",
            Severity::High,
            "new workspace member",
            observed_at,
            None,
        )
        .map_err(ctx("encode"))?;

        assert!(line.ends_with('\n'), "one JSON object per line");
        assert_eq!(line.matches('\n').count(), 1, "exactly one newline");

        let parsed: HubDodFinding =
            serde_json::from_str(line.trim_end()).map_err(ctx("hub-shaped parse"))?;
        assert_eq!(parsed.finding_id.as_str(), "f-1");
        assert_eq!(parsed.host.as_str(), "host-a");
        assert_eq!(parsed.rule_id, "structure-drift");
        assert_eq!(parsed.severity, ImpactSeverity::High);
        assert_eq!(parsed.summary, "new workspace member");
        assert_eq!(parsed.observed_at, observed_at);

        let value: serde_json::Value =
            serde_json::from_str(line.trim_end()).map_err(ctx("raw parse"))?;
        assert_eq!(value["severity"], "high", "snake_case severity names");
        assert_eq!(value["observed_at"], "2026-09-27T10:00:00Z");
        Ok(())
    }

    #[test]
    fn test_encoded_line_is_schema_version_2_without_scope_by_default() -> TestResult {
        let line = encode_line(
            &finding_id()?,
            &host()?,
            "structure-drift",
            Severity::High,
            "s",
            Timestamp::UNIX_EPOCH,
            None,
        )
        .map_err(ctx("encode"))?;
        let strict: ExportLineV2 =
            serde_json::from_str(line.trim_end()).map_err(ctx("strict v2 parse"))?;
        assert_eq!(strict.schema_version, EXPORT_SCHEMA_VERSION);
        assert_eq!(strict.schema_version, 2);
        assert_eq!(strict.scope, None);
        let value: serde_json::Value =
            serde_json::from_str(line.trim_end()).map_err(ctx("raw parse"))?;
        assert!(value.get("scope").is_none(), "absent scope is omitted");
        Ok(())
    }

    #[test]
    fn test_encoded_line_carries_an_acceptable_scope() -> TestResult {
        let line = encode_line(
            &finding_id()?,
            &host()?,
            "egress-flow",
            Severity::Critical,
            "s",
            Timestamp::UNIX_EPOCH,
            Some("/sys/fs/cgroup/harw.slice/harw-job-1"),
        )
        .map_err(ctx("encode"))?;
        let strict: ExportLineV2 =
            serde_json::from_str(line.trim_end()).map_err(ctx("strict v2 parse"))?;
        assert_eq!(
            strict.scope.as_deref(),
            Some("/sys/fs/cgroup/harw.slice/harw-job-1")
        );
        Ok(())
    }

    #[test]
    fn test_sanitize_scope_omits_instead_of_rewriting() {
        assert_eq!(sanitize_scope(None), None);
        assert_eq!(sanitize_scope(Some("")), None);
        assert_eq!(sanitize_scope(Some("relative/cgroup")), None);
        assert_eq!(sanitize_scope(Some("/a\nb")), None);
        assert_eq!(sanitize_scope(Some("/a\u{202E}b")), None);
        let long = format!("/{}", "x".repeat(MAX_SCOPE_BYTES));
        assert_eq!(sanitize_scope(Some(&long)), None);
        assert_eq!(sanitize_scope(Some("/harw.slice")), Some("/harw.slice"));
    }

    #[test]
    fn test_v1_and_v2_lines_parse_for_every_known_reader() -> TestResult {
        let v2 = encode_line(
            &finding_id()?,
            &host()?,
            "structure-drift",
            Severity::High,
            "s",
            "2026-09-27T10:00:00Z".parse().map_err(ctx("timestamp"))?,
            Some("/sys/fs/cgroup/harw.slice"),
        )
        .map_err(ctx("encode"))?;
        for raw in [V1_LINE, v2.trim_end()] {
            let hub: HubDodFinding = serde_json::from_str(raw).map_err(ctx("hub reader"))?;
            assert_eq!(hub.rule_id, "structure-drift");
            assert_eq!(hub.severity, ImpactSeverity::High);
            let signals: SecuritySignalsLine =
                serde_json::from_str(raw).map_err(ctx("security_signals reader"))?;
            assert_eq!(signals.rule_id, "structure-drift");
            assert_eq!(signals.severity, "high");
            assert_eq!(signals.observed_at, "2026-09-27T10:00:00Z");
        }
        let v1: ExportLineV2 = serde_json::from_str(V1_LINE).map_err(ctx("v1 via v2 shape"))?;
        assert_eq!(v1.schema_version, 1, "missing schema_version means v1");
        assert_eq!(v1.scope, None);
        assert_eq!(v1.finding_id.as_str(), "f-1");
        assert_eq!(v1.host.as_str(), "host-a");
        assert_eq!(v1.rule_id, "structure-drift");
        assert_eq!(v1.severity, ImpactSeverity::High);
        assert_eq!(v1.summary, "s");
        assert_eq!(
            v1.observed_at,
            "2026-09-27T10:00:00Z"
                .parse::<Timestamp>()
                .map_err(ctx("timestamp"))?
        );
        Ok(())
    }

    #[test]
    fn test_v2_fixture_line_is_byte_identical_to_encode_line() -> TestResult {
        let encoded = encode_line(
            &finding_id()?,
            &host()?,
            "structure-drift",
            Severity::High,
            "s",
            "2026-09-27T10:00:00Z".parse().map_err(ctx("timestamp"))?,
            Some("/sys/fs/cgroup/harw.slice"),
        )
        .map_err(ctx("encode"))?;
        assert_eq!(encoded, format!("{V2_FIXTURE_LINE}\n"));
        let strict: ExportLineV2 =
            serde_json::from_str(V2_FIXTURE_LINE).map_err(ctx("strict v2 parse"))?;
        assert_eq!(strict.schema_version, EXPORT_SCHEMA_VERSION);
        assert_eq!(strict.scope.as_deref(), Some("/sys/fs/cgroup/harw.slice"));
        Ok(())
    }

    #[test]
    fn test_every_severity_maps_to_the_same_named_impact_severity() -> TestResult {
        for (severity, name) in [
            (Severity::Info, "info"),
            (Severity::Low, "low"),
            (Severity::Medium, "medium"),
            (Severity::High, "high"),
            (Severity::Critical, "critical"),
        ] {
            let encoded =
                serde_json::to_string(&impact_severity(severity)).map_err(ctx("encode"))?;
            assert_eq!(encoded, format!("\"{name}\""));
        }
        Ok(())
    }

    #[test]
    fn test_sanitize_summary_replaces_control_and_bidi_characters() {
        let cleaned = sanitize_summary("a\nb\r\tc\u{1b}[31md\u{202E}e\u{0}f");
        assert_eq!(cleaned, "a b  c [31md e f");
        assert!(!cleaned.chars().any(is_unsafe_char));
    }

    #[test]
    fn test_sanitize_summary_caps_at_512_bytes_on_a_char_boundary() {
        let ascii = "x".repeat(2 * MAX_SUMMARY_BYTES);
        assert_eq!(sanitize_summary(&ascii).len(), MAX_SUMMARY_BYTES);

        // 3-Byte-Zeichen: 512 ist nicht durch 3 teilbar, also 510 Bytes.
        let wide = "€".repeat(MAX_SUMMARY_BYTES);
        let cut = sanitize_summary(&wide);
        assert_eq!(cut.len(), 510);
        assert!(cut.chars().all(|ch| ch == '€'));
    }

    #[test]
    fn test_encoded_line_carries_the_sanitized_summary() -> TestResult {
        let line = encode_line(
            &finding_id()?,
            &host()?,
            "structure-drift",
            Severity::Low,
            &format!("evil\n{{\"injected\":1}}{}", "y".repeat(4096)),
            Timestamp::UNIX_EPOCH,
            None,
        )
        .map_err(ctx("encode"))?;
        assert_eq!(line.matches('\n').count(), 1, "no line injection");
        let parsed: HubDodFinding =
            serde_json::from_str(line.trim_end()).map_err(ctx("hub-shaped parse"))?;
        assert!(parsed.summary.len() <= MAX_SUMMARY_BYTES);
        assert!(parsed.summary.starts_with("evil {"));
        Ok(())
    }

    #[test]
    fn test_exporter_appends_one_hub_parsable_line_per_finding() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        let mut exporter = FindingsExporter::new(path.clone(), host()?);

        for rule in ["structure-drift", "egress-flow"] {
            let ok = exporter.export_fields(
                &NullSink,
                &finding_id()?,
                rule,
                Severity::Medium,
                "summary",
                Timestamp::UNIX_EPOCH,
                None,
            );
            assert!(ok);
        }
        assert_eq!(exporter.written(), 2);
        assert_eq!(exporter.failures(), 0);

        let lines = read_lines(&path)?;
        assert_eq!(lines.len(), 2);
        let second: HubDodFinding = serde_json::from_str(&lines[1]).map_err(ctx("second line"))?;
        assert_eq!(second.rule_id, "egress-flow");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_exporter_creates_the_file_with_mode_0640() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        let mut exporter = FindingsExporter::new(path.clone(), host()?);
        exporter.prepare(&NullSink);
        assert_eq!(exporter.failures(), 0);
        let mode = fs::metadata(&path)
            .map_err(ctx("metadata"))?
            .permissions()
            .mode()
            & 0o777;
        // Die Prozess-umask kann höchstens Bits entfernen, nie hinzufügen.
        assert_eq!(mode & !EXPORT_FILE_MODE, 0, "mode {mode:o} exceeds 0640");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_exporter_narrows_an_existing_world_readable_file() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        fs::write(&path, b"").map_err(ctx("seed"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).map_err(ctx("chmod"))?;
        let mut exporter = FindingsExporter::new(path.clone(), host()?);
        exporter.prepare(&NullSink);
        let mode = fs::metadata(&path)
            .map_err(ctx("metadata"))?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o640);
        Ok(())
    }

    #[test]
    fn test_exporter_rotates_to_dot_one_when_the_cap_would_be_exceeded() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        let line_len = encode_line(
            &finding_id()?,
            &host()?,
            "structure-drift",
            Severity::Info,
            "s",
            Timestamp::UNIX_EPOCH,
            None,
        )
        .map_err(ctx("encode"))?
        .len();
        // Platz für genau zwei Zeilen.
        let cap = u64::try_from(2 * line_len).map_err(ctx("cap"))?;
        let mut exporter = FindingsExporter::with_max_bytes(path.clone(), host()?, cap);

        for _ in 0..3 {
            let ok = exporter.export_fields(
                &NullSink,
                &finding_id()?,
                "structure-drift",
                Severity::Info,
                "s",
                Timestamp::UNIX_EPOCH,
                None,
            );
            assert!(ok);
        }

        let rotated = dir.path().join("findings.jsonl.1");
        assert_eq!(read_lines(&rotated)?.len(), 2, "first two lines rotated");
        assert_eq!(read_lines(&path)?.len(), 1, "third line in a fresh file");
        let size = fs::metadata(&path).map_err(ctx("metadata"))?.len();
        assert!(size <= cap);
        assert_eq!(exporter.failures(), 0);
        Ok(())
    }

    #[test]
    fn test_keep_three_generations_shifts_older_files_and_drops_the_oldest() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        let line_len = encode_line(
            &finding_id()?,
            &host()?,
            "structure-drift",
            Severity::Info,
            "s",
            Timestamp::UNIX_EPOCH,
            None,
        )
        .map_err(ctx("encode"))?
        .len();
        // Genau eine Zeile je Datei: jede weitere Zeile rotiert.
        let cap = u64::try_from(line_len).map_err(ctx("cap"))?;
        let mut exporter =
            FindingsExporter::with_max_bytes(path.clone(), host()?, cap).with_keep(3);
        for _ in 0..6 {
            assert!(exporter.export_fields(
                &NullSink,
                &finding_id()?,
                "structure-drift",
                Severity::Info,
                "s",
                Timestamp::UNIX_EPOCH,
                None,
            ));
        }
        for generation in 1..=3 {
            let name = format!("findings.jsonl.{generation}");
            assert_eq!(read_lines(&dir.path().join(&name))?.len(), 1, "{name}");
        }
        assert!(
            !dir.path().join("findings.jsonl.4").exists(),
            "older generations fall out"
        );
        assert_eq!(read_lines(&path)?.len(), 1);
        assert_eq!(exporter.failures(), 0);
        Ok(())
    }

    #[test]
    fn test_keep_is_at_least_one() -> TestResult {
        let exporter = FindingsExporter::new(PathBuf::from("/tmp/x.jsonl"), host()?).with_keep(0);
        assert_eq!(exporter.keep, 1);
        Ok(())
    }

    #[test]
    fn test_default_cap_is_16_mib() {
        assert_eq!(MAX_EXPORT_BYTES, 16 * 1024 * 1024);
    }

    #[test]
    fn test_write_failure_is_counted_not_fatal() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        // Ein Verzeichnis am Zielpfad: jeder Versuch scheitert.
        let path = dir.path().join("is-a-dir");
        fs::create_dir(&path).map_err(ctx("mkdir"))?;
        let mut exporter = FindingsExporter::new(path, host()?);
        exporter.prepare(&NullSink);
        let ok = exporter.export_fields(
            &NullSink,
            &finding_id()?,
            "structure-drift",
            Severity::High,
            "s",
            Timestamp::UNIX_EPOCH,
            None,
        );
        assert!(!ok);
        assert_eq!(exporter.failures(), 2, "prepare + export both counted");
        assert_eq!(exporter.written(), 0);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_symlink_at_export_path_is_refused() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let target = dir.path().join("target");
        fs::write(&target, b"").map_err(ctx("seed target"))?;
        let path = dir.path().join("findings.jsonl");
        std::os::unix::fs::symlink(&target, &path).map_err(ctx("symlink"))?;
        let mut exporter = FindingsExporter::new(path, host()?);
        exporter.prepare(&NullSink);
        assert_eq!(exporter.failures(), 1);
        let untouched = fs::read(&target).map_err(ctx("read target"))?;
        assert!(untouched.is_empty());
        Ok(())
    }

    #[test]
    fn test_resolve_host_prefers_explicit_then_hostname_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let hostname = dir.path().join("hostname");
        fs::write(&hostname, "node-7\n").map_err(ctx("hostname"))?;

        let explicit = host()?;
        let chosen =
            resolve_host(Some(&explicit), &hostname).ok_or(TestError::Missing("explicit host"))?;
        assert_eq!(chosen.as_str(), "host-a");

        let fallback = resolve_host(None, &hostname).ok_or(TestError::Missing("hostname file"))?;
        assert_eq!(fallback.as_str(), "node-7");

        assert!(resolve_host(None, &dir.path().join("missing")).is_none());
        fs::write(&hostname, "  \n").map_err(ctx("blank hostname"))?;
        assert!(resolve_host(None, &hostname).is_none());
        Ok(())
    }

    #[test]
    fn test_rotated_path_appends_dot_one() {
        assert_eq!(
            rotated_path(Path::new("/var/lib/harw-sentinel/findings.jsonl")),
            PathBuf::from("/var/lib/harw-sentinel/findings.jsonl.1")
        );
    }
}
