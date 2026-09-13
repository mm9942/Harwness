//! Versionierte Migration der `config.toml` gemäß Contract-Master
//! `docs/design/CONTRACT-setup-install.md`, Abschnitt „Crate `harw-install` /
//! `src/migration.rs`".
//!
//! # Zweck
//! Führt eine geordnete Kette von Konfigurationsmigrationen aus, die ein
//! `toml_edit::DocumentMut` schrittweise von einer vorgefundenen
//! `config_version` auf die Zielversion (`latest`) heben. Kommentare und
//! Formatierung des TOML-Dokuments bleiben durch `toml_edit` erhalten.
//!
//! # Verantwortung
//! Dieses Modul besitzt die Ablaufsteuerung (`MigrationRunner`) und die
//! Migrations-Schnittstelle (`ConfigMigration`). Die konkreten Transformationen
//! werden von den einzelnen [`ConfigMigration`]-Implementierungen beigesteuert;
//! die Fehlerdefinition liegt in [`crate::error`].
//!
//! # Exportierte Typen
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
//! use harw_install::migration::{ConfigMigration, MigrationRunner};
//! use harw_install::error::MigrationError;
//!
//! struct Noop;
//! impl ConfigMigration for Noop {
//!     fn from_version(&self) -> u32 { 0 }
//!     fn apply(&self, _doc: &mut toml_edit::DocumentMut) -> Result<(), MigrationError> {
//!         Ok(())
//!     }
//! }
//!
//! let runner = MigrationRunner::new(vec![Box::new(Noop)], 1);
//! let _reached = runner.run(Path::new("/tmp/config.toml"));
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use toml_edit::DocumentMut;

use crate::error::MigrationError;

/// TOML-Schlüssel, unter dem die Konfigurationsversion abgelegt wird.
const VERSION_KEY: &str = "config_version";

