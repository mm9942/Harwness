//! `/bug-report` — manuell ausgelöster, lokaler Bug-Report.
//!
//! # Crate-Platzierungs-Befund
//! Dieser Typ (`BugReport`) und sein Schreiber (`write_bug_report`,
//! `new_report_id`, `BugReportError`) mussten sowohl von `harw-cli`
//! (`Command::BugReport`) als auch von `harw-ops` (dieser Operation)
//! erreichbar sein. Geprüft: `harw-cli/Cargo.toml` listet
//! `harw-ops = { path = "../harw-ops" }` als Abhängigkeit; `harw-ops/Cargo.toml`
//! listet **keine** Abhängigkeit auf `harw-cli`. Die Abhängigkeitsrichtung ist
//! also `harw-cli → harw-ops`, nicht umgekehrt. Ein Platzieren in
//! `harw-cli/src/bug_report.rs` hätte daher einen (verbotenen) Rückwärts-Import
//! aus `harw-ops` erzwungen. `harw-ops` hängt bereits von `harw-home` ab (siehe
//! `bug_report_dir` unten), also lebt der komplette Speicher-Grundbaustein hier;
//! `harw-cli`s `Command::BugReport`-Handler ruft
//! `harw_ops::bug_report::write_bug_report(...)` auf.
//!
//! # Umfang
//! Rein lokal: schreibt strukturierte Berichte nach
//! `<home>/bug-report/<id>.md`, ohne Netzwerk-Versand — eine Datei je Bericht.
//! Die aufwändigere automatische Incident-Erkennung (gekillte Kinder,
//! erschöpfte Retries, Panics) ist ein separates, noch ausstehendes
//! Arbeitspaket — dieses Modul ist der einfache manuelle Fallback:
//! sowohl über `harw bug-report` (CLI) als auch über `/bug-report` (TUI).
//!
//! Die Operation `/bug-report` schreibt in den an die Sitzung gebundenen
//! Root-Space (`harw_home::ResolvedHomeContext`, siehe
//! `crate::config_util::bound_home`) — nie über `HARW_HOME`; ohne Bindung
//! schlägt sie geschlossen fehl. `harw bug-report` übergibt seinen eigenen
//! Root-Space direkt an [`write_bug_report`].
//!
//! `/bug-report <title> :: <what happened>` — schreibt einen minimalen,
//! manuell ausgelösten Bug-Report als SINGLE-LINE-Befehl (Titel und
//! Beschreibung durch das literale Token `" :: "` getrennt). Ein echter
//! mehrstufiger interaktiver Nachfrage-Dialog bräuchte neue TUI-Overlay-
//! Mechanik, die außerhalb des Umfangs dieser Aufgabe liegt — dieser Befehl
//! ist die einfache manuelle Fläche dafür.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use harw_macros::operation;
use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput};

/// Ein strukturierter Bug-Report, bereit zum Schreiben.
///
/// # Redaction-Vertrag
/// `what_user_said`/`evidence` werden beim Schreiben zusätzlich auf bekannte
/// Secret-Träger geprüft. Aufrufer sollen trotzdem bevorzugt bereits
/// redigierten Text übergeben; unbekannte interne Fehlertexte gehören nicht
/// in einen Report.
pub struct BugReport {
    /// Sortierbare Report-ID, siehe [`new_report_id`].
    pub id: String,
    /// Berichtsart (z. B. `"manual"`).
    pub report_type: String,
    /// Kurztitel des Berichts.
    pub title: String,
    /// Betroffener Bereich/Modul.
    pub area: String,
    /// Beobachteter Fehlermodus.
    pub failure_mode: String,
    /// Optionale Aufgabenkategorie.
    pub task_category: Option<String>,
    /// Freitext-Beschreibung des Vorfalls.
    pub what_happened: String,
    /// Optionaler, bereits redigierter Nutzer-O-Ton.
    pub what_user_said: Option<String>,
    /// Optionale Reproduktionsschritte.
    pub repro: Option<String>,
    /// Optionale, bereits redigierte Beleg-Ausschnitte.
    pub evidence: Option<String>,
}

/// Fehler beim Schreiben eines lokalen Bug-Reports.
#[derive(Debug)]
pub enum BugReportError {
    /// Der Root-Space (`HARW_HOME`/`~`) konnte nicht aufgelöst werden.
    Home(String),
    /// Verzeichnis- oder Schreibfehler beim Anlegen der Report-Datei.
    Io(std::io::Error),
    /// Die Report-ID ist kein sicherer Dateiname.
    InvalidId,
}

