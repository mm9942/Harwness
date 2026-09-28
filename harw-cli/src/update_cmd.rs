//! `harw update`: neue Version prüfen, installieren, beim Start darauf hinweisen.
//!
//! # Ablauf
//! 1. **Prüfen.** `GET <api>/releases/latest` (Standard: das GitHub-Repository,
//!    überschreibbar mit `HARW_UPDATE_API`). Das Ergebnis landet in
//!    `~/.harw/version.json` ([`harw_install::UpdateChecker`]); ein früheres
//!    `--dismiss` bleibt erhalten.
//! 2. **Release zuerst.** Gibt es eine neuere Release, lädt `harw update`
//!    `harw-<tag>-<target>.tar.gz` und `SHA256SUMS`, prüft die Prüfsumme,
//!    entpackt mit dem System-`tar` in ein Staging-Verzeichnis neben den
//!    Binaries und ersetzt `harw`, `killer` und (falls enthalten)
//!    `harw-agent-runner` per `rename`. Die vorige Fassung bleibt als
//!    `<name>.old` liegen.
//! 3. **Sonst Quell-Update.** Ohne veröffentlichte Release baut
//!    `harw update` aus den Quellen, die `make install` in
//!    `~/.harw/install.toml` vermerkt hat: ein Git-Checkout wird mit
//!    `git pull --ff-only` aktualisiert und mit `make install` installiert,
//!    ein Quellarchiv über `scripts/install.sh` neu geladen und gebaut.
//! 4. **Hinweis beim Start.** [`on_start`] liest beim TUI-Start nur
//!    `version.json` (kein Netz im Vordergrund) und zeigt eine neuere,
//!    nicht verworfene Version an. Ist die letzte Prüfung älter als
//!    [`harw_install::update::DEFAULT_TTL_SECS`] (20 Stunden), startet es
//!    `harw update --check` losgelöst im Hintergrund. `HARW_NO_UPDATE_CHECK`
//!    schaltet beides ab.
//!
//! # Exit-Codes
//! `harw update --check` endet mit [`UPDATE_AVAILABLE_EXIT`] (10), wenn eine
//! neuere Release bereitsteht, sonst mit 0; Fehler enden wie jeder andere
//! Befehl mit 2.
//!
//! # Nebenläufigkeit
//! Synchron; der Netzzugriff läuft auf einer eigenen Single-Thread-Runtime.
//! Nicht aus einer laufenden Tokio-Runtime heraus aufrufen.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use harw_agent_compiler::env::{InstallRecord, host_target};
use harw_install::release::{
    CHECKSUMS_FILE, ReleaseInfo, is_newer, parse_latest_release, tarball_name, update_notice,
    verify_sha256sums,
};
use harw_install::update::DEFAULT_TTL_SECS;
use harw_install::{UpdateChecker, VersionInfo};

use crate::home::resolve_home;

/// Standard-Endpunkt für die neueste Release.
pub const RELEASE_API_DEFAULT: &str =
    "https://api.github.com/repos/mm9942/Harwness/releases/latest";

/// Überschreibt [`RELEASE_API_DEFAULT`] (Forks, Spiegel, Tests).
pub const RELEASE_API_ENV: &str = "HARW_UPDATE_API";

/// Schaltet Hinweis und Hintergrundprüfung beim Start ab.
pub const NO_UPDATE_CHECK_ENV: &str = "HARW_NO_UPDATE_CHECK";

/// Exit-Code von `harw update --check`, wenn eine neuere Release bereitsteht.
pub const UPDATE_AVAILABLE_EXIT: i32 = 10;

/// Die installierte Version.
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Zeitlimit für Prüfung und Download im Vordergrund.
const FOREGROUND_TIMEOUT: Duration = Duration::from_secs(120);

/// Obergrenze eines heruntergeladenen Release-Assets.
const MAX_ASSET_BYTES: usize = 512 * 1024 * 1024;

