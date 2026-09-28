//! `harw auth migrate` — Klartext-Secret-Referenzen in den verschlüsselten
//! SecretStore überführen.
//!
//! Umfang: `auth` in `<home>/providers/*.toml` und `<profile>/providers/*.toml`
//! sowie jeder `credential_pool`-Eintrag in `<home>/auth.toml` und
//! `<profile>/auth.toml`. `file:`- und `file-json:`-Verweise (auch Dateien
//! externer CLIs) und `env:`-Verweise werden in den Store kopiert und auf
//! `secrets:<id>` umgeschrieben; die Umgebungsvariable selbst bleibt
//! unberührt. `keyring:`-Verweise und Codex-Login-Verweise
//! ([`harw_provider_http::is_codex_login_reference`]; sie schalten die
//! Codex-Route und deren Refresh frei) bleiben unverändert und werden
//! gemeldet. `credentials`-Einträge in `auth.toml` werden nicht migriert,
//! zählen aber als verbleibende Verweise.
//!
//! Regeln: erst speichern, dann umschreiben, dann löschen; ein Fehler bricht
//! vor dem nächsten Schritt ab (fail closed). Gelöscht werden nur von harw
//! selbst angelegte Klartextdateien unter `<home>/secrets/` (`*.key`,
//! `*-oauth.token`), die kein verbleibender Verweis mehr nennt; jede andere
//! Quelle bleibt bestehen und wird gemeldet (Dateien externer CLIs als
//! Schnappschuss, der deren Refresh nicht folgt). `--dry-run` öffnet weder
//! Store noch Config-Writer und ändert nichts.
//!
//! Ausgegeben werden Dateien, Fundstellen und Verweise — nie ein Wert.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use harw_config::{AuthConfig, ConfigWriter, CredentialEntry, ProviderToml, ResolvedConfig, SecretRef};
use secrecy::{ExposeSecret as _, SecretString};

/// Obergrenze einer Klartext-Quelldatei (64 KiB).
const MAX_SOURCE_LEN: usize = 64 * 1024;

/// Zweck der migrierten Store-Einträge.
const MIGRATED_PURPOSE: &str = "provider authentication (migrated)";

/// Dateinamen-Endungen, die harw selbst unter `<home>/secrets/` schreibt
/// (Onboarding-Schlüssel und `harw_oauth::save_token`).
const HARW_SECRET_SUFFIXES: [&str; 2] = [".key", "-oauth.token"];

/// Hinweis für Quellen außerhalb von `<home>/secrets/`.
const SNAPSHOT_NOTE: &str = "Quelle bleibt (Schnappschuss; folgt keinem Refresh des Tools)";

/// Platzhalter für die noch nicht vergebene ID im Probelauf.
const PLANNED_REF: &str = "secrets:<neu>";

/// Welche Konfigurationsdateien eine Migration betrachtet.
pub(crate) struct MigrationScope<'a> {
    /// Root-Space (`<home>`).
    pub(crate) home: &'a Path,
    /// Verzeichnis des aktiven Profils.
    pub(crate) profile: &'a Path,
    /// Env-Layer aus den `.env`-Dateien; nur gelesen, nie verändert.
    pub(crate) env_layer: &'a BTreeMap<String, String>,
}

/// Ergebnis einer Migration (oder eines Probelaufs). Enthält keine Werte.
#[derive(Debug, Default)]
pub(crate) struct MigrationReport {
    /// Menschenlesbare Zeilen: Datei, Fundstelle, alter und neuer Verweis.
    pub(crate) lines: Vec<String>,
    /// Anzahl neu verschlüsselt gespeicherter Secrets.
    pub(crate) stored: usize,
    /// Umgeschriebene Konfigurationsdateien.
    pub(crate) rewritten_files: Vec<PathBuf>,
    /// Gelöschte, von harw angelegte Klartextdateien.
    pub(crate) deleted: Vec<PathBuf>,
    /// Quelldateien, die bestehen bleiben.
    pub(crate) kept_sources: Vec<PathBuf>,
}

/// Führt `harw auth migrate [--dry-run]` für das Home `home` aus.
///
/// # Errors
/// Config-Auflösung, ungültiger Profilname oder ein Fehler der Migration
/// (siehe [`migrate`]). Die Meldung nennt nie einen Wert.
pub(crate) fn run(home: &Path, dry_run: bool) -> Result<(), String> {
    let layers = harw_home::config_layers(home).map_err(|error| error.to_string())?;
    let config = harw_config::discover_config(&layers).map_err(|error| error.to_string())?;
    let profile = harw_home::profile_dir(home, &harw_home::active_profile_name(home))
        .map_err(|error| error.to_string())?;
    let scope = MigrationScope {
        home,
        profile: &profile,
        env_layer: &config.env_layer,
    };
    let report = migrate(&scope, &config, dry_run)?;
    for line in &report.lines {
        println!("{line}");
    }
    if dry_run {
        println!("Probelauf: nichts geändert.");
    } else {
        println!(
            "{} Secret(s) verschlüsselt gespeichert, {} Datei(en) umgeschrieben, \
             {} Klartextdatei(en) gelöscht, {} Quelle(n) bleiben bestehen.",
            report.stored,
            report.rewritten_files.len(),
            report.deleted.len(),
            report.kept_sources.len()
        );
    }
    Ok(())
}

