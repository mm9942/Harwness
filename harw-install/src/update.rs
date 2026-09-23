//! Update-/Versionsprüfung mit persistenten Metadaten (`~/.harw/version.json`).
//!
//! # Zweck
//! Implementiert die Update-Statusverwaltung gemäß Contract-Master
//! `docs/design/CONTRACT-setup-install.md`, Abschnitt „Crate `harw-install` /
//! `src/update.rs`". Es geht **ausschließlich** um das Lesen/Schreiben der lokalen
//! Versions-Metadaten und die Throttle-Logik — kein Netzwerkzugriff.
//!
//! # Verantwortung
//! - Besitzt [`VersionInfo`] (serialisierbarer Zustand der letzten Prüfung).
//! - Besitzt [`UpdateChecker`], der `version.json` unter der Home-Wurzel liest,
//!   schreibt, verwirft (`dismiss`) und die 20-Stunden-Drosselung berechnet.
//! - Delegiert Fehler an [`crate::error::UpdateError`].
//!
//! # Serde-Hinweis
//! `jiff` ist ohne aktiviertes `serde`-Feature eingebunden, daher trägt
//! [`jiff::Timestamp`] keine `Serialize`/`Deserialize`-Impls. Das Feld
//! `last_checked_at` wird deshalb über ein handgeschriebenes Serde-Modul
//! ([`ts_serde`]) als RFC-3339-String (`Timestamp::to_string`/`FromStr`)
//! (de-)serialisiert.
//!
//! # Nebenläufigkeit
//! [`UpdateChecker`] hält nur einen `PathBuf` und ist `Send + Sync`. Die
//! Datei-Operationen sind nicht gegen konkurrierende Schreiber innerhalb desselben
//! Prozesses synchronisiert; der Aufrufer stellt Exklusivität sicher, falls nötig.
//!
//! # Fehlertypen
//! Alle fehlbaren Operationen liefern [`crate::error::UpdateError`]
//! (`Io` / `Parse`).
//!
//! # Examples
//! ```rust,no_run
//! use harw_install::update::{UpdateChecker, VersionInfo};
//!
//! let checker = UpdateChecker::new("/home/u/.harw");
//! let now = jiff::Timestamp::now();
//! // 20h-Throttle: älter als TTL → fällig.
//! let _fällig = checker.is_stale(now, 20 * 60 * 60);
//! ```

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::UpdateError;

/// Dateiname der Versions-/Update-Metadaten relativ zur Home-Wurzel.
const VERSION_FILE: &str = "version.json";

/// Default-Drosselung zwischen zwei Update-Prüfungen: 20 Stunden in Sekunden.
///
/// Dient Aufrufern als kanonischer TTL-Wert für [`UpdateChecker::is_stale`].
pub const DEFAULT_TTL_SECS: i64 = 20 * 60 * 60;

/// Persistenter Zustand der zuletzt durchgeführten Update-Prüfung.
///
/// # Description
/// Wird als JSON unter `~/.harw/version.json` abgelegt. `last_checked_at` wird
/// über [`ts_serde`] als RFC-3339-String kodiert, da `jiff` ohne `serde`-Feature
/// eingebunden ist (siehe Modul-Doku).
///
/// # Concurrency
/// Reiner Werttyp, `Send + Sync`; kein gemeinsam veränderlicher Zustand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionInfo {
    /// Zuletzt bekannte neueste Version (z. B. `"1.4.2"`).
    pub latest_version: String,
    /// Zeitpunkt der letzten Prüfung (RFC-3339 serialisiert).
    #[serde(with = "ts_serde")]
    pub last_checked_at: jiff::Timestamp,
    /// Vom Nutzer verworfene Version, für die keine Meldung mehr erfolgt.
    #[serde(default)]
    pub dismissed_version: Option<String>,
}

/// Liest/schreibt die lokalen Update-Metadaten unter einer Home-Wurzel.
///
/// # Description
/// Kapselt den Zugriff auf `<home>/version.json`. Alle Methoden sind I/O-nah und
/// liefern [`UpdateError`] bei Fehlern. `is_stale` ist fehlertolerant (fail-open):
/// fehlt oder verweigert die Datei, gilt die Prüfung als fällig.
///
/// # Concurrency
/// Hält nur einen `PathBuf`; `Send + Sync`. Keine prozessinterne Sperre.
#[derive(Debug, Clone)]
pub struct UpdateChecker {
    /// Home-Wurzel (typischerweise `~/.harw`).
    home: PathBuf,
}

