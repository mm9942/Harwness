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
//! Die Feldtypen sind dieselben, die der Hub deserialisiert:
//! [`harw_types::FindingId`], [`harw_types::HostId`],
//! [`harw_types::ImpactSeverity`] (snake_case) und `jiff::Timestamp`.
//! `harw_dod_signals::Severity` wird über [`impact_severity`] erschöpfend auf
//! `ImpactSeverity` abgebildet — keine String-Umwandlung dazwischen.
//!
//! # Keine Geheimnisse, bereinigter Freitext
//! Exportiert werden ausschließlich die sechs Felder oben — kein Beleg,
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

harw_macros::metrics! {
    /// Wie oft eine Befundzeile seit Prozessstart nicht in die
    /// JSON-Lines-Exportdatei geschrieben werden konnte (Öffnen, Rotation,
    /// Kodierung oder Schreiben). Erwartet: `0`.
    FINDINGS_EXPORT_FAILURE_TOTAL: counter, unit = count, labels = [], cardinality = single,
        name = "harw_sentinel_findings_export_failure_total";
}

/// Eine Exportzeile — Feldnamen und -typen spiegeln exakt
/// `harw_security_hub::findings::DodFinding`.
#[derive(Debug, Serialize)]
struct ExportLine<'a> {
    finding_id: &'a FindingId,
    host: &'a HostId,
    rule_id: &'a str,
    severity: ImpactSeverity,
    summary: String,
    observed_at: Timestamp,
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

/// Kodiert eine Exportzeile inklusive abschließendem `\n`.
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
) -> Result<String, serde_json::Error> {
    let line = ExportLine {
        finding_id,
        host,
        rule_id,
        severity: impact_severity(severity),
        summary: sanitize_summary(summary),
        observed_at,
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
    let mut name = OsString::from(path.as_os_str());
    name.push(".1");
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
            host,
            max_bytes,
            file: None,
            written: 0,
            failures: 0,
        }
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
        self.export_fields(
            sink,
            finding.id(),
            finding.rule_id(),
            finding.severity(),
            finding.summary(),
            finding.observed_at(),
        )
    }

    /// Wie [`Self::export`], aus Einzelfeldern — der eigentliche Schreibweg.
    pub fn export_fields(
        &mut self,
        sink: &dyn TelemetrySink,
        finding_id: &FindingId,
        rule_id: &str,
        severity: Severity,
        summary: &str,
        observed_at: Timestamp,
    ) -> bool {
        let line = match encode_line(
            finding_id,
            &self.host,
            rule_id,
            severity,
            summary,
            observed_at,
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
        match fs::rename(&self.path, &self.rotated) {
            Ok(()) => {
                tracing::info!(
                    path = %self.path.display(),
                    rotated = %self.rotated.display(),
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
    /// Feldsatz, dieselben Typen. `deny_unknown_fields` macht den Test
    /// strenger als den Hub: der Export darf **genau** diese Felder tragen.
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct HubDodFinding {
        finding_id: FindingId,
        host: HostId,
        rule_id: String,
        severity: ImpactSeverity,
        summary: String,
        observed_at: Timestamp,
    }

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