/// Fundstelle eines Verweises in einer Konfigurationsdatei.
enum Location {
    /// `auth` einer `providers/*.toml`.
    ProviderAuth,
    /// Eintrag `index` in `credential_pool.<provider>` einer `auth.toml`.
    Pool { provider: String, index: usize },
}

/// Ein gefundener Verweis samt Herkunft.
struct Occurrence {
    file: PathBuf,
    location: Location,
    /// Providername für den Store-Namen `migrated-<provider>`.
    provider: String,
    reference: SecretRef,
}

impl Occurrence {
    /// `<datei>: <fundstelle>: <verweis>` — ohne Wert.
    fn describe(&self) -> String {
        let location = match &self.location {
            Location::ProviderAuth => "auth".to_owned(),
            Location::Pool { provider, index } => format!("credential_pool.{provider}[{index}]"),
        };
        format!("{}: {location}: {}", self.file.display(), self.reference)
    }
}

/// Ergebnis des Einlesens aller Dateien im Umfang.
#[derive(Default)]
struct Scan {
    occurrences: Vec<Occurrence>,
    /// Geparste `auth.toml`-Dateien, Grundlage für das Umschreiben der Pools.
    auth_files: Vec<(PathBuf, AuthConfig)>,
    /// `credentials`-Werte: werden nicht migriert, zählen aber als Verweise.
    credentials: Vec<SecretRef>,
    /// Anzahl nicht lesbarer Dateien; dann wird nichts gelöscht.
    unreadable_files: usize,
}

/// Ein zu migrierender Wert (eine Quelle je eindeutigem Verweis-String).
struct Source {
    value: SecretString,
    name: String,
    reference: SecretRef,
}

