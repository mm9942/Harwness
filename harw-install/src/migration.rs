//! Versionierte Migration der `config.toml` gemäß Contract-Master
//! `docs/design/CONTRACT-setup-install.md`, Abschnitt „Crate `harw-install` /
//! `src/migration.rs`".
//!
//! # Zweck
//! Führt eine geordnete Kette von Konfigurationsmigrationen aus, die ein
//! [`ConfigDocument`] schrittweise von einer vorgefundenen
//! `config_version` auf die Zielversion (`latest`) heben. Kommentare und
//! Formatierung des TOML-Dokuments bleiben durch `toml_edit` erhalten; der
//! Fremdtyp bleibt dabei hinter [`ConfigDocument`] verborgen.
//!
//! # Verantwortung
//! Dieses Modul besitzt die Ablaufsteuerung (`MigrationRunner`) und die
//! Migrations-Schnittstelle (`ConfigMigration`). Die konkreten Transformationen
//! werden von den einzelnen [`ConfigMigration`]-Implementierungen beigesteuert;
//! die Fehlerdefinition liegt in [`crate::error`].
//!
//! # Exportierte Typen
//! - [`ConfigDocument`] — bearbeitbares Dokument für Migrationen; kapselt `toml_edit`.
//! - [`ConfigMigration`] — Trait für einen einzelnen Versionsschritt.
//! - [`MigrationRunner`] — orchestriert Lesen, Backup, Anwenden, Persistieren.
//!
//! # Ablauf von [`MigrationRunner::run`]
//! 1. `config.toml` lesen (fehlend → leeres Dokument, Version 0).
//! 2. Aktuelle `config_version` lesen (fehlt → 0).
//! 3. Ist die aktuelle Version bereits `latest`, No-op (idempotent).
//! 4. Ist die aktuelle Version größer als `latest`, [`MigrationError::Version`].
//! 5. Backup `config.toml.bak.<n>` schreiben (nur wenn Datei existierte).
//! 6. Alle Migrationen mit `from_version >= current` aufsteigend anwenden.
//! 7. `config_version = latest` setzen und persistieren.
//!
//! # Nebenläufigkeit
//! `MigrationRunner` ist `Send`, sofern die enthaltenen Trait-Objekte `Send`
//! sind; er hält keinen gemeinsam veränderlichen Zustand. `run` führt reine
//! Datei-I/O aus und ist nicht für nebenläufige Aufrufe auf denselben Pfad
//! ausgelegt (keine Sperren).
//!
//! # Fehler
//! Alle Fehler dieses Moduls sind [`MigrationError`]-Varianten.
//!
//! # Examples
//! ```rust,no_run
//! use std::path::Path;
//! use harw_install::migration::{ConfigDocument, ConfigMigration, MigrationRunner};
//! use harw_install::error::MigrationError;
//!
//! struct EnableFeature;
//! impl ConfigMigration for EnableFeature {
//!     fn from_version(&self) -> u32 { 0 }
//!     fn apply(&self, doc: &mut ConfigDocument) -> Result<(), MigrationError> {
//!         if doc.set_bool(&["feature", "enabled"], true) {
//!             Ok(())
//!         } else {
//!             Err(MigrationError::Apply {
//!                 from_version: 0,
//!                 reason: "feature ist keine Tabelle".to_owned(),
//!             })
//!         }
//!     }
//! }
//!
//! let runner = MigrationRunner::new(vec![Box::new(EnableFeature)], 1);
//! let _reached = runner.run(Path::new("/tmp/config.toml"));
//! ```

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table};

use crate::error::MigrationError;

/// TOML-Schlüssel, unter dem die Konfigurationsversion abgelegt wird.
const VERSION_KEY: &str = "config_version";