/// Ein einzelner, versionierter Migrationsschritt.
///
/// # Description
/// Eine `ConfigMigration` transformiert ein `toml_edit::DocumentMut` in-place
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
    /// - `doc` (`&mut toml_edit::DocumentMut`): das zu verändernde Dokument;
    ///   wird in-place mutiert, Formatierung/Kommentare bleiben erhalten.
    ///
    /// # Returns
    /// `Ok(())` bei erfolgreicher Transformation.
    ///
    /// # Errors
    /// - [`MigrationError::Apply`]: wenn die Transformation fehlschlägt.
    ///
    /// # Concurrency
    /// Muss nicht threadsicher sein; wird vom Runner sequentiell aufgerufen.
    fn apply(&self, doc: &mut DocumentMut) -> Result<(), MigrationError>;
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
    /// - [`MigrationError::Parse`]: der Dateiinhalt war kein gültiges TOML.
    /// - [`MigrationError::Version`]: die vorgefundene Version ist größer als
    ///   `latest` (kein Abwärts-Migrationspfad).
    /// - [`MigrationError::Apply`]: ein Migrationsschritt schlug fehl.
    ///
    /// # Concurrency
    /// Führt blockierende Datei-I/O aus; nicht für parallele Aufrufe auf
    /// denselben Pfad gedacht (keine Sperren).
    pub fn run(&self, config_path: &Path) -> Result<u32, MigrationError> {
        let existing = read_optional(config_path)?;

        let mut doc: DocumentMut = match &existing {
            Some(content) => content.parse::<DocumentMut>()?,
            None => DocumentMut::new(),
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

        doc[VERSION_KEY] = toml_edit::value(i64::from(self.latest));
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
fn read_version(doc: &DocumentMut) -> u32 {
    doc.get(VERSION_KEY)
        .and_then(|item| item.as_integer())
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
fn write_document(config_path: &Path, doc: &DocumentMut) -> Result<(), MigrationError> {
    fs::write(config_path, doc.to_string()).map_err(|source| MigrationError::Io {
        path: config_path.display().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dummy-Migration 0→1: setzt einen Marker-Schlüssel `step_a`.
    struct AddStepA;
    impl ConfigMigration for AddStepA {
        fn from_version(&self) -> u32 {
            0
        }
        fn apply(&self, doc: &mut DocumentMut) -> Result<(), MigrationError> {
            doc["step_a"] = toml_edit::value(true);
            Ok(())
        }
    }

    /// Dummy-Migration 1→2: setzt einen Marker-Schlüssel `step_b`.
    struct AddStepB;
    impl ConfigMigration for AddStepB {
        fn from_version(&self) -> u32 {
            1
        }
        fn apply(&self, doc: &mut DocumentMut) -> Result<(), MigrationError> {
            doc["step_b"] = toml_edit::value(42);
            Ok(())
        }
    }

    /// Migration, die immer scheitert — prüft Fehlerpropagation.
    struct FailingStep;
    impl ConfigMigration for FailingStep {
        fn from_version(&self) -> u32 {
            0
        }
        fn apply(&self, _doc: &mut DocumentMut) -> Result<(), MigrationError> {
            Err(MigrationError::Apply {
                from_version: 0,
                reason: "absichtlich".to_owned(),
            })
        }
    }

    fn tmp_config(name: &str) -> PathBuf {
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
        std::fs::create_dir_all(&dir).expect("temp dir anlegen"); // Testaufbau
        dir.push("config.toml");
        dir
    }

    #[test]
    fn test_run_chain_zero_to_two_applies_both() {
        let path = tmp_config("chain");
        std::fs::write(&path, "config_version = 0\n").expect("schreiben"); // Testaufbau

        let runner = MigrationRunner::new(vec![Box::new(AddStepA), Box::new(AddStepB)], 2);
        let reached = runner.run(&path).expect("migration"); // Testprüfung
        assert_eq!(reached, 2);

        let text = std::fs::read_to_string(&path).expect("lesen"); // Testprüfung
        let doc = text.parse::<DocumentMut>().expect("parse"); // Testprüfung
        assert_eq!(read_version(&doc), 2);
        assert_eq!(doc.get("step_a").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(doc.get("step_b").and_then(|v| v.as_integer()), Some(42));
    }

    #[test]
    fn test_run_missing_file_starts_from_zero() {
        let path = tmp_config("missing");
        // Datei existiert bewusst nicht.
        let runner = MigrationRunner::new(vec![Box::new(AddStepA), Box::new(AddStepB)], 2);
        let reached = runner.run(&path).expect("migration"); // Testprüfung
        assert_eq!(reached, 2);
        assert!(path.exists());
    }

    #[test]
    fn test_run_writes_backup_on_migration() {
        let path = tmp_config("backup");
        std::fs::write(&path, "config_version = 0\n").expect("schreiben"); // Testaufbau

        let runner = MigrationRunner::new(vec![Box::new(AddStepA), Box::new(AddStepB)], 2);
        runner.run(&path).expect("migration"); // Testprüfung

        let backup = backup_candidate(&path, 0);
        assert!(backup.exists(), "Backup config.toml.bak.0 muss entstehen");
        let backup_text = std::fs::read_to_string(&backup).expect("backup lesen"); // Testprüfung
        assert_eq!(backup_text, "config_version = 0\n");
    }

    #[test]
    fn test_run_backup_numbering_increments() {
        let path = tmp_config("numbering");
        std::fs::write(&path, "config_version = 0\n").expect("schreiben"); // Testaufbau

        // Erster Lauf 0→1 legt .bak.0 an.
        MigrationRunner::new(vec![Box::new(AddStepA)], 1)
            .run(&path)
            .expect("erster lauf"); // Testprüfung
        // Zweiter Lauf 1→2 legt .bak.1 an.
        MigrationRunner::new(vec![Box::new(AddStepB)], 2)
            .run(&path)
            .expect("zweiter lauf"); // Testprüfung

        assert!(backup_candidate(&path, 0).exists());
        assert!(backup_candidate(&path, 1).exists());
    }

    #[test]
    fn test_run_idempotent_no_op_when_current_equals_latest() {
        let path = tmp_config("idempotent");
        std::fs::write(&path, "config_version = 2\n").expect("schreiben"); // Testaufbau

        let runner = MigrationRunner::new(vec![Box::new(AddStepA), Box::new(AddStepB)], 2);
        let reached = runner.run(&path).expect("migration"); // Testprüfung
        assert_eq!(reached, 2);

        // Kein Backup, da No-op.
        assert!(!backup_candidate(&path, 0).exists());
        // Marker aus AddStepA/B dürfen NICHT gesetzt worden sein.
        let text = std::fs::read_to_string(&path).expect("lesen"); // Testprüfung
        let doc = text.parse::<DocumentMut>().expect("parse"); // Testprüfung
        assert!(doc.get("step_a").is_none());
        assert!(doc.get("step_b").is_none());
    }

    #[test]
    fn test_run_rejects_downgrade_with_version_error() {
        let path = tmp_config("downgrade");
        std::fs::write(&path, "config_version = 5\n").expect("schreiben"); // Testaufbau

        let runner = MigrationRunner::new(vec![Box::new(AddStepA)], 2);
        match runner.run(&path) {
            Err(MigrationError::Version { found, expected }) => {
                assert_eq!(found, 5);
                assert_eq!(expected, 2);
            }
            other => panic!("erwartete Version-Fehler, erhielt {other:?}"),
        }
    }

    #[test]
    fn test_run_propagates_apply_error() {
        let path = tmp_config("apply-fail");
        std::fs::write(&path, "config_version = 0\n").expect("schreiben"); // Testaufbau

        let runner = MigrationRunner::new(vec![Box::new(FailingStep)], 1);
        match runner.run(&path) {
            Err(MigrationError::Apply { from_version, .. }) => assert_eq!(from_version, 0),
            other => panic!("erwartete Apply-Fehler, erhielt {other:?}"),
        }
    }

    #[test]
    fn test_run_invalid_toml_returns_parse_error() {
        let path = tmp_config("badtoml");
        std::fs::write(&path, "a = = 1\n").expect("schreiben"); // Testaufbau

        let runner = MigrationRunner::new(vec![Box::new(AddStepA)], 1);
        assert!(matches!(
            runner.run(&path),
            Err(MigrationError::Parse { .. })
        ));
    }

    #[test]
    fn test_only_matching_migrations_applied() {
        let path = tmp_config("subset");
        std::fs::write(&path, "config_version = 1\n").expect("schreiben"); // Testaufbau

        // current=1: nur AddStepB (from_version 1) darf greifen, AddStepA nicht.
        let runner = MigrationRunner::new(vec![Box::new(AddStepA), Box::new(AddStepB)], 2);
        runner.run(&path).expect("migration"); // Testprüfung

        let text = std::fs::read_to_string(&path).expect("lesen"); // Testprüfung
        let doc = text.parse::<DocumentMut>().expect("parse"); // Testprüfung
        assert!(doc.get("step_a").is_none(), "AddStepA darf nicht greifen");
        assert_eq!(doc.get("step_b").and_then(|v| v.as_integer()), Some(42));
    }
}