/// Die Binaries eines Release-Tarballs; `true` = muss enthalten sein.
/// `killer` braucht Linux-procfs und pidfd und fehlt in Android-Releases.
const RELEASE_BINARIES: [(&str, bool); 3] = [
    ("harw", true),
    ("killer", cfg!(target_os = "linux")),
    ("harw-agent-runner", false),
];

/// Die Flags von `harw update`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UpdateArgs {
    /// Nur prüfen und Stand anzeigen, nichts installieren.
    pub check: bool,
    /// Ohne Rückfrage installieren.
    pub yes: bool,
    /// Den Hinweis auf die bekannte neueste Version ausblenden.
    pub dismiss: bool,
}

/// Ergebnis einer Prüfung.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Check {
    /// Die neueste Release ist nicht neuer als die installierte Version.
    UpToDate(String),
    /// Eine neuere Release steht bereit.
    Available(ReleaseInfo),
    /// Es gibt keine veröffentlichte Release.
    NoRelease,
}

/// `harw update [--check] [--yes] [--dismiss]`.
///
/// # Errors
/// Netzwerk-, Prüfsummen-, Datei- und Build-Fehler als deutsche Meldung.
/// `--check` beendet den Prozess mit [`UPDATE_AVAILABLE_EXIT`], wenn eine
/// neuere Release bereitsteht.
pub fn run(home_override: Option<PathBuf>, args: UpdateArgs) -> Result<(), String> {
    let home = resolve_home(home_override.clone())?;
    let checker = UpdateChecker::new(&home);
    if args.dismiss {
        return dismiss(&checker);
    }
    let api = std::env::var(RELEASE_API_ENV).unwrap_or_else(|_| RELEASE_API_DEFAULT.to_owned());
    let outcome = check(&checker, &api, FOREGROUND_TIMEOUT)?;
    match &outcome {
        Check::UpToDate(latest) => {
            println!("harw {CURRENT_VERSION} ist aktuell (neueste Release: {latest}).");
        }
        Check::Available(release) => println!(
            "harw {} ist verfügbar (installiert: {CURRENT_VERSION}).",
            release.version
        ),
        Check::NoRelease => println!(
            "Es gibt noch keine veröffentlichte Release; installiert ist {CURRENT_VERSION}."
        ),
    }
    if args.check {
        if matches!(outcome, Check::Available(_)) {
            std::process::exit(UPDATE_AVAILABLE_EXIT);
        }
        return Ok(());
    }
    match outcome {
        Check::UpToDate(_) => Ok(()),
        Check::Available(release) => {
            if !confirm(
                &format!("harw {} aus der Release installieren?", release.version),
                args.yes,
            )? {
                return Ok(());
            }
            install_release(&home, home_override.as_deref(), &release)?;
            println!("harw {} installiert.", release.version);
            Ok(())
        }
        Check::NoRelease => {
            let record = InstallRecord::read(&home).map_err(|error| error.to_string())?;
            let plan = source_plan(record.as_ref())?;
            if !confirm(
                &format!("Aus den Quellen aktualisieren ({plan})?"),
                args.yes,
            )? {
                return Ok(());
            }
            let bindir = bindir_for(record.as_ref(), std::env::current_exe().ok().as_deref())?;
            run_source_update(&plan, &bindir, &home)
        }
    }
}