/// Bearbeitbares TOML-Dokument, das eine [`ConfigMigration`] verändert.
///
/// # Description
/// Crate-eigene Hülle um das formatierungserhaltende TOML-Dokument; `toml_edit`
/// erscheint dadurch nicht in der öffentlichen API. Pfade sind Segmentlisten
/// (`&["sandbox", "timeout"]` = `[sandbox] timeout`) und werden nicht an
/// Punkten zerlegt. Die Lesezugriffe (`contains`, `get_*`) folgen Tabellen und
/// Inline-Tabellen; die Schreibhelfer (`set_*`, `remove`) folgen nur
/// Standard-Tabellen (`[a]`, `a.b = …`), `set_*` legt fehlende davon an.
///
/// Fail closed: Bei leerem Pfad oder einem vorhandenen Nicht-Tabellen-Eintrag
/// auf dem Weg (Wert, Inline-Tabelle, Array) liefern die Schreibhelfer `false`;
/// `set_*` zusätzlich, wenn das Ziel eine Tabelle oder ein Array von Tabellen
/// ist. Das Dokument bleibt in diesen Fällen unverändert.
///
/// # Concurrency
/// Kein gemeinsam veränderlicher Zustand; Mutation nur über `&mut self`.
#[derive(Default)]
pub struct ConfigDocument {
    /// Das formatierungserhaltende TOML-Dokument.
    inner: DocumentMut,
}

impl ConfigDocument {
    /// Erstellt ein leeres Dokument.
    ///
    /// # Returns
    /// Ein Dokument ohne Einträge.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parst TOML-Text formatierungserhaltend.
    ///
    /// # Arguments
    /// - `text` (`&str`): der vollständige Dateiinhalt.
    ///
    /// # Returns
    /// Das geparste Dokument.
    ///
    /// # Errors
    /// - [`MigrationError::Parse`]: `text` ist kein gültiges TOML; Meldung und
    ///   Fehlerbereich stehen in der Variante.
    pub fn parse(text: &str) -> Result<Self, MigrationError> {
        text.parse::<DocumentMut>()
            .map(|inner| Self { inner })
            .map_err(|err| MigrationError::from_toml(&err))
    }

    /// Prüft, ob unter `path` ein Eintrag (Wert oder Tabelle) existiert.
    ///
    /// # Returns
    /// `true`, wenn der Pfad auflösbar ist; bei leerem Pfad `false`.
    #[must_use]
    pub fn contains(&self, path: &[&str]) -> bool {
        self.lookup(path).is_some()
    }

    /// Liest eine Ganzzahl unter `path`.
    ///
    /// # Returns
    /// `Some(wert)`, wenn dort eine Ganzzahl steht; sonst `None` (fehlend,
    /// anderer Typ oder leerer Pfad).
    #[must_use]
    pub fn get_integer(&self, path: &[&str]) -> Option<i64> {
        self.lookup(path).and_then(Item::as_integer)
    }

    /// Liest einen Wahrheitswert unter `path`.
    ///
    /// # Returns
    /// `Some(wert)`, wenn dort ein Wahrheitswert steht; sonst `None`.
    #[must_use]
    pub fn get_bool(&self, path: &[&str]) -> Option<bool> {
        self.lookup(path).and_then(Item::as_bool)
    }

    /// Liest eine Zeichenkette unter `path`.
    ///
    /// # Returns
    /// `Some(wert)`, wenn dort eine Zeichenkette steht; sonst `None`.
    #[must_use]
    pub fn get_str(&self, path: &[&str]) -> Option<&str> {
        self.lookup(path).and_then(Item::as_str)
    }

    /// Setzt eine Ganzzahl unter `path`; fehlende Tabellen werden angelegt.
    ///
    /// # Returns
    /// `true`, wenn geschrieben wurde (ein Wert anderen Typs wird ersetzt);
    /// `false` in den Fail-closed-Fällen des Typs, das Dokument bleibt dann
    /// unverändert.
    #[must_use]
    pub fn set_integer(&mut self, path: &[&str], value: i64) -> bool {
        self.set_item(path, toml_edit::value(value))
    }

    /// Setzt einen Wahrheitswert unter `path`; fehlende Tabellen werden angelegt.
    ///
    /// # Returns
    /// Wie [`set_integer`](ConfigDocument::set_integer).
    #[must_use]
    pub fn set_bool(&mut self, path: &[&str], value: bool) -> bool {
        self.set_item(path, toml_edit::value(value))
    }

    /// Setzt eine Zeichenkette unter `path`; fehlende Tabellen werden angelegt.
    ///
    /// # Returns
    /// Wie [`set_integer`](ConfigDocument::set_integer).
    #[must_use]
    pub fn set_str(&mut self, path: &[&str], value: &str) -> bool {
        self.set_item(path, toml_edit::value(value))
    }