impl std::fmt::Display for BugReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Home(msg) => write!(f, "Home nicht auflösbar: {msg}"),
            Self::Io(err) => write!(f, "Bug-Report konnte nicht geschrieben werden: {err}"),
            Self::InvalidId => write!(f, "Bug-Report-ID ist kein sicherer Dateiname"),
        }
    }
}

impl std::error::Error for BugReportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Home(_) => None,
            Self::InvalidId => None,
        }
    }
}

impl From<std::io::Error> for BugReportError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Laufender Zähler für die 4-Hex-Suffix-Eindeutigkeit von [`new_report_id`],
/// falls zwei Aufrufe innerhalb derselben Sekunde und mit identischem
/// `SystemTime::now()`-Hash erfolgen.
static REPORT_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Erzeugt eine neue, sortierbare Report-ID: `bug-YYYYMMDD-HHMMSS-<4-hex>`.
///
/// # Description
/// Die Zeitkomponente wird von Hand aus `SystemTime::now()` (Sekunden seit
/// der Unix-Epoche) in ein UTC-Kalenderdatum umgerechnet (Tage-seit-Epoche-
/// Algorithmus, kein Schaltsekunden-Anspruch). Der 4-Hex-Suffix hasht
/// Systemzeit, Prozess-ID und einen statischen `AtomicU64`-Zähler mit
/// `DefaultHasher`, um Kollisionen bei schneller Wiederholung zu vermeiden.
///
/// # Returns
/// Eine ID der Form `bug-20260917-153000-a1b2`.
#[must_use]
pub fn new_report_id() -> String {
    let now = SystemTime::now();
    let since_epoch = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let total_secs = since_epoch.as_secs();
    let days_since_epoch = total_secs / 86_400;
    let secs_of_day = total_secs % 86_400;
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;

    let (year, month, day) = civil_from_days(days_since_epoch as i64);

    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let counter = REPORT_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut hasher = DefaultHasher::new();
    now.hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    counter.hash(&mut hasher);
    let suffix = (hasher.finish() & 0xFFFF) as u16;

    format!("bug-{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}-{suffix:04x}")
}

/// Rechnet Tage seit der Unix-Epoche (1970-01-01) in ein UTC-Kalenderdatum
/// `(Jahr, Monat, Tag)` um.
///
/// # Description
/// Implementiert Howard Hinnants bekannten "days_from_civil"-Umkehralgorithmus
/// (proleptischer Gregorianischer Kalender), korrekt für alle Schaltjahre.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

/// Schreibt `report` als Markdown nach `<home>/bug-report/<id>.md`.
///
/// # Errors
/// [`BugReportError::Io`] bei Verzeichnis-/Schreibfehlern.
pub fn write_bug_report(home: &Path, report: &BugReport) -> Result<PathBuf, BugReportError> {
    write_bug_report_with_retention(home, report, &harw_retention::RetentionConfig::default())
}

/// Zeitbudget für das Aufräumen nach dem Schreiben (Klasse `bug_reports`).
const PRUNE_BUDGET: std::time::Duration = std::time::Duration::from_millis(200);

/// Wie [`write_bug_report`], begrenzt danach aber das Verzeichnis über die
/// Retention-Klasse `bug_reports` aus `retention` (Alter/Anzahl/Bytes).
///
/// Das Aufräumen läuft erst nach erfolgreichem Schreiben, ist zeitbegrenzt
/// und best effort: ein Fehler dort lässt das Schreiben nie scheitern, und
/// der soeben geschriebene Report ist der neueste und bleibt (`keep_newest`).
///
/// # Errors
/// [`BugReportError::Io`] bei Verzeichnis-/Schreibfehlern; [`BugReportError::InvalidId`].
pub fn write_bug_report_with_retention(
    home: &Path,
    report: &BugReport,
    retention: &harw_retention::RetentionConfig,
) -> Result<PathBuf, BugReportError> {
    let path = write_bug_report_file(home, report)?;
    prune_bug_reports(home, retention);
    Ok(path)
}

fn prune_bug_reports(home: &Path, retention: &harw_retention::RetentionConfig) {
    let Some(class) = harw_retention::policy_for(retention, "bug_reports") else {
        return;
    };
    if !class.enabled {
        return;
    }
    let roots = harw_retention::Roots {
        home: home.to_path_buf(),
        project: None,
    };
    let _ = class.sweep(
        &roots,
        harw_retention::SweepMode::Apply,
        Some(std::time::Instant::now() + PRUNE_BUDGET),
        SystemTime::now(),
    );
}

