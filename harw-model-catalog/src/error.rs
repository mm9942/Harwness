//! Fehlertypen des `harw-model-catalog` Crates.
//!
//! Spezifikation: `docs/design/CONTRACT-setup-install.md`, Abschnitt
//! „harw-model-catalog / src/error.rs".
//!
//! # Verantwortung
//! Dieses Modul besitzt den crate-weiten Fehler-Enum [`CatalogError`] samt
//! [`CatalogResult`] Alias. Es delegiert keine Fehlerbehandlung an andere
//! Module; alle fehlbaren Operationen des Crates propagieren über diesen Typ.
//!
//! # Exportierte Typen
//! - [`CatalogError`]: hand-geschriebener Fehler-Enum (kein `anyhow`/`thiserror`).
//! - [`CatalogResult`]: `Result<T, CatalogError>` Alias.
//!
//! # Fehlerquellen
//! Parsing (`Parse`), Datei-/IO-Operationen (`Io`) und HTTP-Abrufe (`Http`).
//!
//! # Concurrency
//! [`CatalogError`] ist `Send + Sync` (alle Felder sind es), damit über
//! Thread-Grenzen propagierbar. Keine internen Locks oder Threads.
//!
//! # Examples
//! ```rust,no_run
//! use harw_model_catalog::error::{CatalogError, CatalogResult};
//!
//! fn load() -> CatalogResult<()> {
//!     Err(CatalogError::Parse("unexpected token".to_owned()))
//! }
//! ```

use std::error::Error as StdError;
use std::fmt;

use crate::runtime::DelegationPolicy;

/// Kurzform für `Result<T, CatalogError>`.
///
/// # Description
/// Kanonischer Ergebnistyp aller fehlbaren Funktionen des Crates.
///
/// # Examples
/// ```rust,no_run
/// use harw_model_catalog::error::CatalogResult;
///
/// fn parse() -> CatalogResult<u32> {
///     Ok(42)
/// }
/// ```
pub type CatalogResult<T> = Result<T, CatalogError>;

/// Fehler bei der Validierung eines Runtime-Profils.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeProfileValidationError {
    /// Ein Retry-Budget muss mindestens einen Wiederholungsversuch erlauben.
    ZeroRetryLimit,
    /// Ein Retry-Budget überschreitet die feste Sicherheitsobergrenze.
    RetryLimitExceedsMaximum { actual: u8, maximum: u8 },
    /// Ein Retry-Backoff muss positiv sein.
    ZeroRetryBackoff,
    /// Ein Retry-Backoff überschreitet die feste Sicherheitsobergrenze.
    RetryBackoffExceedsMaximum { actual: u32, maximum: u32 },
    /// Ein Turn muss mindestens einen Tool-Slot haben.
    ZeroParallelToolLimit,
    /// Die Tool-Parallelität überschreitet die feste Sicherheitsobergrenze.
    ParallelToolLimitExceedsMaximum { actual: u8, maximum: u8 },
    /// Die Child-Fanout-Grenze überschreitet die feste Sicherheitsobergrenze.
    ChildFanoutExceedsMaximum { actual: u8, maximum: u8 },
    /// Eine Delegation verbietende Policy darf keinen Child-Fanout erlauben.
    ForbiddenDelegationAllowsChildren { actual: u8 },
    /// Eine Delegation erlaubende Policy benötigt mindestens einen Child-Slot.
    DelegationPolicyHasNoChildSlots { policy: DelegationPolicy },
}

impl fmt::Display for RuntimeProfileValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroRetryLimit => formatter.write_str("max_retries must be greater than zero"),
            Self::RetryLimitExceedsMaximum { actual, maximum } => {
                write!(formatter, "max_retries {actual} exceeds maximum {maximum}")
            }
            Self::ZeroRetryBackoff => formatter.write_str("backoff_ms must be greater than zero"),
            Self::RetryBackoffExceedsMaximum { actual, maximum } => {
                write!(formatter, "backoff_ms {actual} exceeds maximum {maximum}")
            }
            Self::ZeroParallelToolLimit => {
                formatter.write_str("max_parallel_tools must be greater than zero")
            }
            Self::ParallelToolLimitExceedsMaximum { actual, maximum } => {
                write!(
                    formatter,
                    "max_parallel_tools {actual} exceeds maximum {maximum}"
                )
            }
            Self::ChildFanoutExceedsMaximum { actual, maximum } => {
                write!(
                    formatter,
                    "max_child_fanout {actual} exceeds maximum {maximum}"
                )
            }
            Self::ForbiddenDelegationAllowsChildren { actual } => write!(
                formatter,
                "delegation_policy forbidden requires max_child_fanout 0, got {actual}"
            ),
            Self::DelegationPolicyHasNoChildSlots { policy } => write!(
                formatter,
                "delegation_policy {policy:?} requires max_child_fanout greater than zero"
            ),
        }
    }
}

impl StdError for RuntimeProfileValidationError {}