/// Liest beim TUI-Start den gespeicherten Stand, zeigt eine neuere Version an
/// und startet bei Bedarf eine Hintergrundprüfung. Blockiert nie, schlägt nie
/// fehl.
pub fn on_start(home_override: Option<PathBuf>) {
    if std::env::var_os(NO_UPDATE_CHECK_ENV).is_some() {
        return;
    }
    let Ok(home) = resolve_home(home_override.clone()) else {
        return;
    };
    let checker = UpdateChecker::new(&home);
    if let Ok(Some(info)) = checker.read() {
        if let Some(notice) = update_notice(&info, CURRENT_VERSION) {
            eprintln!("harw: {notice}");
        }
    }
    if !checker.is_stale(jiff::Timestamp::now(), DEFAULT_TTL_SECS) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut command = Command::new(exe);
    if let Some(home) = home_override {
        command.arg("--home").arg(home);
    }
    command
        .args(["update", "--check"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match command.spawn() {
        Ok(mut child) => {
            tracing::debug!(pid = child.id(), "update.check.spawned");
            // Das Kind endet von selbst; harw kann vorher beenden.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(error) => tracing::debug!(%error, "update.check.spawn_failed"),
    }
}

fn dismiss(checker: &UpdateChecker) -> Result<(), String> {
    let info = checker
        .read()
        .map_err(|error| error.to_string())?
        .ok_or("Noch keine Version bekannt: erst `harw update --check` ausführen.")?;
    checker
        .dismiss(&info.latest_version)
        .map_err(|error| error.to_string())?;
    println!(
        "Hinweis auf harw {} ausgeblendet; eine spätere Version wird wieder angezeigt.",
        info.latest_version
    );
    Ok(())
}

/// Fragt die neueste Release ab und hält das Ergebnis in `version.json` fest.
fn check(checker: &UpdateChecker, api: &str, timeout: Duration) -> Result<Check, String> {
    let body = http_get(api, timeout, true)?;
    let outcome = match body {
        None => Check::NoRelease,
        Some(body) => {
            let release = parse_latest_release(&body).map_err(|error| error.to_string())?;
            if is_newer(CURRENT_VERSION, &release.version) {
                Check::Available(release)
            } else {
                Check::UpToDate(release.version)
            }
        }
    };
    let latest = match &outcome {
        Check::Available(release) => release.version.clone(),
        Check::UpToDate(latest) => latest.clone(),
        Check::NoRelease => CURRENT_VERSION.to_owned(),
    };
    let previous = checker.read().ok().flatten();
    checker
        .write(&recorded(previous, latest, jiff::Timestamp::now()))
        .map_err(|error| error.to_string())?;
    Ok(outcome)
}

/// Der neue Stand von `version.json`: die neueste Version und der
/// Prüfzeitpunkt; ein früheres Verwerfen bleibt erhalten.
fn recorded(previous: Option<VersionInfo>, latest: String, now: jiff::Timestamp) -> VersionInfo {
    VersionInfo {
        latest_version: latest,
        last_checked_at: now,
        dismissed_version: previous.and_then(|info| info.dismissed_version),
    }
}

/// `GET url`; mit `not_found_is_none` wird `404` zu `Ok(None)`.
fn http_get(
    url: &str,
    timeout: Duration,
    not_found_is_none: bool,
) -> Result<Option<Vec<u8>>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("Tokio-Runtime nicht verfügbar: {error}"))?;
    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .user_agent(format!("harw/{CURRENT_VERSION}"))
            .timeout(timeout)
            .build()
            .map_err(|error| format!("HTTP-Client nicht verfügbar: {error}"))?;
        let response = client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|error| format!("{url} nicht erreichbar: {error}"))?;
        let status = response.status();
        if not_found_is_none && status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(format!("{url} antwortet mit {status}"));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_ASSET_BYTES as u64)
        {
            return Err(format!("{url} ist größer als {MAX_ASSET_BYTES} Bytes"));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| format!("{url} nicht vollständig geladen: {error}"))?;
        if bytes.len() > MAX_ASSET_BYTES {
            return Err(format!("{url} ist größer als {MAX_ASSET_BYTES} Bytes"));
        }
        Ok(Some(bytes.to_vec()))
    })
}

/// Fragt nach, sofern nicht `--yes`; ohne Terminal ist `--yes` nötig.
fn confirm(question: &str, yes: bool) -> Result<bool, String> {
    if yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(format!(
            "{question} — ohne Terminal bitte `harw update --yes` verwenden."
        ));
    }
    print!("{question} [j/N] ");
    std::io::stdout()
        .flush()
        .map_err(|error| error.to_string())?;
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|error| error.to_string())?;
    Ok(matches!(answer.trim(), "j" | "J" | "ja" | "y" | "yes"))
}