/// Was nach dem Umschreiben mit einer Quelldatei geschieht.
enum Disposition {
    Delete(PathBuf),
    Keep(PathBuf, &'static str),
}

/// Plant und (ohne `dry_run`) führt die Migration aus.
///
/// # Description
/// Reihenfolge ohne `dry_run`: (1) alle Werte über einen einzigen Writer
/// ([`crate::secret_store::open_secret_store_for_writing`]) speichern,
/// (2) Konfigurationsdateien umschreiben, (3) nicht mehr referenzierte, von
/// harw angelegte Klartextdateien löschen. Mit `dry_run` wird nur geplant:
/// kein Store (also kein KEK-Bootstrap), kein Config-Writer, keine Löschung.
///
/// # Errors
/// Nicht lesbares Verzeichnis im Umfang, Store-Fehler (vor jedem
/// Umschreiben) oder Schreibfehler einer Konfigurationsdatei (vor jeder
/// Löschung). Meldungen nennen Dateien und Verweise, nie Werte.
pub(crate) fn migrate(
    scope: &MigrationScope<'_>,
    config: &ResolvedConfig,
    dry_run: bool,
) -> Result<MigrationReport, String> {
    let mut report = MigrationReport::default();
    let scan = scan(scope, &mut report)?;

    // Klassifizieren: was bleibt, was wird kopiert.
    let mut sources: BTreeMap<String, Source> = BTreeMap::new();
    let mut failed: HashMap<String, &'static str> = HashMap::new();
    let mut migratable: Vec<&Occurrence> = Vec::new();
    let mut remaining: Vec<SecretRef> = scan.credentials.clone();
    for occurrence in &scan.occurrences {
        let reference = &occurrence.reference;
        if matches!(reference, SecretRef::Secrets(_)) {
            continue;
        }
        if matches!(reference, SecretRef::Keyring(_)) {
            report
                .lines
                .push(format!("{} — keyring: bleibt", occurrence.describe()));
            remaining.push(reference.clone());
            continue;
        }
        if harw_provider_http::is_codex_login_reference(reference) {
            report.lines.push(format!(
                "{} — Codex-Login-Verweis bleibt (Codex-Route und Refresh)",
                occurrence.describe()
            ));
            remaining.push(reference.clone());
            continue;
        }
        if let Location::Pool { provider, .. } = &occurrence.location {
            if provider.contains('.') {
                report.lines.push(format!(
                    "{} — bleibt (Providername mit '.' kann nicht umgeschrieben werden)",
                    occurrence.describe()
                ));
                remaining.push(reference.clone());
                continue;
            }
        }
        let key = reference.as_ref_string();
        if sources.contains_key(&key) {
            migratable.push(occurrence);
            continue;
        }
        if let Some(reason) = failed.get(&key) {
            report
                .lines
                .push(format!("{} — bleibt ({reason})", occurrence.describe()));
            remaining.push(reference.clone());
            continue;
        }
        match read_source_value(reference, scope.env_layer) {
            Ok(value) => {
                sources.insert(
                    key,
                    Source {
                        value,
                        name: format!("migrated-{}", occurrence.provider),
                        reference: reference.clone(),
                    },
                );
                migratable.push(occurrence);
            }
            Err(reason) => {
                failed.insert(key, reason);
                report
                    .lines
                    .push(format!("{} — bleibt ({reason})", occurrence.describe()));
                remaining.push(reference.clone());
            }
        }
    }

    let dispositions = plan_dispositions(scope.home, &sources, &remaining, scan.unreadable_files);

    if dry_run {
        report.lines.push("Plan, nichts geändert:".to_owned());
        for occurrence in &migratable {
            report
                .lines
                .push(format!("{} -> {PLANNED_REF}", occurrence.describe()));
        }
        for disposition in dispositions {
            match disposition {
                Disposition::Delete(path) => report
                    .lines
                    .push(format!("{}: würde gelöscht", path.display())),
                Disposition::Keep(path, note) => {
                    report.lines.push(format!("{}: {note}", path.display()));
                    report.kept_sources.push(path);
                }
            }
        }
        return Ok(report);
    }

    if sources.is_empty() {
        report.lines.push("Nichts zu migrieren.".to_owned());
        return Ok(report);
    }

    // (1) Speichern. Noch ist keine Datei umgeschrieben.
    let mut writer = crate::secret_store::open_secret_store_for_writing(scope.home, config)
        .map_err(|error| {
            format!("SecretStore konnte nicht geöffnet werden: {error}; nichts wurde umgeschrieben")
        })?;
    let mut new_refs: HashMap<String, SecretRef> = HashMap::new();
    for (key, source) in &sources {
        let new_ref = writer
            .store(&source.name, MIGRATED_PURPOSE, &source.value)
            .map_err(|error| {
                let kept = if report.stored == 0 {
                    String::new()
                } else {
                    format!(
                        "; {} bereits gespeicherte(r) Datensatz/Datensätze bleiben \
                         verschlüsselt und ungenutzt",
                        report.stored
                    )
                };
                format!(
                    "{} konnte nicht gespeichert werden: {error}{kept}; nichts wurde umgeschrieben",
                    source.reference
                )
            })?;
        report.stored += 1;
        new_refs.insert(key.clone(), new_ref);
    }

    // (2) Umschreiben. Ein Fehler bricht vor jeder Löschung ab.
    rewrite_files(&scan, &migratable, &new_refs, &mut report)?;

    // (3) Löschen bzw. Quellen melden.
    for disposition in dispositions {
        match disposition {
            Disposition::Delete(path) => match fs::remove_file(&path) {
                Ok(()) => {
                    report.lines.push(format!("{}: gelöscht", path.display()));
                    report.deleted.push(path);
                }
                Err(_) => {
                    report.lines.push(format!(
                        "{}: konnte nicht gelöscht werden (Klartext bleibt, bitte manuell entfernen)",
                        path.display()
                    ));
                    report.kept_sources.push(path);
                }
            },
            Disposition::Keep(path, note) => {
                report.lines.push(format!("{}: {note}", path.display()));
                report.kept_sources.push(path);
            }
        }
    }
    Ok(report)
}

/// Liest alle Dateien im Umfang ein. Nicht parsebare Dateien werden
/// gemeldet und übersprungen.
fn scan(scope: &MigrationScope<'_>, report: &mut MigrationReport) -> Result<Scan, String> {
    let mut scan = Scan::default();

    let mut provider_files = toml_files(&scope.home.join("providers"))?;
    provider_files.extend(toml_files(&scope.profile.join("providers"))?);
    dedup_paths(&mut provider_files);
    for path in provider_files {
        let parsed = fs::read_to_string(&path)
            .ok()
            .and_then(|content| toml::from_str::<ProviderToml>(&content).ok());
        let Some(provider) = parsed else {
            report.lines.push(format!(
                "{}: nicht lesbar oder ungültig, übersprungen",
                path.display()
            ));
            scan.unreadable_files += 1;
            continue;
        };
        if let Some(reference) = provider.auth {
            scan.occurrences.push(Occurrence {
                file: path,
                location: Location::ProviderAuth,
                provider: provider.name,
                reference,
            });
        }
    }

    let mut auth_files = vec![
        harw_home::auth_path(scope.home),
        harw_home::auth_path(scope.profile),
    ];
    dedup_paths(&mut auth_files);
    for path in auth_files {
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                report
                    .lines
                    .push(format!("{}: nicht lesbar, übersprungen", path.display()));
                scan.unreadable_files += 1;
                continue;
            }
        };
        let Ok(auth) = toml::from_str::<AuthConfig>(&content) else {
            report
                .lines
                .push(format!("{}: ungültig, übersprungen", path.display()));
            scan.unreadable_files += 1;
            continue;
        };
        scan.credentials.extend(auth.credentials.values().cloned());
        let mut providers: Vec<&String> = auth.credential_pool.keys().collect();
        providers.sort();
        for provider in providers {
            let Some(entries) = auth.credential_pool.get(provider) else {
                continue;
            };
            for (index, entry) in entries.iter().enumerate() {
                scan.occurrences.push(Occurrence {
                    file: path.clone(),
                    location: Location::Pool {
                        provider: provider.clone(),
                        index,
                    },
                    provider: provider.clone(),
                    reference: entry.secret.clone(),
                });
            }
        }
        scan.auth_files.push((path, auth));
    }
    Ok(scan)
}

/// Alle `*.toml`-Dateien in `dir`, sortiert; ein fehlendes Verzeichnis ist
/// leer.
fn toml_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(format!("'{}' konnte nicht gelesen werden", dir.display())),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| format!("'{}' konnte nicht gelesen werden", dir.display()))?;
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "toml") && path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Entfernt doppelte Pfade unter Erhalt der Reihenfolge.
fn dedup_paths(paths: &mut Vec<PathBuf>) {
    let mut seen = BTreeSet::new();
    paths.retain(|path| seen.insert(normalized_source_path(path)));
}