    /// Entfernt den Eintrag unter `path`, gleich ob Wert oder Tabelle.
    ///
    /// # Returns
    /// `true`, wenn ein Eintrag entfernt wurde; `false`, wenn der Pfad fehlt,
    /// leer ist oder über eine Nicht-Tabelle führt (Dokument unverändert).
    #[must_use]
    pub fn remove(&mut self, path: &[&str]) -> bool {
        let Some((leaf, parents)) = path.split_last() else {
            return false;
        };
        // Wie `table_for_write`, aber ohne fehlende Tabellen anzulegen.
        let mut table = self.inner.as_table_mut();
        for segment in parents {
            let Some(next) = table.get_mut(segment).and_then(Item::as_table_mut) else {
                return false;
            };
            table = next;
        }
        table.remove(leaf).is_some()
    }

    /// Löst `path` über Tabellen und Inline-Tabellen auf; leerer Pfad → `None`.
    fn lookup(&self, path: &[&str]) -> Option<&Item> {
        if path.is_empty() {
            return None;
        }
        let mut item = self.inner.as_item();
        for segment in path {
            item = item.get(*segment)?;
        }
        Some(item)
    }

    /// Schreibt `value` unter `path`; `false` in den Fail-closed-Fällen des Typs.
    fn set_item(&mut self, path: &[&str], value: Item) -> bool {
        let Some((leaf, parents)) = path.split_last() else {
            return false;
        };
        let Some(table) = table_for_write(self.inner.as_table_mut(), parents) else {
            return false;
        };
        match table.get_mut(leaf) {
            // Vorhandene Werte werden an Ort und Stelle ersetzt: `Table::insert`
            // würde die Schlüssel-Formatierung samt Kommentar darüber verwerfen.
            Some(existing) if existing.is_value() => *existing = value,
            // Tabellen und Arrays von Tabellen werden nie überschrieben.
            Some(_) => return false,
            None => {
                table.insert(leaf, value);
            }
        }
        true
    }
}

impl fmt::Display for ConfigDocument {
    /// Rendert das Dokument als TOML-Text (Kommentare und Formatierung erhalten).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.inner, f)
    }
}

/// Folgt `parents` über Standard-Tabellen und legt fehlende implizit an.
///
/// Liefert `None`, sobald ein vorhandenes Segment keine Standard-Tabelle ist.
/// Tabellen entstehen erst hinter dem letzten vorhandenen Segment; ein `None`
/// hinterlässt daher nie Teiländerungen.
fn table_for_write<'a>(mut table: &'a mut Table, parents: &[&str]) -> Option<&'a mut Table> {
    for segment in parents {
        if !table.contains_key(segment) {
            let mut fresh = Table::new();
            fresh.set_implicit(true);
            table.insert(segment, Item::Table(fresh));
        }
        table = table.get_mut(segment)?.as_table_mut()?;
    }
    Some(table)
}

/// Ein einzelner, versionierter Migrationsschritt.
///
/// # Description
/// Eine `ConfigMigration` transformiert ein [`ConfigDocument`] in-place
/// von ihrer Ausgangsversion ([`from_version`](ConfigMigration::from_version))
/// auf die nächsthöhere. Der [`MigrationRunner`] wählt und ordnet die
/// anzuwendenden Schritte; eine Implementierung muss nur ihre eigene
/// Transformation kennen.
///
/// # Concurrency
/// Implementierungen sollten zustandslos und damit gefahrlos gemeinsam nutzbar
/// sein; der Runner ruft `apply` sequentiell auf.
// Signatur `from_version(&self)` ist vom Contract-Master verbindlich vorgegeben;
// der Namenskonvention-Lint ist daher bewusst unterdrückt.
#[allow(clippy::wrong_self_convention)]
pub trait ConfigMigration {
    /// Gibt die Ausgangsversion zurück, ab der dieser Schritt greift.
    ///
    /// # Returns
    /// Die Version (`u32`), die das Dokument vor Anwendung dieses Schritts
    /// besitzt. Ein Dokument der Version `n` wird durch die Migration mit
    /// `from_version() == n` auf `n + 1` gehoben.
    fn from_version(&self) -> u32;