/// Wohin installiert wird: das `bindir` aus `install.toml`, sonst das
/// Verzeichnis des laufenden `harw`.
fn bindir_for(
    record: Option<&InstallRecord>,
    current_exe: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(bindir) = record.and_then(|record| record.bindir.clone()) {
        return Ok(bindir);
    }
    current_exe
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "Installationsverzeichnis von harw nicht bestimmbar".to_owned())
}

/// Lädt, prüft und installiert die Release.
fn install_release(
    home: &Path,
    home_override: Option<&Path>,
    release: &ReleaseInfo,
) -> Result<(), String> {
    let target = host_target();
    let name = tarball_name(&release.tag, &target);
    let tarball_asset = release.asset(&name).map_err(|error| error.to_string())?;
    let sums_asset = release
        .asset(CHECKSUMS_FILE)
        .map_err(|error| error.to_string())?;
    let record = InstallRecord::read(home).map_err(|error| error.to_string())?;
    let bindir = bindir_for(record.as_ref(), std::env::current_exe().ok().as_deref())?;

    let sums = http_get(&sums_asset.url, FOREGROUND_TIMEOUT, false)?.ok_or("SHA256SUMS fehlt")?;
    let sums = String::from_utf8(sums).map_err(|_| "SHA256SUMS ist kein Text".to_owned())?;
    println!("Lade {name} …");
    let tarball = http_get(&tarball_asset.url, FOREGROUND_TIMEOUT, false)?
        .ok_or_else(|| format!("{name} fehlt"))?;
    verify_sha256sums(&sums, &name, &tarball).map_err(|error| error.to_string())?;
    // Die Prüfsumme stammt aus derselben Release und macht die Einträge
    // nicht harmlos: vor `tar` jeden Eintrag prüfen (nur Dateien und
    // Verzeichnisse unter `harw-<tag>-<ziel>/`, kein `..`, keine Links).
    let root = name.strip_suffix(".tar.gz").unwrap_or(&name);
    harw_install::validate_release_archive(&tarball, root).map_err(|error| error.to_string())?;

    let staging = Staging::create(&bindir)?;
    let archive = staging.path().join(&name);
    std::fs::write(&archive, &tarball)
        .map_err(|error| format!("{} nicht schreibbar: {error}", archive.display()))?;
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(staging.path())
        .status()
        .map_err(|error| format!("tar nicht ausführbar: {error}"))?;
    if !status.success() {
        return Err(format!("tar konnte {name} nicht entpacken ({status})"));
    }
    let unpacked = unpacked_root(staging.path(), &name);
    let mut installed = Vec::new();
    for (binary, required) in RELEASE_BINARIES {
        let staged = unpacked.join(binary);
        if !staged.is_file() {
            if required {
                return Err(format!("{name} enthält `{binary}` nicht; nichts ersetzt"));
            }
            continue;
        }
        installed.push((binary, staged));
    }
    for (binary, staged) in &installed {
        replace_binary(staged, &bindir.join(binary))?;
    }
    if installed
        .iter()
        .any(|(binary, _)| *binary == "harw-agent-runner")
    {
        let runner = home
            .join("bin/.runners")
            .join(&target)
            .join(&release.version)
            .join("harw-agent-runner");
        if let Some(parent) = runner.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("{} nicht anlegbar: {error}", parent.display()))?;
        }
        std::fs::copy(bindir.join("harw-agent-runner"), &runner).map_err(|error| {
            format!("Runner-Kopie {} fehlgeschlagen: {error}", runner.display())
        })?;
    }
    refresh_install_record(&bindir, home_override, record.as_ref());
    Ok(())
}

/// Wo die Binaries nach dem Entpacken liegen: `release.yml` packt sie in
/// ein Verzeichnis `harw-<tag>-<target>/` (der Tarball-Name ohne
/// `.tar.gz`); ein flacher Tarball wird ebenso angenommen.
fn unpacked_root(staging: &Path, tarball: &str) -> PathBuf {
    let nested = staging.join(tarball.trim_end_matches(".tar.gz"));
    if nested.is_dir() {
        nested
    } else {
        staging.to_path_buf()
    }
}