/// Liest den Wert eines migrierbaren Verweises. Der Fehler ist ein fester
/// Grund, nie Inhalt.
fn read_source_value(
    reference: &SecretRef,
    env_layer: &BTreeMap<String, String>,
) -> Result<SecretString, &'static str> {
    match reference {
        SecretRef::Env(name) => harw_config::resolve_env_ref(name, env_layer)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map(SecretString::from)
            .ok_or("Variable nicht gesetzt oder leer"),
        SecretRef::File(path) => read_plaintext_source(Path::new(path)),
        SecretRef::FileJson { path, pointer } => {
            let raw = read_plaintext_source(Path::new(path))?;
            let document: serde_json::Value = serde_json::from_str(raw.expose_secret())
                .map_err(|_| "Quelle ist kein gültiges JSON")?;
            document
                .pointer(pointer)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| SecretString::from(value.to_owned()))
                .ok_or("JSON-Pointer liefert keinen nicht-leeren String")
        }
        SecretRef::Keyring(_) | SecretRef::Secrets(_) => Err("Verweis wird nicht migriert"),
    }
}

/// Strenger Leser für Klartext-Quelldateien.
///
/// Verlangt einen absoluten Pfad auf eine reguläre Datei (kein Symlink), die
/// nur für den Besitzer lesbar ist (`mode & 0o077 == 0`), höchstens 64 KiB
/// groß und UTF-8 ist. Liefert den getrimmten Inhalt.
fn read_plaintext_source(path: &Path) -> Result<SecretString, &'static str> {
    if !path.is_absolute() {
        return Err("Quelle ist kein absoluter Pfad");
    }
    let before = fs::symlink_metadata(path).map_err(|_| "Quelle nicht lesbar")?;
    if before.file_type().is_symlink() {
        return Err("Quelle ist ein Symlink");
    }
    if !before.is_file() {
        return Err("Quelle ist keine reguläre Datei");
    }
    ensure_private_mode(&before)?;
    if before.len() > MAX_SOURCE_LEN as u64 {
        return Err("Quelle ist größer als 64 KiB");
    }
    let file = fs::File::open(path).map_err(|_| "Quelle nicht lesbar")?;
    let opened = file.metadata().map_err(|_| "Quelle nicht lesbar")?;
    ensure_same_file(&before, &opened)?;
    let mut bytes = Vec::new();
    file.take(MAX_SOURCE_LEN as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Quelle nicht lesbar")?;
    if bytes.len() > MAX_SOURCE_LEN {
        return Err("Quelle ist größer als 64 KiB");
    }
    let text =
        SecretString::from(String::from_utf8(bytes).map_err(|_| "Quelle ist kein UTF-8")?);
    let trimmed = text.expose_secret().trim();
    if trimmed.is_empty() {
        return Err("Quelle ist leer");
    }
    Ok(SecretString::from(trimmed.to_owned()))
}

#[cfg(unix)]
fn ensure_private_mode(metadata: &fs::Metadata) -> Result<(), &'static str> {
    use std::os::unix::fs::PermissionsExt as _;
    if metadata.permissions().mode() & 0o077 == 0 {
        Ok(())
    } else {
        Err("Quelle ist für Gruppe oder andere zugänglich (erwartet 0600)")
    }
}

#[cfg(not(unix))]
fn ensure_private_mode(_metadata: &fs::Metadata) -> Result<(), &'static str> {
    Err("Dateirechte der Quelle sind auf dieser Plattform nicht prüfbar")
}

#[cfg(unix)]
fn ensure_same_file(before: &fs::Metadata, opened: &fs::Metadata) -> Result<(), &'static str> {
    use std::os::unix::fs::MetadataExt as _;
    if before.dev() == opened.dev() && before.ino() == opened.ino() {
        Ok(())
    } else {
        Err("Quelle wurde während des Lesens ersetzt")
    }
}

#[cfg(not(unix))]
fn ensure_same_file(_before: &fs::Metadata, _opened: &fs::Metadata) -> Result<(), &'static str> {
    Ok(())
}

/// Pfad der Quelldatei eines `file:`/`file-json:`-Verweises.
fn source_path(reference: &SecretRef) -> Option<&Path> {
    match reference {
        SecretRef::File(path) | SecretRef::FileJson { path, .. } => Some(Path::new(path)),
        SecretRef::Env(_) | SecretRef::Keyring(_) | SecretRef::Secrets(_) => None,
    }
}

/// Pfad mit kanonischem Elternverzeichnis (der Dateiname selbst wird nicht
/// aufgelöst, damit ein Symlink nicht als sein Ziel gilt).
fn normalized_source_path(path: &Path) -> PathBuf {
    let parent = path.parent().and_then(|parent| parent.canonicalize().ok());
    match (parent, path.file_name()) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

/// `true`, wenn `reference` eine von harw selbst angelegte Klartextdatei
/// direkt unter `<home>/secrets/` nennt (`file:`, `*.key`/`*-oauth.token`,
/// kein Symlink).
fn is_harw_plaintext_file(reference: &SecretRef, home_secrets: &Path) -> bool {
    let SecretRef::File(raw) = reference else {
        return false;
    };
    let path = Path::new(raw);
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !HARW_SECRET_SUFFIXES
        .iter()
        .any(|suffix| name.ends_with(suffix))
    {
        return false;
    }
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return false;
    }
    path.parent()
        .and_then(|parent| parent.canonicalize().ok())
        .is_some_and(|parent| parent == home_secrets)
}