    /// Wendet die Transformation dieses Schritts auf das Dokument an.
    ///
    /// # Arguments
    /// - `doc` (`&mut ConfigDocument`): das zu verändernde Dokument;
    ///   wird in-place mutiert, Formatierung/Kommentare bleiben erhalten.
    ///
    /// # Returns
    /// `Ok(())` bei erfolgreicher Transformation.
    ///
    /// # Errors
    /// - [`MigrationError::Apply`]: wenn die Transformation fehlschlägt. Die
    ///   Schreibhelfer von [`ConfigDocument`] liefern `false`, statt zu
    ///   überschreiben; die Implementierung macht daraus diesen Fehler.
    ///
    /// # Concurrency
    /// Muss nicht threadsicher sein; wird vom Runner sequentiell aufgerufen.
    fn apply(&self, doc: &mut ConfigDocument) -> Result<(), MigrationError>;
}

/// Orchestriert die Ausführung einer Migrationskette über `config.toml`.
///
/// # Description
/// Hält die verfügbaren Migrationsschritte und die Zielversion (`latest`).
/// [`run`](MigrationRunner::run) liest die Datei, legt bei Bedarf ein Backup an,
/// wendet die passenden Schritte in aufsteigender Reihenfolge an und persistiert
/// das Ergebnis mit gesetzter Zielversion.
///
/// # Concurrency
/// Der Runner selbst ist unveränderlich nach Konstruktion. Er hält keine
/// Sperren; parallele `run`-Aufrufe auf denselben Pfad sind nicht abgesichert.
pub struct MigrationRunner {
    /// Verfügbare Migrationsschritte (Reihenfolge beliebig; wird intern sortiert).
    migrations: Vec<Box<dyn ConfigMigration>>,
    /// Zielversion, auf die `run` das Dokument hebt.
    latest: u32,
}

impl MigrationRunner {
    /// Erstellt einen Runner aus einer Menge von Migrationen und der Zielversion.
    ///
    /// # Arguments
    /// - `migrations` (`Vec<Box<dyn ConfigMigration>>`): die verfügbaren
    ///   Schritte; Übernahme des Eigentums. Reihenfolge ist unerheblich, da
    ///   `run` nach `from_version` sortiert.
    /// - `latest` (`u32`): die Zielversion, auf die migriert werden soll.
    ///
    /// # Returns
    /// Einen konstruierten [`MigrationRunner`].
    ///
    /// # Concurrency
    /// Reine Wertkonstruktion, keine I/O, threadsicher.
    pub fn new(migrations: Vec<Box<dyn ConfigMigration>>, latest: u32) -> Self {
        Self { migrations, latest }
    }

    /// Liest, migriert und persistiert `config.toml`; gibt die erreichte Version zurück.
    ///
    /// # Description
    /// Vollständiger Ablauf siehe Moduldokumentation. Ist die vorgefundene
    /// Version bereits `latest`, erfolgt keine Änderung (idempotent). Ein Backup
    /// wird nur geschrieben, wenn die Datei existierte und tatsächlich migriert
    /// wird.
    ///
    /// # Arguments
    /// - `config_path` (`&Path`): Pfad zur `config.toml`. Existiert die Datei
    ///   nicht, wird ein leeres Dokument der Version 0 angenommen.
    ///
    /// # Returns
    /// Die nach dem Lauf gültige `config_version` (im Erfolgsfall stets
    /// gleich `latest`; bei bereits aktueller Datei ebenfalls `latest`).
    ///
    /// # Errors
    /// - [`MigrationError::Io`]: Lesen/Schreiben von Datei oder Backup schlug fehl.
    /// - [`MigrationError::Parse`]: der Dateiinhalt war kein gültiges TOML;
    ///   Meldung und Fehlerbereich stehen in der Variante.
    /// - [`MigrationError::Version`]: die vorgefundene Version ist größer als
    ///   `latest` (kein Abwärts-Migrationspfad).
    /// - [`MigrationError::Apply`]: ein Migrationsschritt schlug fehl.
    ///
    /// # Concurrency
    /// Führt blockierende Datei-I/O aus; nicht für parallele Aufrufe auf
    /// denselben Pfad gedacht (keine Sperren).
    pub fn run(&self, config_path: &Path) -> Result<u32, MigrationError> {
        let existing = read_optional(config_path)?;

        let mut doc = match &existing {
            Some(content) => ConfigDocument::parse(content)?,
            None => ConfigDocument::new(),
        };

        let current = read_version(&doc);

        if current == self.latest {
            return Ok(current);
        }
        if current > self.latest {
            return Err(MigrationError::Version {
                found: current,
                expected: self.latest,
            });
        }

        // Backup nur anlegen, wenn eine Datei mit Inhalt existierte.
        if let Some(content) = &existing {
            write_backup(config_path, content)?;
        }

        let mut steps: Vec<&Box<dyn ConfigMigration>> = self
            .migrations
            .iter()
            .filter(|m| m.from_version() >= current)
            .collect();
        steps.sort_by_key(|m| m.from_version());

        for step in steps {
            step.apply(&mut doc)?;
        }

        // Der Runner besitzt `config_version` und überschreibt den Schlüssel wie
        // bisher, unabhängig von der vorgefundenen Form (daher nicht `set_integer`).
        doc.inner[VERSION_KEY] = toml_edit::value(i64::from(self.latest));
        write_document(config_path, &doc)?;

        Ok(self.latest)
    }
}