/// Ersetzt `dest` durch `staged` per `rename` und behält die vorige Fassung
/// als `<dest>.old`.
///
/// # Description
/// Beide Pfade liegen im selben Verzeichnis, `rename` ist dort atomar. Schlägt
/// das Einsetzen fehl, wird die vorige Fassung zurückbenannt.
fn replace_binary(staged: &Path, dest: &Path) -> Result<(), String> {
    set_executable(staged)?;
    let mut old = dest.as_os_str().to_owned();
    old.push(".old");
    let old = PathBuf::from(old);
    let had_previous = dest.exists();
    if had_previous {
        std::fs::rename(dest, &old)
            .map_err(|error| format!("{} nicht sicherbar: {error}", dest.display()))?;
    }
    if let Err(error) = std::fs::rename(staged, dest) {
        if had_previous {
            let _ = std::fs::rename(&old, dest);
        }
        return Err(format!("{} nicht ersetzbar: {error}", dest.display()));
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|error| format!("{} nicht ausführbar setzbar: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Lässt das neue `harw` sein `install.toml` schreiben (neue Version, Ziel,
/// bisheriges Quellverzeichnis). Ein Fehlschlag ist nur eine Warnung.
fn refresh_install_record(
    bindir: &Path,
    home_override: Option<&Path>,
    record: Option<&InstallRecord>,
) {
    let mut command = Command::new(bindir.join("harw"));
    if let Some(home) = home_override {
        command.arg("--home").arg(home);
    }
    command
        .args(["agent", "install-record", "--bindir"])
        .arg(bindir);
    if let Some(source) = record.and_then(|record| record.source_dir.as_ref()) {
        command.arg("--source-dir").arg(source);
    }
    match command.status() {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("harw: install.toml nicht aktualisiert ({status})"),
        Err(error) => eprintln!("harw: install.toml nicht aktualisiert: {error}"),
    }
}

/// Wie ein Quell-Update abläuft.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SourcePlan {
    /// Git-Checkout: `git pull --ff-only`, dann `make install`.
    Git(PathBuf),
    /// Quellarchiv: `scripts/install.sh` lädt das aktuelle Archiv und baut.
    Installer(PathBuf),
}

impl std::fmt::Display for SourcePlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Git(dir) => write!(f, "git pull + make install in {}", dir.display()),
            Self::Installer(dir) => {
                write!(
                    f,
                    "Quellarchiv neu laden über {}/scripts/install.sh",
                    dir.display()
                )
            }
        }
    }
}

/// Wählt den Weg des Quell-Updates aus `install.toml`.
fn source_plan(record: Option<&InstallRecord>) -> Result<SourcePlan, String> {
    let source = record.and_then(|record| record.source_dir.clone()).ok_or(
        "Keine Release veröffentlicht und kein Quellverzeichnis bekannt \
             (install.toml fehlt). Aus den Quellen mit `make install` installieren.",
    )?;
    if source.join(".git").exists() {
        Ok(SourcePlan::Git(source))
    } else if source.join("scripts/install.sh").is_file() {
        Ok(SourcePlan::Installer(source))
    } else {
        Err(format!(
            "{} ist weder ein Git-Checkout noch enthält es scripts/install.sh",
            source.display()
        ))
    }
}

fn run_source_update(plan: &SourcePlan, bindir: &Path, home: &Path) -> Result<(), String> {
    match plan {
        SourcePlan::Git(dir) => {
            run_step(
                Command::new("git")
                    .arg("-C")
                    .arg(dir)
                    .args(["pull", "--ff-only"]),
            )?;
            run_step(
                Command::new("make")
                    .arg("-C")
                    .arg(dir)
                    .arg("install")
                    .arg(format!("BINDIR={}", bindir.display()))
                    .env("HARW_HOME", home),
            )
        }
        SourcePlan::Installer(dir) => run_step(
            Command::new("bash")
                .arg(dir.join("scripts/install.sh"))
                .env("HARW_INSTALL_DIR", bindir)
                .env("HARW_HOME", home),
        ),
    }
}