/// Legt für jede Quelldatei fest, ob sie nach dem Umschreiben gelöscht wird
/// oder bestehen bleibt (mit Begründung).
fn plan_dispositions(
    home: &Path,
    sources: &BTreeMap<String, Source>,
    remaining: &[SecretRef],
    unreadable_files: usize,
) -> Vec<Disposition> {
    let home_secrets = home.join("secrets").canonicalize().ok();
    let remaining_paths: BTreeSet<PathBuf> = remaining
        .iter()
        .filter_map(source_path)
        .map(normalized_source_path)
        .collect();
    let mut seen = BTreeSet::new();
    let mut dispositions = Vec::new();
    for source in sources.values() {
        let Some(raw_path) = source_path(&source.reference) else {
            continue;
        };
        let path = normalized_source_path(raw_path);
        if !seen.insert(path.clone()) {
            continue;
        }
        let inside_home_secrets = home_secrets
            .as_deref()
            .is_some_and(|secrets| path.parent() == Some(secrets));
        let disposition = match home_secrets.as_deref() {
            _ if !inside_home_secrets => Disposition::Keep(path, SNAPSHOT_NOTE),
            Some(secrets) if is_harw_plaintext_file(&source.reference, secrets) => {
                if remaining_paths.contains(&path) {
                    Disposition::Keep(path, "Quelle bleibt (noch referenziert)")
                } else if unreadable_files > 0 {
                    Disposition::Keep(
                        path,
                        "Quelle bleibt (nicht lesbare Konfigurationsdatei im Umfang)",
                    )
                } else {
                    Disposition::Delete(path)
                }
            }
            _ => Disposition::Keep(path, "Quelle bleibt (nicht von harw angelegt)"),
        };
        dispositions.push(disposition);
    }
    dispositions
}

/// Schreibt Provider-Dateien und `credential_pool`-Tabellen auf die neuen
/// `secrets:`-Verweise um.
fn rewrite_files(
    scan: &Scan,
    migratable: &[&Occurrence],
    new_refs: &HashMap<String, SecretRef>,
    report: &mut MigrationReport,
) -> Result<(), String> {
    let write_error = |path: &Path| {
        format!(
            "'{}' konnte nicht geschrieben werden; bereits umgeschriebene Dateien \
             verweisen auf gültige secrets:-Einträge, nichts wurde gelöscht",
            path.display()
        )
    };

    let mut pools: BTreeMap<&Path, BTreeSet<&str>> = BTreeMap::new();
    for occurrence in migratable {
        let Some(new_ref) = new_refs.get(&occurrence.reference.as_ref_string()) else {
            continue;
        };
        match &occurrence.location {
            Location::ProviderAuth => {
                let path = occurrence.file.as_path();
                let mut writer = ConfigWriter::open(path).map_err(|_| write_error(path))?;
                writer
                    .set_value("auth", toml_edit::value(new_ref.as_ref_string()))
                    .map_err(|_| write_error(path))?;
                writer.save().map_err(|_| write_error(path))?;
                report.rewritten_files.push(path.to_path_buf());
            }
            Location::Pool { provider, .. } => {
                pools
                    .entry(occurrence.file.as_path())
                    .or_default()
                    .insert(provider.as_str());
            }
        }
        report
            .lines
            .push(format!("{} -> {new_ref}", occurrence.describe()));
    }

    for (path, providers) in pools {
        let Some((_, auth)) = scan.auth_files.iter().find(|(file, _)| file == path) else {
            continue;
        };
        let mut writer = ConfigWriter::open(path).map_err(|_| write_error(path))?;
        for provider in providers {
            let Some(entries) = auth.credential_pool.get(provider) else {
                continue;
            };
            writer
                .set_value(
                    &format!("credential_pool.{provider}"),
                    toml_edit::Item::ArrayOfTables(pool_tables(entries, new_refs)),
                )
                .map_err(|_| write_error(path))?;
        }
        writer.save().map_err(|_| write_error(path))?;
        report.rewritten_files.push(path.to_path_buf());
    }
    Ok(())
}

/// Baut `[[credential_pool.<provider>]]` aus den typisierten Einträgen neu;
/// migrierte Verweise werden ersetzt, alle anderen bleiben.
fn pool_tables(
    entries: &[CredentialEntry],
    new_refs: &HashMap<String, SecretRef>,
) -> toml_edit::ArrayOfTables {
    let mut tables = toml_edit::ArrayOfTables::new();
    for entry in entries {
        let secret = new_refs
            .get(&entry.secret.as_ref_string())
            .unwrap_or(&entry.secret);
        let mut table = toml_edit::Table::new();
        table.insert("secret", toml_edit::value(secret.as_ref_string()));
        if let Some(label) = &entry.label {
            table.insert("label", toml_edit::value(label.as_str()));
        }
        table.insert("priority", toml_edit::value(i64::from(entry.priority)));
        if let Some(base_url) = &entry.base_url {
            table.insert("base_url", toml_edit::value(base_url.as_str()));
        }
        tables.push(table);
    }
    tables
}