impl UpdateChecker {
    /// Erzeugt einen Checker für die angegebene Home-Wurzel.
    ///
    /// # Description
    /// Speichert die Wurzel; es findet kein I/O statt. Der Pfad zur
    /// Versionsdatei ist `<home>/version.json`.
    ///
    /// # Arguments
    /// - `home` (`impl Into<PathBuf>`): Home-Wurzel, Eigentum wird übernommen.
    ///
    /// # Returns
    /// Ein neuer [`UpdateChecker`].
    ///
    /// # Concurrency
    /// Threadsicher; nur Feldzuweisung.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_install::update::UpdateChecker;
    /// let checker = UpdateChecker::new("/home/u/.harw");
    /// let _ = checker;
    /// ```
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    /// Voller Pfad zur Versionsdatei (`<home>/version.json`).
    ///
    /// # Returns
    /// Neu erzeugter [`PathBuf`] der Datei.
    fn version_path(&self) -> PathBuf {
        self.home.join(VERSION_FILE)
    }

    /// Liest die Versions-Metadaten, falls vorhanden.
    ///
    /// # Description
    /// Liest `<home>/version.json` und deserialisiert den Inhalt. Eine fehlende
    /// Datei ist kein Fehler und liefert `Ok(None)`.
    ///
    /// # Returns
    /// `Ok(Some(VersionInfo))` bei vorhandener, lesbarer Datei; `Ok(None)`, wenn
    /// die Datei nicht existiert.
    ///
    /// # Errors
    /// - [`UpdateError::Io`][]:I/O-Fehler ungleich „nicht gefunden".
    /// - [`UpdateError::Parse`]: Datei enthält kein gültiges `VersionInfo`-JSON.
    ///
    /// # Concurrency
    /// Nur lesend; keine Sperre.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_install::update::UpdateChecker;
    /// let checker = UpdateChecker::new("/home/u/.harw");
    /// let maybe = checker.read()?;
    /// assert!(maybe.is_none() || maybe.is_some());
    /// # Ok::<(), harw_install::error::UpdateError>(())
    /// ```
    pub fn read(&self) -> Result<Option<VersionInfo>, UpdateError> {
        let path = self.version_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(UpdateError::Io {
                    path: path.display().to_string(),
                    source,
                });
            }
        };
        let info: VersionInfo = serde_json::from_slice(&bytes)?;
        Ok(Some(info))
    }

    /// Schreibt die Versions-Metadaten atomar-nah nach `<home>/version.json`.
    ///
    /// # Description
    /// Legt das Home-Verzeichnis bei Bedarf an und serialisiert `info` als
    /// eingerücktes JSON. Ein abschließender Zeilenumbruch wird angehängt.
    ///
    /// # Arguments
    /// - `info` (`&VersionInfo`): zu persistierender Zustand (geliehen).
    ///
    /// # Returns
    /// `Ok(())` bei Erfolg.
    ///
    /// # Errors
    /// - [`UpdateError::Io`][]: Verzeichnis-/Datei-Schreibfehler.
    /// - [`UpdateError::Parse`]: Serialisierung schlug fehl.
    ///
    /// # Concurrency
    /// Schreibend; keine prozessinterne Sperre.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_install::update::{UpdateChecker, VersionInfo};
    /// let checker = UpdateChecker::new("/home/u/.harw");
    /// let info = VersionInfo {
    ///     latest_version: "1.0.0".to_owned(),
    ///     last_checked_at: jiff::Timestamp::now(),
    ///     dismissed_version: None,
    /// };
    /// checker.write(&info)?;
    /// # Ok::<(), harw_install::error::UpdateError>(())
    /// ```
    pub fn write(&self, info: &VersionInfo) -> Result<(), UpdateError> {
        if let Err(source) = std::fs::create_dir_all(&self.home) {
            return Err(UpdateError::Io {
                path: self.home.display().to_string(),
                source,
            });
        }
        let path = self.version_path();
        let mut json = serde_json::to_string_pretty(info)?;
        json.push('\n');
        std::fs::write(&path, json).map_err(|source| UpdateError::Io {
            path: path.display().to_string(),
            source,
        })
    }

    /// Markiert eine Version als verworfen und persistiert den Zustand.
    ///
    /// # Description
    /// Liest den bestehenden Zustand und setzt `dismissed_version`. Existiert noch
    /// keine Datei, wird ein neuer Eintrag mit `latest_version = version` und
    /// `last_checked_at = Timestamp::now()` angelegt, damit `dismiss` idempotent
    /// persistiert.
    ///
    /// # Arguments
    /// - `version` (`&str`): zu verwerfende Version.
    ///
    /// # Returns
    /// `Ok(())` bei Erfolg.
    ///
    /// # Errors
    /// - [`UpdateError::Io`][]: Lese-/Schreibfehler.
    /// - [`UpdateError::Parse`]: (De-)Serialisierung schlug fehl.
    ///
    /// # Concurrency
    /// Read-modify-write ohne Sperre; der Aufrufer sichert Exklusivität.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_install::update::UpdateChecker;
    /// let checker = UpdateChecker::new("/home/u/.harw");
    /// checker.dismiss("1.4.2")?;
    /// # Ok::<(), harw_install::error::UpdateError>(())
    /// ```
    pub fn dismiss(&self, version: &str) -> Result<(), UpdateError> {
        let info = match self.read()? {
            Some(mut existing) => {
                existing.dismissed_version = Some(version.to_owned());
                existing
            }
            None => VersionInfo {
                latest_version: version.to_owned(),
                last_checked_at: jiff::Timestamp::now(),
                dismissed_version: Some(version.to_owned()),
            },
        };
        self.write(&info)
    }

    /// Prüft, ob die letzte Update-Prüfung älter als die TTL ist (20h-Throttle).
    ///
    /// # Description
    /// Liest den gespeicherten `last_checked_at` und vergleicht `now -
    /// last_checked_at` mit `ttl_secs`. Fehlt die Datei oder ist sie unlesbar,
    /// gilt die Prüfung fail-open als fällig (`true`). Liegt `last_checked_at` in
    /// der Zukunft (negative Differenz), gilt sie als **nicht** fällig.
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): aktueller Zeitpunkt (Wert übernommen).
    /// - `ttl_secs` (`i64`): erlaubtes Alter in Sekunden (z. B.
    ///   [`DEFAULT_TTL_SECS`]).
    ///
    /// # Returns
    /// `true`, wenn eine erneute Prüfung fällig ist, sonst `false`.
    ///
    /// # Concurrency
    /// Nur lesend; keine Sperre.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_install::update::{UpdateChecker, DEFAULT_TTL_SECS};
    /// let checker = UpdateChecker::new("/home/u/.harw");
    /// let due = checker.is_stale(jiff::Timestamp::now(), DEFAULT_TTL_SECS);
    /// let _ = due;
    /// ```
    pub fn is_stale(&self, now: jiff::Timestamp, ttl_secs: i64) -> bool {
        match self.read() {
            Ok(Some(info)) => {
                let elapsed = now.as_second() - info.last_checked_at.as_second();
                elapsed > ttl_secs
            }
            // Kein Zustand oder unlesbar → fail-open: Prüfung ist fällig.
            Ok(None) | Err(_) => true,
        }
    }
}