fn run_step(command: &mut Command) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("{command:?} nicht ausführbar: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{command:?} fehlgeschlagen ({status})"))
    }
}

/// Staging-Verzeichnis neben den Binaries; wird beim Verlassen entfernt.
struct Staging(PathBuf);

impl Staging {
    fn create(bindir: &Path) -> Result<Self, String> {
        let path = bindir.join(format!(".harw-update-{}", std::process::id()));
        if path.exists() {
            std::fs::remove_dir_all(&path)
                .map_err(|error| format!("{} nicht leerbar: {error}", path.display()))?;
        }
        std::fs::create_dir_all(&path)
            .map_err(|error| format!("{} nicht anlegbar: {error}", path.display()))?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn record(source_dir: Option<PathBuf>, bindir: Option<PathBuf>) -> InstallRecord {
        InstallRecord::new(source_dir, bindir)
    }

    #[test]
    fn test_replace_binary_keeps_previous_as_old() -> TestResult {
        let dir = tempfile::tempdir()?;
        let dest = dir.path().join("harw");
        let staged = dir.path().join("harw.new");
        std::fs::write(&dest, b"old")?;
        std::fs::write(&staged, b"new")?;
        replace_binary(&staged, &dest)?;
        assert_eq!(std::fs::read(&dest)?, b"new");
        assert_eq!(std::fs::read(dir.path().join("harw.old"))?, b"old");
        assert!(!staged.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&dest)?.permissions().mode() & 0o777,
                0o755
            );
        }
        Ok(())
    }

    #[test]
    fn test_replace_binary_installs_without_previous() -> TestResult {
        let dir = tempfile::tempdir()?;
        let dest = dir.path().join("killer");
        let staged = dir.path().join("killer.new");
        std::fs::write(&staged, b"new")?;
        replace_binary(&staged, &dest)?;
        assert_eq!(std::fs::read(&dest)?, b"new");
        assert!(!dir.path().join("killer.old").exists());
        Ok(())
    }

    #[test]
    fn test_replace_binary_restores_previous_on_failure() -> TestResult {
        let dir = tempfile::tempdir()?;
        let dest = dir.path().join("harw");
        std::fs::write(&dest, b"old")?;
        // Kein Staging-Datei: das Einsetzen scheitert, die alte Fassung bleibt.
        let missing = dir.path().join("does-not-exist");
        assert!(replace_binary(&missing, &dest).is_err());
        assert_eq!(std::fs::read(&dest)?, b"old");
        Ok(())
    }

    #[test]
    fn test_source_plan_prefers_git_then_installer() -> TestResult {
        let git = tempfile::tempdir()?;
        std::fs::create_dir(git.path().join(".git"))?;
        assert_eq!(
            source_plan(Some(&record(Some(git.path().to_path_buf()), None)))?,
            SourcePlan::Git(git.path().to_path_buf())
        );
        let archive = tempfile::tempdir()?;
        std::fs::create_dir(archive.path().join("scripts"))?;
        std::fs::write(archive.path().join("scripts/install.sh"), b"#!/bin/sh\n")?;
        assert_eq!(
            source_plan(Some(&record(Some(archive.path().to_path_buf()), None)))?,
            SourcePlan::Installer(archive.path().to_path_buf())
        );
        let empty = tempfile::tempdir()?;
        assert!(source_plan(Some(&record(Some(empty.path().to_path_buf()), None))).is_err());
        assert!(source_plan(Some(&record(None, None))).is_err());
        assert!(source_plan(None).is_err());
        Ok(())
    }

    #[test]
    fn test_bindir_prefers_install_record_then_current_exe() -> TestResult {
        let from_record = record(None, Some(PathBuf::from("/opt/harw/bin")));
        assert_eq!(
            bindir_for(Some(&from_record), Some(Path::new("/usr/bin/harw")))?,
            PathBuf::from("/opt/harw/bin")
        );
        assert_eq!(
            bindir_for(Some(&record(None, None)), Some(Path::new("/usr/bin/harw")))?,
            PathBuf::from("/usr/bin")
        );
        assert!(bindir_for(None, None).is_err());
        Ok(())
    }

    #[test]
    fn test_recorded_keeps_dismissed_version() -> TestResult {
        let now = jiff::Timestamp::from_second(1_790_000_000)?;
        let previous = VersionInfo {
            latest_version: "0.9.0".to_owned(),
            last_checked_at: now,
            dismissed_version: Some("0.9.0".to_owned()),
        };
        let next = recorded(Some(previous), "0.9.1".to_owned(), now);
        assert_eq!(next.latest_version, "0.9.1");
        assert_eq!(next.dismissed_version.as_deref(), Some("0.9.0"));
        assert_eq!(
            recorded(None, "0.9.1".to_owned(), now).dismissed_version,
            None
        );
        Ok(())
    }

    /// Serves exactly one HTTP response on a local port and returns its URL.
    fn serve_once(
        status: &'static str,
        body: String,
    ) -> Result<String, Box<dyn std::error::Error>> {
        use std::io::{Read, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}/releases/latest", listener.local_addr()?);
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Ok(url)
    }

    fn release_body(tag: &str) -> String {
        serde_json::json!({
            "tag_name": tag,
            "draft": false,
            "prerelease": false,
            "assets": []
        })
        .to_string()
    }

    #[test]
    fn test_check_records_newer_release_as_available() -> TestResult {
        let home = tempfile::tempdir()?;
        let checker = UpdateChecker::new(home.path());
        let url = serve_once("200 OK", release_body("v99.0.0"))?;
        match check(&checker, &url, Duration::from_secs(10))? {
            Check::Available(release) => assert_eq!(release.version, "99.0.0"),
            other => return Err(format!("expected Available, got {other:?}").into()),
        }
        let info = checker.read()?.ok_or("version.json missing")?;
        assert_eq!(info.latest_version, "99.0.0");
        Ok(())
    }

    #[test]
    fn test_check_treats_older_release_as_up_to_date() -> TestResult {
        let home = tempfile::tempdir()?;
        let checker = UpdateChecker::new(home.path());
        let url = serve_once("200 OK", release_body("v0.0.1"))?;
        assert_eq!(
            check(&checker, &url, Duration::from_secs(10))?,
            Check::UpToDate("0.0.1".to_owned())
        );
        Ok(())
    }

    #[test]
    fn test_check_without_release_records_current_version() -> TestResult {
        let home = tempfile::tempdir()?;
        let checker = UpdateChecker::new(home.path());
        let url = serve_once("404 Not Found", "{}".to_owned())?;
        assert_eq!(
            check(&checker, &url, Duration::from_secs(10))?,
            Check::NoRelease
        );
        let info = checker.read()?.ok_or("version.json missing")?;
        assert_eq!(info.latest_version, CURRENT_VERSION);
        Ok(())
    }

    #[test]
    fn test_check_failure_leaves_version_state_unchanged() -> TestResult {
        let home = tempfile::tempdir()?;
        let checker = UpdateChecker::new(home.path());
        let url = serve_once("500 Internal Server Error", "{}".to_owned())?;
        assert!(check(&checker, &url, Duration::from_secs(10)).is_err());
        assert!(checker.read()?.is_none());
        Ok(())
    }

    #[test]
    fn test_unpacked_root_prefers_release_directory() -> TestResult {
        let dir = tempfile::tempdir()?;
        let name = "harw-v0.9.0-x86_64-unknown-linux-gnu.tar.gz";
        assert_eq!(unpacked_root(dir.path(), name), dir.path());
        let nested = dir.path().join("harw-v0.9.0-x86_64-unknown-linux-gnu");
        std::fs::create_dir(&nested)?;
        assert_eq!(unpacked_root(dir.path(), name), nested);
        Ok(())
    }

    #[test]
    fn test_staging_is_removed_on_drop() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = {
            let staging = Staging::create(dir.path())?;
            std::fs::write(staging.path().join("x"), b"x")?;
            staging.path().to_path_buf()
        };
        assert!(!path.exists());
        Ok(())
    }
}