/// Crate-weiter Fehler-Enum für den Modell-Katalog.
///
/// # Description
/// Deckt die drei Fehlerklassen des Crates ab. Jede Variante trägt genügend
/// Kontext, um die Ursache ohne Blick in den Quellcode zu verstehen.
///
/// # Variants
/// - [`CatalogError::Parse`][]: Text-/JSON-/TOML-Parsing fehlgeschlagen; enthält
///   eine menschenlesbare Beschreibung.
/// - [`CatalogError::Io`][]: Datei-/IO-Operation fehlgeschlagen; enthält den
///   betroffenen Pfad und die zugrunde liegende [`std::io::Error`].
/// - [`CatalogError::Http`][]: HTTP-Abruf fehlgeschlagen; enthält eine
///   menschenlesbare Beschreibung.
///
/// # Concurrency
/// `Send + Sync`; sicher über Thread-Grenzen zu propagieren.
pub enum CatalogError {
    /// Parsing-Fehler mit menschenlesbarer Beschreibung.
    Parse(String),
    /// IO-Fehler mit betroffenem Pfad und zugrunde liegender Ursache.
    Io {
        /// Pfad der betroffenen Datei/Ressource (leer, falls unbekannt).
        path: String,
        /// Ursprünglicher IO-Fehler.
        source: std::io::Error,
    },
    /// HTTP-Fehler mit menschenlesbarer Beschreibung.
    Http(String),
}

impl CatalogError {
    /// Konstruiert eine [`CatalogError::Io`] Variante aus Pfad und IO-Fehler.
    ///
    /// # Description
    /// Bequemer Konstruktor, um einen [`std::io::Error`] mit dem betroffenen
    /// Pfad-Kontext anzureichern.
    ///
    /// # Arguments
    /// - `path` (`impl Into<String>`): Pfad der betroffenen Ressource.
    /// - `source` (`std::io::Error`): der ursprüngliche IO-Fehler (übernommen).
    ///
    /// # Returns
    /// Eine [`CatalogError::Io`] Instanz.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_model_catalog::error::CatalogError;
    /// use std::io;
    ///
    /// let err = CatalogError::io(
    ///     "/home/user/.harw/cache/models_dev.json",
    ///     io::Error::new(io::ErrorKind::NotFound, "missing"),
    /// );
    /// ```
    pub fn io(path: impl Into<String>, source: std::io::Error) -> Self {
        CatalogError::Io {
            path: path.into(),
            source,
        }
    }
}

impl fmt::Display for CatalogError {
    /// Formatiert den Fehler menschenlesbar, ohne internen Jargon.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogError::Parse(msg) => write!(f, "catalog parse error: {msg}"),
            CatalogError::Io { path, source } => {
                if path.is_empty() {
                    write!(f, "catalog io error: {source}")
                } else {
                    write!(f, "catalog io error at '{path}': {source}")
                }
            }
            CatalogError::Http(msg) => write!(f, "catalog http error: {msg}"),
        }
    }
}

impl fmt::Debug for CatalogError {
    /// Delegiert an [`Display`](fmt::Display) (eine Implementierung, keine Duplikation).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl StdError for CatalogError {
    /// Liefert die zugrunde liegende Ursache für [`CatalogError::Io`].
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            CatalogError::Io { source, .. } => Some(source),
            CatalogError::Parse(_) | CatalogError::Http(_) => None,
        }
    }
}

impl From<std::io::Error> for CatalogError {
    /// Konvertiert einen [`std::io::Error`] in [`CatalogError::Io`] mit leerem Pfad.
    ///
    /// # Description
    /// Ermöglicht `?`-Propagation. Wenn der Pfad-Kontext bekannt ist, sollte
    /// stattdessen [`CatalogError::io`] verwendet werden.
    fn from(source: std::io::Error) -> Self {
        CatalogError::Io {
            path: String::new(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_parse_contains_message() {
        let err = CatalogError::Parse("bad token".to_owned());
        assert_eq!(err.to_string(), "catalog parse error: bad token");
    }

    #[test]
    fn test_display_http_contains_message() {
        let err = CatalogError::Http("503 upstream".to_owned());
        assert_eq!(err.to_string(), "catalog http error: 503 upstream");
    }

    #[test]
    fn test_display_io_with_path() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let err = CatalogError::io("/tmp/x.json", io_err);
        assert_eq!(
            err.to_string(),
            "catalog io error at '/tmp/x.json': missing"
        );
    }

    #[test]
    fn test_display_io_empty_path_from_impl() {
        let io_err = std::io::Error::other("boom");
        let err: CatalogError = io_err.into();
        assert_eq!(err.to_string(), "catalog io error: boom");
    }

    #[test]
    fn test_source_present_for_io_absent_otherwise() {
        let io_err = std::io::Error::other("boom");
        let err = CatalogError::io("/p", io_err);
        assert!(err.source().is_some());
        assert!(CatalogError::Parse("x".to_owned()).source().is_none());
    }

    #[test]
    fn test_debug_delegates_to_display() {
        let err = CatalogError::Parse("z".to_owned());
        assert_eq!(format!("{err:?}"), format!("{err}"));
    }
}