#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};

    use harw_config::{AuthConfig, CredentialEntry, ProviderToml, ResolvedConfig, SecretRef};
    use harw_provider_http::SecretResolver as _;
    use secrecy::ExposeSecret as _;
    use tempfile::TempDir;

    use super::{MigrationReport, MigrationScope, migrate};
    use crate::test_support::{TestError, TestResult, ctx, some_or};

    /// Temporäres Home (von Hand, ohne Katalog-Seed), Profilverzeichnis
    /// des aktiven Profils und ein Verzeichnis außerhalb des Homes.
    struct Fixture {
        home: TempDir,
        outside: TempDir,
        profile: PathBuf,
    }

    impl Fixture {
        fn new() -> TestResult<Self> {
            let home = TempDir::new().map_err(ctx("temporary home"))?;
            let outside = TempDir::new().map_err(ctx("temporary outside dir"))?;
            let profile = harw_home::profile_dir(
                home.path(),
                &harw_home::active_profile_name(home.path()),
            )
            .map_err(ctx("profile dir"))?;
            fs::create_dir_all(&profile).map_err(ctx("create profile dir"))?;
            Ok(Self {
                home,
                outside,
                profile,
            })
        }

        fn home(&self) -> &Path {
            self.home.path()
        }

        fn run(
            &self,
            env_layer: &BTreeMap<String, String>,
            dry_run: bool,
        ) -> TestResult<MigrationReport> {
            let scope = MigrationScope {
                home: self.home(),
                profile: &self.profile,
                env_layer,
            };
            migrate(&scope, &ResolvedConfig::default(), dry_run)
                .map_err(TestError::Unexpected)
        }
    }

    /// Schreibt `content` nach `path` (Elternverzeichnisse anlegen) mit
    /// Modus `mode`.
    fn write_file(path: &Path, content: &str, mode: u32) -> TestResult {
        let parent = some_or(path.parent(), "parent dir")?;
        fs::create_dir_all(parent).map_err(ctx("create parent dir"))?;
        fs::write(path, content).map_err(ctx("write file"))?;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(ctx("chmod file"))
    }

    fn provider_toml(name: &str, auth: &str) -> String {
        format!(
            "name = \"{name}\"\napi = \"openai-compatible\"\nbase_url = \"https://example.test\"\nauth = \"{auth}\"\n"
        )
    }

    fn read_provider_auth(path: &Path) -> TestResult<SecretRef> {
        let content = fs::read_to_string(path).map_err(ctx("read provider file"))?;
        let provider: ProviderToml = toml::from_str(&content).map_err(ctx("parse provider file"))?;
        some_or(provider.auth, "provider auth")
    }

    fn read_pool(path: &Path, provider: &str) -> TestResult<Vec<CredentialEntry>> {
        let content = fs::read_to_string(path).map_err(ctx("read auth.toml"))?;
        let auth: AuthConfig = toml::from_str(&content).map_err(ctx("parse auth.toml"))?;
        some_or(auth.credential_pool.get(provider).cloned(), "credential pool")
    }

    /// Löst `reference` über den konfigurierten Resolver auf; der KEK kommt
    /// aus der umgeschriebenen `auth.toml` unter `auth_path`.
    fn resolve(home: &Path, auth_path: &Path, reference: &SecretRef) -> TestResult<String> {
        let content = fs::read_to_string(auth_path).map_err(ctx("read auth.toml with kek"))?;
        let auth: AuthConfig = toml::from_str(&content).map_err(ctx("parse auth.toml with kek"))?;
        let mut config = ResolvedConfig::default();
        config.auth.kek = Some(some_or(auth.kek, "bootstrapped [kek]")?);
        config.providers.insert(
            "check".to_owned(),
            toml::from_str::<ProviderToml>(&provider_toml("check", &reference.as_ref_string()))
                .map_err(ctx("check provider"))?,
        );
        let resolver = crate::secret_store::open_configured_secret_resolver(home, &config)
            .map_err(TestError::Unexpected)?;
        let resolver = some_or(resolver, "sealed resolver")?;
        let value = resolver
            .resolve(&reference.as_ref_string())
            .map_err(TestError::Unexpected)?;
        Ok(value.expose_secret().to_owned())
    }

    fn expect_secrets_ref(reference: &SecretRef) -> TestResult {
        if matches!(reference, SecretRef::Secrets(_)) {
            Ok(())
        } else {
            Err(TestError::Unexpected(format!(
                "expected a secrets: reference, got {reference}"
            )))
        }
    }

    /// Prozesseindeutiger Variablenname, der nicht in der Prozess-Umgebung
    /// steht (es wird nie `set_var` aufgerufen).
    fn unique_env_name(suffix: &str) -> TestResult<String> {
        let name = format!("HARW_MIGRATE_TEST_{}_{suffix}", std::process::id());
        if std::env::var_os(&name).is_some() {
            return Err(TestError::Unexpected(format!("{name} is set in the process env")));
        }
        Ok(name)
    }

    /// Rekursiver Schnappschuss: Pfad -> (Modus, Inhalt; `None` für
    /// Verzeichnisse).
    fn snapshot(root: &Path) -> TestResult<BTreeMap<PathBuf, (u32, Option<Vec<u8>>)>> {
        let mut result = BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).map_err(ctx("read snapshot dir"))? {
                let path = entry.map_err(ctx("snapshot entry"))?.path();
                let metadata = fs::symlink_metadata(&path).map_err(ctx("snapshot metadata"))?;
                let mode = metadata.permissions().mode();
                if metadata.is_dir() {
                    result.insert(path.clone(), (mode, None));
                    stack.push(path);
                } else {
                    let bytes = fs::read(&path).map_err(ctx("snapshot file"))?;
                    result.insert(path, (mode, Some(bytes)));
                }
            }
        }
        let root_mode = fs::metadata(root)
            .map_err(ctx("snapshot root"))?
            .permissions()
            .mode();
        result.insert(root.to_path_buf(), (root_mode, None));
        Ok(result)
    }

    #[test]
    fn migrate_rewrites_provider_file_ref_and_deletes_harw_plaintext_file() -> TestResult {
        let fixture = Fixture::new()?;
        let key = fixture.home().join("secrets").join("x.key");
        write_file(&key, "sk-harw-owned-value\n", 0o600)?;
        let provider_path = fixture.home().join("providers").join("x.toml");
        write_file(
            &provider_path,
            &provider_toml("x", &format!("file:{}", key.display())),
            0o600,
        )?;

        let report = fixture.run(&BTreeMap::new(), false)?;

        assert_eq!(report.stored, 1);
        let reference = read_provider_auth(&provider_path)?;
        expect_secrets_ref(&reference)?;
        assert!(!key.exists(), "harw plaintext file must be deleted");
        assert_eq!(report.deleted.len(), 1);
        assert!(report.rewritten_files.contains(&provider_path));
        let auth_path = harw_home::auth_path(fixture.home());
        assert_eq!(
            resolve(fixture.home(), &auth_path, &reference)?,
            "sk-harw-owned-value"
        );
        Ok(())
    }

    #[test]
    fn migrate_env_ref_is_copied_and_variable_source_untouched() -> TestResult {
        let fixture = Fixture::new()?;
        let name = unique_env_name("ENV")?;
        let dotenv = fixture.home().join(".env");
        write_file(&dotenv, &format!("{name}=sk-env-value\n"), 0o600)?;
        let dotenv_before = fs::read(&dotenv).map_err(ctx("read .env before"))?;
        let profile_auth = harw_home::auth_path(&fixture.profile);
        write_file(
            &profile_auth,
            &format!("[[credential_pool.openai]]\nsecret = \"env:{name}\"\nlabel = \"work\"\npriority = 2\n"),
            0o600,
        )?;
        let mut env_layer = BTreeMap::new();
        env_layer.insert(name.clone(), "sk-env-value".to_owned());

        let report = fixture.run(&env_layer, false)?;

        assert_eq!(report.stored, 1);
        assert_eq!(
            fs::read(&dotenv).map_err(ctx("read .env after"))?,
            dotenv_before,
            ".env must stay byte-identical"
        );
        assert!(std::env::var_os(&name).is_none(), "process env untouched");
        let pool = read_pool(&profile_auth, "openai")?;
        let entry = some_or(pool.first(), "pool entry")?;
        expect_secrets_ref(&entry.secret)?;
        assert_eq!(entry.label.as_deref(), Some("work"));
        assert_eq!(entry.priority, 2);
        assert_eq!(
            resolve(fixture.home(), &profile_auth, &entry.secret)?,
            "sk-env-value"
        );
        Ok(())
    }

    #[test]
    fn migrate_external_file_json_is_copied_but_kept() -> TestResult {
        let fixture = Fixture::new()?;
        let creds = fixture.outside.path().join("creds.json");
        write_file(&creds, "{\"OPENAI_API_KEY\": \"sk-external-json\"}", 0o600)?;
        let creds_before = fs::read(&creds).map_err(ctx("read creds before"))?;
        let provider_path = fixture.home().join("providers").join("ext.toml");
        write_file(
            &provider_path,
            &provider_toml("ext", &format!("file-json:{}#/OPENAI_API_KEY", creds.display())),
            0o600,
        )?;

        let report = fixture.run(&BTreeMap::new(), false)?;

        let reference = read_provider_auth(&provider_path)?;
        expect_secrets_ref(&reference)?;
        assert_eq!(
            fs::read(&creds).map_err(ctx("read creds after"))?,
            creds_before,
            "external source stays untouched"
        );
        assert!(report.deleted.is_empty());
        assert!(
            report
                .kept_sources
                .iter()
                .any(|path| path.file_name() == creds.file_name())
        );
        assert!(
            report
                .lines
                .iter()
                .any(|line| line.contains("Schnappschuss"))
        );
        let auth_path = harw_home::auth_path(fixture.home());
        assert_eq!(
            resolve(fixture.home(), &auth_path, &reference)?,
            "sk-external-json"
        );
        Ok(())
    }

    #[test]
    fn migrate_keeps_codex_login_reference_and_reports_it() -> TestResult {
        let fixture = Fixture::new()?;
        let codex = fixture.outside.path().join(".codex").join("auth.json");
        write_file(
            &codex,
            "{\"tokens\": {\"access_token\": \"codex-access-token\"}}",
            0o600,
        )?;
        let auth_path = harw_home::auth_path(fixture.home());
        write_file(
            &auth_path,
            &format!(
                "[[credential_pool.openai]]\nsecret = \"file-json:{}#/tokens/access_token\"\n",
                codex.display()
            ),
            0o600,
        )?;
        let auth_before = fs::read(&auth_path).map_err(ctx("read auth before"))?;

        let report = fixture.run(&BTreeMap::new(), false)?;

        assert_eq!(
            fs::read(&auth_path).map_err(ctx("read auth after"))?,
            auth_before,
            "Codex login reference must stay byte-identical"
        );
        assert_eq!(report.stored, 0);
        assert!(report.rewritten_files.is_empty());
        assert!(
            report
                .lines
                .iter()
                .any(|line| line.contains("Codex-Login-Verweis bleibt"))
        );
        assert!(codex.exists());
        Ok(())
    }

    #[test]
    fn migrate_deletion_scope_only_harw_named_files_in_home_secrets() -> TestResult {
        let fixture = Fixture::new()?;
        let secrets = fixture.home().join("secrets");
        let custom = secrets.join("custom.txt");
        let outside_key = fixture.outside.path().join("y.key");
        let still_referenced = secrets.join("z.key");
        let deletable = secrets.join("w-oauth.token");
        write_file(&custom, "custom-value", 0o600)?;
        write_file(&outside_key, "outside-value", 0o600)?;
        write_file(&still_referenced, "still-referenced-value", 0o600)?;
        write_file(&deletable, "deletable-value", 0o600)?;
        let providers = fixture.home().join("providers");
        for (name, path) in [
            ("a", &custom),
            ("b", &outside_key),
            ("c", &still_referenced),
            ("d", &deletable),
        ] {
            write_file(
                &providers.join(format!("{name}.toml")),
                &provider_toml(name, &format!("file:{}", path.display())),
                0o600,
            )?;
        }
        write_file(
            &harw_home::auth_path(fixture.home()),
            &format!(
                "[credentials]\nlegacy = \"file:{}\"\n",
                still_referenced.display()
            ),
            0o600,
        )?;

        let report = fixture.run(&BTreeMap::new(), false)?;

        assert_eq!(report.stored, 4);
        for name in ["a", "b", "c", "d"] {
            expect_secrets_ref(&read_provider_auth(&providers.join(format!("{name}.toml")))?)?;
        }
        assert!(custom.exists(), "non-harw name in <home>/secrets stays");
        assert!(outside_key.exists(), "file outside <home>/secrets stays");
        assert!(still_referenced.exists(), "still referenced file stays");
        assert!(!deletable.exists(), "unreferenced harw file is deleted");
        assert_eq!(report.deleted.len(), 1);
        assert_eq!(report.kept_sources.len(), 3);
        let credentials = fs::read_to_string(harw_home::auth_path(fixture.home()))
            .map_err(ctx("read auth.toml after"))?;
        assert!(credentials.contains(&format!("file:{}", still_referenced.display())));
        Ok(())
    }

    #[test]
    fn migrate_dry_run_changes_nothing() -> TestResult {
        let fixture = Fixture::new()?;
        let key = fixture.home().join("secrets").join("x.key");
        write_file(&key, "sk-dry-run-value", 0o600)?;
        let provider_path = fixture.home().join("providers").join("x.toml");
        write_file(
            &provider_path,
            &provider_toml("x", &format!("file:{}", key.display())),
            0o600,
        )?;
        let name = unique_env_name("DRY")?;
        write_file(
            &harw_home::auth_path(&fixture.profile),
            &format!("[[credential_pool.openai]]\nsecret = \"env:{name}\"\n"),
            0o600,
        )?;
        let mut env_layer = BTreeMap::new();
        env_layer.insert(name.clone(), "sk-dry-env".to_owned());
        let home_before = snapshot(fixture.home())?;
        let outside_before = snapshot(fixture.outside.path())?;

        let report = fixture.run(&env_layer, true)?;

        assert_eq!(snapshot(fixture.home())?, home_before);
        assert_eq!(snapshot(fixture.outside.path())?, outside_before);
        assert!(!fixture.home().join("keys").exists());
        assert!(!fixture.home().join("sealed-secrets").exists());
        assert_eq!(report.stored, 0);
        assert!(report.rewritten_files.is_empty());
        assert!(report.deleted.is_empty());
        assert!(
            report
                .lines
                .iter()
                .any(|line| line.contains("Plan, nichts geändert"))
        );
        assert!(report.lines.iter().any(|line| {
            line.contains(&format!("file:{}", key.display())) && line.contains("secrets:<neu>")
        }));
        assert!(
            report
                .lines
                .iter()
                .any(|line| line.contains(&format!("env:{name}")) && line.contains("secrets:<neu>"))
        );
        Ok(())
    }

    #[test]
    fn migrate_output_never_contains_values() -> TestResult {
        let fixture = Fixture::new()?;
        let values = [
            "sk-value-file-9f31",
            "sk-value-json-7c22",
            "sk-value-env-5a10",
            "sk-value-loose-3e44",
        ];
        let key = fixture.home().join("secrets").join("f.key");
        write_file(&key, values[0], 0o600)?;
        let json = fixture.outside.path().join("c.json");
        write_file(&json, &format!("{{\"k\": \"{}\"}}", values[1]), 0o600)?;
        let loose = fixture.outside.path().join("loose.key");
        write_file(&loose, values[3], 0o644)?;
        let name = unique_env_name("OUT")?;
        let providers = fixture.home().join("providers");
        write_file(
            &providers.join("f.toml"),
            &provider_toml("f", &format!("file:{}", key.display())),
            0o600,
        )?;
        write_file(
            &providers.join("j.toml"),
            &provider_toml("j", &format!("file-json:{}#/k", json.display())),
            0o600,
        )?;
        write_file(
            &harw_home::auth_path(fixture.home()),
            &format!(
                "[[credential_pool.openai]]\nsecret = \"env:{name}\"\n\n\
                 [[credential_pool.openai]]\nsecret = \"file:{}\"\n",
                loose.display()
            ),
            0o600,
        )?;
        let mut env_layer = BTreeMap::new();
        env_layer.insert(name, values[2].to_owned());

        let dry = fixture.run(&env_layer, true)?;
        let real = fixture.run(&env_layer, false)?;

        assert_eq!(real.stored, 3, "the group-readable file is not migrated");
        assert!(loose.exists());
        for line in dry.lines.iter().chain(real.lines.iter()) {
            for value in values {
                assert!(!line.contains(value), "report line leaks a value");
            }
        }
        Ok(())
    }
}