/// Liest den Dateiinhalt oder gibt `None` zurück, wenn die Datei fehlt.
///
/// # Errors
/// - [`MigrationError::Io`]: bei I/O-Fehlern außer `NotFound`.
fn read_optional(path: &Path) -> Result<Option<String>, MigrationError> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(MigrationError::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}

/// Liest die `config_version` aus dem Dokument; fehlt sie, gilt 0.
fn read_version(doc: &ConfigDocument) -> u32 {
    doc.get_integer(&[VERSION_KEY])
        .and_then(|i| u32::try_from(i).ok())
        .unwrap_or(0)
}

/// Schreibt ein Backup `config.toml.bak.<n>` mit der nächsten freien Nummer.
///
/// # Errors
/// - [`MigrationError::Io`]: das Backup konnte nicht geschrieben werden.
fn write_backup(config_path: &Path, content: &str) -> Result<(), MigrationError> {
    let backup = next_backup_path(config_path);
    fs::write(&backup, content).map_err(|source| MigrationError::Io {
        path: backup.display().to_string(),
        source,
    })
}

/// Ermittelt den ersten nicht vergebenen Backup-Pfad `config.toml.bak.<n>`.
fn next_backup_path(config_path: &Path) -> PathBuf {
    let mut n: u32 = 0;
    loop {
        let candidate = backup_candidate(config_path, n);
        if !candidate.exists() {
            return candidate;
        }
        n = n.saturating_add(1);
    }
}