/// Handgeschriebenes Serde-Modul für [`jiff::Timestamp`] via RFC-3339-String.
///
/// # Description
/// `jiff` ist ohne `serde`-Feature eingebunden, daher kann [`jiff::Timestamp`]
/// nicht direkt (de-)serialisiert werden. Dieses Modul kodiert den Zeitstempel
/// über `Timestamp::to_string` (RFC 3339) und parst ihn per `FromStr` zurück.
/// Verwendung an Feldern via `#[serde(with = "ts_serde")]`.
mod ts_serde {
    use std::str::FromStr;

    use jiff::Timestamp;
    use serde::{Deserialize, Deserializer, Serializer};

    /// Serialisiert einen [`Timestamp`] als RFC-3339-String.
    ///
    /// # Errors
    /// Reicht Serializer-Fehler durch.
    pub fn serialize<S>(ts: &Timestamp, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&ts.to_string())
    }

    /// Deserialisiert einen [`Timestamp`] aus einem RFC-3339-String.
    ///
    /// # Errors
    /// [`serde::de::Error`], wenn der String kein gültiger Zeitstempel ist.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Timestamp, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Timestamp::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::str::FromStr;

    /// Fester Referenz-Zeitstempel für deterministische Tests.
    fn fixed_ts() -> TestResult<jiff::Timestamp> {
        jiff::Timestamp::from_str("2024-01-01T00:00:00Z").map_err(ctx("gültiger Zeitstempel"))
    }

    /// Legt einen temporären Home-Ordner unter dem OS-Temp-Verzeichnis an.
    /// Ein prozessweiter Atomic-Zähler garantiert Eindeutigkeit pro Aufruf,
    /// damit parallel laufende Tests nicht dieselbe `version.json` teilen.
    fn temp_home() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut base = std::env::temp_dir();
        base.push(format!("harw-update-test-{}-{n}", std::process::id()));
        base
    }

    #[test]
    fn test_write_read_roundtrip() -> TestResult {
        let home = temp_home();
        let checker = UpdateChecker::new(&home);
        let info = VersionInfo {
            latest_version: "1.2.3".to_owned(),
            last_checked_at: fixed_ts()?,
            dismissed_version: Some("1.2.2".to_owned()),
        };
        checker.write(&info).map_err(ctx("write erfolgreich"))?;
        let read_back = checker.read().map_err(ctx("read erfolgreich"))?;
        assert_eq!(read_back, Some(info));
        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[test]
    fn test_read_missing_returns_none() -> TestResult {
        let home = temp_home();
        let checker = UpdateChecker::new(&home);
        let result = checker.read().map_err(ctx("read ohne Datei ist Ok"))?;
        assert_eq!(result, None);
        Ok(())
    }

    #[test]
    fn test_dismiss_creates_and_persists() -> TestResult {
        let home = temp_home();
        let checker = UpdateChecker::new(&home);
        checker
            .dismiss("9.9.9")
            .map_err(ctx("dismiss erfolgreich"))?;
        let info = checker
            .read()
            .map_err(ctx("read erfolgreich"))?
            .ok_or(TestError::Missing("Info vorhanden"))?;
        assert_eq!(info.dismissed_version, Some("9.9.9".to_owned()));
        assert_eq!(info.latest_version, "9.9.9");
        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[test]
    fn test_dismiss_updates_existing() -> TestResult {
        let home = temp_home();
        let checker = UpdateChecker::new(&home);
        let info = VersionInfo {
            latest_version: "2.0.0".to_owned(),
            last_checked_at: fixed_ts()?,
            dismissed_version: None,
        };
        checker.write(&info).map_err(ctx("write erfolgreich"))?;
        checker
            .dismiss("2.0.0")
            .map_err(ctx("dismiss erfolgreich"))?;
        let updated = checker
            .read()
            .map_err(ctx("read erfolgreich"))?
            .ok_or(TestError::Missing("Info vorhanden"))?;
        assert_eq!(updated.latest_version, "2.0.0");
        assert_eq!(updated.dismissed_version, Some("2.0.0".to_owned()));
        assert_eq!(updated.last_checked_at, fixed_ts()?);
        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[test]
    fn test_is_stale_true_when_beyond_ttl() -> TestResult {
        let home = temp_home();
        let checker = UpdateChecker::new(&home);
        let info = VersionInfo {
            latest_version: "1.0.0".to_owned(),
            last_checked_at: fixed_ts()?,
            dismissed_version: None,
        };
        checker.write(&info).map_err(ctx("write erfolgreich"))?;
        // 25 Stunden später bei TTL 20h → fällig.
        let now = jiff::Timestamp::from_str("2024-01-02T01:00:00Z").map_err(ctx("gültig"))?;
        assert!(checker.is_stale(now, DEFAULT_TTL_SECS));
        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[test]
    fn test_is_stale_false_within_ttl() -> TestResult {
        let home = temp_home();
        let checker = UpdateChecker::new(&home);
        let info = VersionInfo {
            latest_version: "1.0.0".to_owned(),
            last_checked_at: fixed_ts()?,
            dismissed_version: None,
        };
        checker.write(&info).map_err(ctx("write erfolgreich"))?;
        // 10 Stunden später bei TTL 20h → nicht fällig.
        let now = jiff::Timestamp::from_str("2024-01-01T10:00:00Z").map_err(ctx("gültig"))?;
        assert!(!checker.is_stale(now, DEFAULT_TTL_SECS));
        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[test]
    fn test_is_stale_true_when_missing_file() -> TestResult {
        let home = temp_home();
        let checker = UpdateChecker::new(&home);
        // Keine Datei → fail-open fällig.
        assert!(checker.is_stale(fixed_ts()?, DEFAULT_TTL_SECS));
        Ok(())
    }

    #[test]
    fn test_is_stale_false_when_last_check_in_future() -> TestResult {
        let home = temp_home();
        let checker = UpdateChecker::new(&home);
        let info = VersionInfo {
            latest_version: "1.0.0".to_owned(),
            last_checked_at: fixed_ts()?,
            dismissed_version: None,
        };
        checker.write(&info).map_err(ctx("write erfolgreich"))?;
        // now liegt VOR last_checked_at → negative Differenz → nicht fällig.
        let now = jiff::Timestamp::from_str("2023-12-31T00:00:00Z").map_err(ctx("gültig"))?;
        assert!(!checker.is_stale(now, DEFAULT_TTL_SECS));
        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }
}