fn write_bug_report_file(home: &Path, report: &BugReport) -> Result<PathBuf, BugReportError> {
    let dir = harw_home::paths::bug_report_dir(home);
    std::fs::create_dir_all(&dir)?;
    if report.id.is_empty()
        || !report
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(BugReportError::InvalidId);
    }
    let path = dir.join(format!("{}.md", report.id));

    let task_category = redact_text(report.task_category.as_deref().unwrap_or("-"));
    let what_user_said = redact_text(report.what_user_said.as_deref().unwrap_or("-"));
    let repro = redact_text(report.repro.as_deref().unwrap_or("-"));
    let evidence = redact_text(report.evidence.as_deref().unwrap_or("-"));
    let title = redact_text(&report.title);
    let report_type = redact_text(&report.report_type);
    let area = redact_text(&report.area);
    let failure_mode = redact_text(&report.failure_mode);
    let what_happened = redact_text(&report.what_happened);

    let mut body = String::new();
    // Infallible: writing to a String never returns Err.
    let _ = write!(
        body,
        "# {title}\n\
         Type: {report_type}\n\
         Area: {area}\n\
         Failure mode: {failure_mode}\n\
         Task category: {task_category}\n\
         \n\
         ## What happened\n\
         {what_happened}\n\
         \n\
         ## What user said\n\
         {what_user_said}\n\
         \n\
         ## Repro\n\
         {repro}\n\
         \n\
         ## Evidence\n\
         {evidence}\n",
        title = title,
        report_type = report_type,
        area = area,
        failure_mode = failure_mode,
        what_happened = what_happened,
    );

    // Schreibe erst vollständig in eine exklusiv angelegte Datei. `hard_link`
    // ist hier absichtlich statt `rename` verwendet: auf Unix würde `rename`
    // ein bereits vorhandenes Ziel atomar überschreiben.
    let temp_path = dir.join(format!(
        ".{}.{}.tmp",
        report.id,
        REPORT_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut temp = options.open(&temp_path)?;
    let result = (|| {
        temp.write_all(body.as_bytes())?;
        temp.sync_all()?;
        std::fs::hard_link(&temp_path, &path)?;
        Ok::<(), std::io::Error>(())
    })();
    drop(temp);
    let _ = std::fs::remove_file(&temp_path);
    result?;
    Ok(path)
}

/// Entfernt Steuerzeichen und maskiert bekannte Secret-Träger aus frei
/// eingegebenem Text. Die betroffene Zeile wird vollständig maskiert, damit
/// der geheime Wert nicht versehentlich im Markdown verbleibt.
fn redact_text(value: &str) -> String {
    value
        .lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            let sensitive = [
                "authorization:",
                "api-key:",
                "x-api-key:",
                "api_key=",
                "api-key=",
                "access_token=",
                "password=",
                "secret=",
                "bearer ",
                "sk-",
                "ghp_",
                "glpat-",
                "xoxb-",
            ]
            .iter()
            .any(|marker| lower.contains(marker));
            if sensitive {
                "[REDACTED]".to_owned()
            } else {
                line.chars()
                    .filter(|ch| !ch.is_control() || *ch == '\t')
                    .collect()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Argumente für `/bug-report <title> :: <what happened>`.
#[derive(Default, serde::Deserialize)]
pub struct BugReportArgs {
    /// Kurztitel des Berichts, `None` wenn kein Argument übergeben wurde.
    pub title: Option<String>,
    /// Freitext-Beschreibung; leer, wenn kein `" :: "`-Trenner gefunden wurde.
    #[serde(default)]
    pub what_happened: String,
}

impl FromRawArgs for BugReportArgs {
    /// Parst `"<title> :: <what happened>"`; ohne `" :: "`-Trenner wird der
    /// gesamte Text als Titel behandelt und `what_happened` bleibt leer
    /// (die Operation verlangt dann eine erneute Eingabe mit Trenner).
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        if tokens.is_empty() {
            return Ok(Self::default());
        }
        let raw = tokens.join(" ");
        match raw.split_once(" :: ") {
            Some((title, what_happened)) => Ok(Self {
                title: Some(title.trim().to_owned()),
                what_happened: what_happened.trim().to_owned(),
            }),
            None => Ok(Self {
                title: Some(raw.trim().to_owned()),
                what_happened: String::new(),
            }),
        }
    }
}

/// `/bug-report`-Operation — siehe Moduldoku für die minimale manuelle UX.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: kein Titel angegeben, oder Titel ohne
///   `" :: <what happened>"`-Teil.
/// - [`OpError::NotAvailable`]: an die Sitzung ist kein Root-Space gebunden
///   (kein Rückfall auf `HARW_HOME`).
/// - [`OpError::Execution`][]: Schreibfehler.
#[operation(
    name = "bug-report",
    summary = "Speichert einen lokalen Bug-Report unter ~/.harw/bug-report/.",
    domain = "misc",
    permission = "operator",
    command(path = "/bug-report", visibility = "tui_only")
)]
async fn bug_report(ctx: &OpContext, args: BugReportArgs) -> Result<OpOutput, OpError> {
    let Some(title) = args.title.clone().filter(|t| !t.is_empty()) else {
        return Err(OpError::InvalidArguments(
            "usage: /bug-report <title> :: <what happened>".into(),
        ));
    };
    if args.what_happened.is_empty() {
        return Err(OpError::InvalidArguments(
            "usage: /bug-report <title> :: <what happened> (missing ' :: ' separator)".into(),
        ));
    }

    let home = crate::config_util::bound_home(ctx)?.home.clone();
    let report = BugReport {
        id: new_report_id(),
        report_type: "manual".to_owned(),
        title,
        area: "unspecified".to_owned(),
        failure_mode: "unspecified".to_owned(),
        task_category: None,
        what_happened: args.what_happened,
        what_user_said: None,
        repro: None,
        evidence: None,
    };

    let path = write_bug_report(&home, &report).map_err(|e| OpError::Execution(e.to_string()))?;
    Ok(OpOutput::from(format!(
        "Bug report saved: {}",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_operations::context::ServiceMap;

    fn sample_report(id: &str) -> BugReport {
        BugReport {
            id: id.to_owned(),
            report_type: "manual".to_owned(),
            title: "Something broke".to_owned(),
            area: "harw-cli".to_owned(),
            failure_mode: "panic".to_owned(),
            task_category: Some("coding".to_owned()),
            what_happened: "The process crashed while parsing input.".to_owned(),
            what_user_said: Some("It just died.".to_owned()),
            repro: Some("Run `harw chat` then type a long message.".to_owned()),
            evidence: Some("stderr: thread panicked at ...".to_owned()),
        }
    }

    #[test]
    fn write_bug_report_creates_file_with_expected_sections() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = tmp.path();
        let report = sample_report("bug-test-0001");

        let path = write_bug_report(home, &report).map_err(ctx("write succeeds"))?;
        assert!(path.exists());
        assert_eq!(path, home.join("bug-report").join("bug-test-0001.md"));

        let content = std::fs::read_to_string(&path).map_err(ctx("read back"))?;
        assert!(content.contains("# Something broke"));
        assert!(content.contains("## What happened"));
        assert!(content.contains("## What user said"));
        assert!(content.contains("## Repro"));
        assert!(content.contains("## Evidence"));
        assert!(content.contains("The process crashed while parsing input."));
        assert!(content.contains("It just died."));
        assert!(content.contains("Run `harw chat` then type a long message."));
        assert!(content.contains("stderr: thread panicked at ..."));
        assert!(content.contains("Task category: coding"));
        Ok(())
    }

    #[test]
    fn write_bug_report_caps_directory_and_keeps_newest() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut cfg = harw_retention::RetentionConfig::default();
        cfg.bug_reports.max_files = Some(3);
        let dir = harw_home::paths::bug_report_dir(tmp.path());
        std::fs::create_dir_all(&dir).map_err(ctx("mkdir"))?;
        let old = SystemTime::now() - std::time::Duration::from_secs(3_600);
        for n in 0..6_u64 {
            let file = std::fs::File::create(dir.join(format!("old-{n}.md")))
                .map_err(ctx("create old"))?;
            file.set_modified(old + std::time::Duration::from_secs(n))
                .map_err(ctx("mtime"))?;
        }
        let report = sample_report("bug-new-0001");
        let path = write_bug_report_with_retention(tmp.path(), &report, &cfg)
            .map_err(ctx("write succeeds"))?;
        assert!(path.exists(), "the fresh report must survive pruning");
        let count = std::fs::read_dir(&dir)
            .map_err(ctx("readdir"))?
            .flatten()
            .count();
        assert_eq!(count, 3);
        assert!(dir.join("old-5.md").exists());
        assert!(!dir.join("old-0.md").exists());
        Ok(())
    }

    #[test]
    fn write_bug_report_handles_none_optional_fields() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = tmp.path();
        let report = BugReport {
            id: "bug-test-0002".to_owned(),
            report_type: "manual".to_owned(),
            title: "Minimal report".to_owned(),
            area: "harw-ops".to_owned(),
            failure_mode: "unknown".to_owned(),
            task_category: None,
            what_happened: "Something odd happened.".to_owned(),
            what_user_said: None,
            repro: None,
            evidence: None,
        };

        let path =
            write_bug_report(home, &report).map_err(ctx("write succeeds with None fields"))?;
        let content = std::fs::read_to_string(&path).map_err(ctx("read back"))?;
        assert!(content.contains("Task category: -"));
        assert!(content.contains("## What user said\n-"));
        assert!(content.contains("## Repro\n-"));
        assert!(content.contains("## Evidence\n-"));
        Ok(())
    }

    #[test]
    fn write_bug_report_is_redacted_and_does_not_overwrite() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut report = sample_report("bug-test-safe");
        report.what_happened = "Authorization: Bearer super-secret\nnormal detail".to_owned();
        report.evidence = Some("api_key=sk-secret-value".to_owned());

        let path = write_bug_report(tmp.path(), &report).map_err(ctx("write succeeds"))?;
        let original = std::fs::read_to_string(&path).map_err(ctx("read back"))?;
        assert!(!original.contains("super-secret"));
        assert!(!original.contains("sk-secret-value"));
        assert!(original.contains("normal detail"));

        let mut replacement = report;
        replacement.what_happened = "replacement".to_owned();
        assert!(write_bug_report(tmp.path(), &replacement).is_err());
        assert_eq!(
            std::fs::read_to_string(path).map_err(ctx("read original"))?,
            original
        );
        Ok(())
    }

    #[test]
    fn write_bug_report_rejects_path_like_ids() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut report = sample_report("../escape");
        assert!(matches!(
            write_bug_report(tmp.path(), &report),
            Err(BugReportError::InvalidId)
        ));
        report.id = "nested/name".to_owned();
        assert!(matches!(
            write_bug_report(tmp.path(), &report),
            Err(BugReportError::InvalidId)
        ));
        Ok(())
    }

    #[test]
    fn two_reports_get_distinct_ids() {
        let first = new_report_id();
        let second = new_report_id();
        assert_ne!(first, second);
    }

    /// Argumente wie aus `/bug-report Titel :: Text`.
    fn op_args() -> TestResult<BugReportArgs> {
        let tokens = ["Titel", "::", "Text"].map(str::to_owned);
        BugReportArgs::from_raw_args(&tokens).map_err(ctx("parse bug-report args"))
    }

    /// Die Operation schreibt in den an die Sitzung gebundenen Root-Space —
    /// nie über `HARW_HOME`.
    #[tokio::test]
    async fn bug_report_op_writes_under_the_bound_home() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut services = ServiceMap::new();
        services.insert(crate::config_util::test_home_context(temp.path())?);
        let op_ctx = crate::knowledge_test_support::op_context(services)?;

        let output = super::bug_report(&op_ctx, op_args()?)
            .await
            .map_err(ctx("bug-report op"))?;

        let entries = std::fs::read_dir(temp.path().join("bug-report"))
            .map_err(ctx("read bound bug-report dir"))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(ctx("list bound bug-report dir"))?;
        let reports = entries
            .iter()
            .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
            .count();
        assert_eq!(reports, 1, "{entries:?}");
        assert!(
            output.text.starts_with("Bug report saved: "),
            "{}",
            output.text
        );
        Ok(())
    }

    /// Ohne gebundenen Root-Space ist die Operation nicht verfügbar, statt
    /// auf den Prozess-Root-Space auszuweichen.
    #[tokio::test]
    async fn bug_report_op_without_bound_home_is_not_available() -> TestResult {
        let op_ctx = crate::knowledge_test_support::op_context(ServiceMap::new())?;

        match super::bug_report(&op_ctx, op_args()?).await {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("Root-Space"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable, got: {other:?}"
                )));
            }
        }
        Ok(())
    }
}