/// Baut den Kandidatenpfad `config.toml.bak.<n>` für eine gegebene Nummer.
fn backup_candidate(config_path: &Path, n: u32) -> PathBuf {
    let mut name = config_path
        .file_name()
        .map(|f| f.to_owned())
        .unwrap_or_default();
    name.push(format!(".bak.{n}"));
    match config_path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

/// Persistiert das Dokument als TOML-Text nach `config_path`.
///
/// # Errors
/// - [`MigrationError::Io`]: das Schreiben schlug fehl.
fn write_document(config_path: &Path, doc: &ConfigDocument) -> Result<(), MigrationError> {
    fs::write(config_path, doc.to_string()).map_err(|source| MigrationError::Io {
        path: config_path.display().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Dummy-Migration 0→1: setzt einen Marker-Schlüssel `step_a`.
    struct AddStepA;
    impl ConfigMigration for AddStepA {
        fn from_version(&self) -> u32 {
            0
        }
        fn apply(&self, doc: &mut ConfigDocument) -> Result<(), MigrationError> {
            if doc.set_bool(&["step_a"], true) {
                Ok(())
            } else {
                Err(MigrationError::Apply {
                    from_version: 0,
                    reason: "step_a nicht setzbar".to_owned(),
                })
            }
        }
    }

    /// Dummy-Migration 1→2: setzt einen Marker-Schlüssel `step_b`.
    struct AddStepB;
    impl ConfigMigration for AddStepB {
        fn from_version(&self) -> u32 {
            1
        }
        fn apply(&self, doc: &mut ConfigDocument) -> Result<(), MigrationError> {
            if doc.set_integer(&["step_b"], 42) {
                Ok(())
            } else {
                Err(MigrationError::Apply {
                    from_version: 1,
                    reason: "step_b nicht setzbar".to_owned(),
                })
            }
        }
    }

    /// Migration, die immer scheitert — prüft Fehlerpropagation.
    struct FailingStep;
    impl ConfigMigration for FailingStep {
        fn from_version(&self) -> u32 {
            0
        }
        fn apply(&self, _doc: &mut ConfigDocument) -> Result<(), MigrationError> {
            Err(MigrationError::Apply {
                from_version: 0,
                reason: "absichtlich".to_owned(),
            })
        }
    }

    fn tmp_config(name: &str) -> TestResult<PathBuf> {
        let mut dir = std::env::temp_dir();
        let unique = format!(
            "harw-migration-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        dir.push(unique);
        std::fs::create_dir_all(&dir).map_err(ctx("temp dir anlegen"))?; // Testaufbau
        dir.push("config.toml");
        Ok(dir)
    }

    #[test]
    fn test_run_chain_zero_to_two_applies_both() -> TestResult {
        let path = tmp_config("chain")?;
        std::fs::write(&path, "config_version = 0\n").map_err(ctx("schreiben"))?; // Testaufbau

        let runner = MigrationRunner::new(vec![Box::new(AddStepA), Box::new(AddStepB)], 2);
        let reached = runner.run(&path).map_err(ctx("migration"))?; // Testprüfung
        assert_eq!(reached, 2);

        let text = std::fs::read_to_string(&path).map_err(ctx("lesen"))?; // Testprüfung
        let doc = ConfigDocument::parse(&text).map_err(ctx("parse"))?; // Testprüfung
        assert_eq!(read_version(&doc), 2);
        assert_eq!(doc.get_bool(&["step_a"]), Some(true));
        assert_eq!(doc.get_integer(&["step_b"]), Some(42));
        Ok(())
    }

    #[test]
    fn test_run_missing_file_starts_from_zero() -> TestResult {
        let path = tmp_config("missing")?;
        // Datei existiert bewusst nicht.
        let runner = MigrationRunner::new(vec![Box::new(AddStepA), Box::new(AddStepB)], 2);
        let reached = runner.run(&path).map_err(ctx("migration"))?; // Testprüfung
        assert_eq!(reached, 2);
        assert!(path.exists());
        Ok(())
    }

    #[test]
    fn test_run_writes_backup_on_migration() -> TestResult {
        let path = tmp_config("backup")?;
        std::fs::write(&path, "config_version = 0\n").map_err(ctx("schreiben"))?; // Testaufbau

        let runner = MigrationRunner::new(vec![Box::new(AddStepA), Box::new(AddStepB)], 2);
        runner.run(&path).map_err(ctx("migration"))?; // Testprüfung

        let backup = backup_candidate(&path, 0);
        assert!(backup.exists(), "Backup config.toml.bak.0 muss entstehen");
        let backup_text = std::fs::read_to_string(&backup).map_err(ctx("backup lesen"))?; // Testprüfung
        assert_eq!(backup_text, "config_version = 0\n");
        Ok(())
    }

    #[test]
    fn test_run_backup_numbering_increments() -> TestResult {
        let path = tmp_config("numbering")?;
        std::fs::write(&path, "config_version = 0\n").map_err(ctx("schreiben"))?; // Testaufbau

        // Erster Lauf 0→1 legt .bak.0 an.
        MigrationRunner::new(vec![Box::new(AddStepA)], 1)
            .run(&path)
            .map_err(ctx("erster lauf"))?; // Testprüfung
        // Zweiter Lauf 1→2 legt .bak.1 an.
        MigrationRunner::new(vec![Box::new(AddStepB)], 2)
            .run(&path)
            .map_err(ctx("zweiter lauf"))?; // Testprüfung

        assert!(backup_candidate(&path, 0).exists());
        assert!(backup_candidate(&path, 1).exists());
        Ok(())
    }

    #[test]
    fn test_run_idempotent_no_op_when_current_equals_latest() -> TestResult {
        let path = tmp_config("idempotent")?;
        std::fs::write(&path, "config_version = 2\n").map_err(ctx("schreiben"))?; // Testaufbau

        let runner = MigrationRunner::new(vec![Box::new(AddStepA), Box::new(AddStepB)], 2);
        let reached = runner.run(&path).map_err(ctx("migration"))?; // Testprüfung
        assert_eq!(reached, 2);

        // Kein Backup, da No-op.
        assert!(!backup_candidate(&path, 0).exists());
        // Marker aus AddStepA/B dürfen NICHT gesetzt worden sein.
        let text = std::fs::read_to_string(&path).map_err(ctx("lesen"))?; // Testprüfung
        let doc = ConfigDocument::parse(&text).map_err(ctx("parse"))?; // Testprüfung
        assert!(!doc.contains(&["step_a"]));
        assert!(!doc.contains(&["step_b"]));
        Ok(())
    }

    #[test]
    fn test_run_rejects_downgrade_with_version_error() -> TestResult {
        let path = tmp_config("downgrade")?;
        std::fs::write(&path, "config_version = 5\n").map_err(ctx("schreiben"))?; // Testaufbau

        let runner = MigrationRunner::new(vec![Box::new(AddStepA)], 2);
        match runner.run(&path) {
            Err(MigrationError::Version { found, expected }) => {
                assert_eq!(found, 5);
                assert_eq!(expected, 2);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartete Version-Fehler, erhielt {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_run_propagates_apply_error() -> TestResult {
        let path = tmp_config("apply-fail")?;
        std::fs::write(&path, "config_version = 0\n").map_err(ctx("schreiben"))?; // Testaufbau

        let runner = MigrationRunner::new(vec![Box::new(FailingStep)], 1);
        match runner.run(&path) {
            Err(MigrationError::Apply { from_version, .. }) => assert_eq!(from_version, 0),
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartete Apply-Fehler, erhielt {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_run_invalid_toml_returns_parse_error() -> TestResult {
        let path = tmp_config("badtoml")?;
        std::fs::write(&path, "a = = 1\n").map_err(ctx("schreiben"))?; // Testaufbau

        // Referenz: derselbe Inhalt direkt geparst liefert Meldung und Bereich.
        let Err(expected) = "a = = 1\n".parse::<DocumentMut>() else {
            return Err(TestError::Unexpected(
                "ungültiges TOML wurde akzeptiert".to_owned(),
            ));
        };

        let runner = MigrationRunner::new(vec![Box::new(AddStepA)], 1);
        match runner.run(&path) {
            Err(MigrationError::Parse { message, span }) => {
                assert!(!message.is_empty());
                assert_eq!(message, expected.to_string());
                assert_eq!(span, expected.span());
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartete Parse-Fehler, erhielt {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_only_matching_migrations_applied() -> TestResult {
        let path = tmp_config("subset")?;
        std::fs::write(&path, "config_version = 1\n").map_err(ctx("schreiben"))?; // Testaufbau

        // current=1: nur AddStepB (from_version 1) darf greifen, AddStepA nicht.
        let runner = MigrationRunner::new(vec![Box::new(AddStepA), Box::new(AddStepB)], 2);
        runner.run(&path).map_err(ctx("migration"))?; // Testprüfung

        let text = std::fs::read_to_string(&path).map_err(ctx("lesen"))?; // Testprüfung
        let doc = ConfigDocument::parse(&text).map_err(ctx("parse"))?; // Testprüfung
        assert!(!doc.contains(&["step_a"]), "AddStepA darf nicht greifen");
        assert_eq!(doc.get_integer(&["step_b"]), Some(42));
        Ok(())
    }

    #[test]
    fn test_config_document_nested_set_get_roundtrip() -> TestResult {
        let mut doc = ConfigDocument::new();
        assert!(doc.set_integer(&["sandbox", "timeout"], 30));
        assert!(doc.set_str(&["sandbox", "mode"], "strict"));
        assert_eq!(doc.get_integer(&["sandbox", "timeout"]), Some(30));
        assert_eq!(doc.get_str(&["sandbox", "mode"]), Some("strict"));

        // Gerenderter Text parst wieder zu denselben Werten.
        let reparsed = ConfigDocument::parse(&doc.to_string()).map_err(ctx("reparse"))?; // Testprüfung
        assert_eq!(reparsed.get_integer(&["sandbox", "timeout"]), Some(30));
        assert_eq!(reparsed.get_str(&["sandbox", "mode"]), Some("strict"));
        Ok(())
    }

    #[test]
    fn test_config_document_set_refuses_non_table_parent_and_leaves_doc_unchanged() -> TestResult
    {
        let mut doc = ConfigDocument::parse("a = 1\n").map_err(ctx("parse"))?; // Testaufbau
        assert!(!doc.set_bool(&["a", "b"], true));
        assert_eq!(doc.to_string(), "a = 1\n");
        assert_eq!(doc.get_integer(&["a"]), Some(1));
        Ok(())
    }

    #[test]
    fn test_config_document_set_refuses_to_overwrite_table() -> TestResult {
        let mut doc = ConfigDocument::parse("[t]\nx = 1\n").map_err(ctx("parse"))?; // Testaufbau
        assert!(!doc.set_integer(&["t"], 5));
        assert_eq!(doc.get_integer(&["t", "x"]), Some(1));
        assert_eq!(doc.to_string(), "[t]\nx = 1\n");
        Ok(())
    }

    #[test]
    fn test_config_document_inline_table_read_only() -> TestResult {
        let mut doc = ConfigDocument::parse("a = { b = true }\n").map_err(ctx("parse"))?; // Testaufbau
        // Lesen folgt Inline-Tabellen, Schreiben nicht.
        assert_eq!(doc.get_bool(&["a", "b"]), Some(true));
        assert!(!doc.set_bool(&["a", "c"], true));
        assert!(!doc.remove(&["a", "b"]));
        assert_eq!(doc.to_string(), "a = { b = true }\n");
        Ok(())
    }

    #[test]
    fn test_config_document_replaces_scalar_of_other_type() -> TestResult {
        let mut doc = ConfigDocument::parse("n = \"x\"\n").map_err(ctx("parse"))?; // Testaufbau
        assert_eq!(doc.get_integer(&["n"]), None);
        assert_eq!(doc.get_str(&["n"]), Some("x"));
        assert!(doc.set_integer(&["n"], 3));
        assert_eq!(doc.get_integer(&["n"]), Some(3));
        Ok(())
    }

    #[test]
    fn test_config_document_remove() -> TestResult {
        let mut doc = ConfigDocument::parse("keep = 1\ndrop = 2\n[old]\nx = 1\n")
            .map_err(ctx("parse"))?; // Testaufbau
        assert!(doc.remove(&["drop"]));
        assert!(!doc.remove(&["drop"]));
        assert!(doc.remove(&["old"]));
        assert!(!doc.contains(&["old"]));
        assert!(!doc.remove(&["missing", "x"]));
        assert!(doc.contains(&["keep"]));
        Ok(())
    }

    #[test]
    fn test_config_document_empty_path_rejected() -> TestResult {
        let mut doc = ConfigDocument::new();
        assert!(!doc.set_bool(&[], true));
        assert!(!doc.remove(&[]));
        assert!(!doc.contains(&[]));
        assert_eq!(doc.get_bool(&[]), None);
        Ok(())
    }

    #[test]
    fn test_config_document_preserves_comments() -> TestResult {
        let mut doc =
            ConfigDocument::parse("# Kopf\nkeep = 1 # bleibt\n").map_err(ctx("parse"))?; // Testaufbau
        assert!(doc.set_integer(&["new"], 2));
        let text = doc.to_string();
        assert!(text.contains("# Kopf"), "Kopfkommentar fehlt: {text}");
        assert!(text.contains("# bleibt"), "Zeilenkommentar fehlt: {text}");
        Ok(())
    }

    #[test]
    fn test_config_document_replace_keeps_comment_above_key() -> TestResult {
        let mut doc =
            ConfigDocument::parse("# Kopf\nkeep = 1\n").map_err(ctx("parse"))?; // Testaufbau
        // Ersetzen eines vorhandenen Werts lässt den Schlüssel samt Kommentar stehen.
        assert!(doc.set_integer(&["keep"], 3));
        assert_eq!(doc.get_integer(&["keep"]), Some(3));
        let text = doc.to_string();
        assert!(text.contains("# Kopf"), "Kopfkommentar fehlt: {text}");
        Ok(())
    }
}
