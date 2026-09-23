//! Die Quellen-Schnittstelle: `trait AuditSource` und eine Testattrappe.
//!
//! # Verantwortungsbereich
//! [`AuditSource`] ist die einzige Nahtstelle, über die `harw-dod-authlog`
//! (Knoten AW2-15) an Audit-Records kommt. Es gibt zwei Implementierungen:
//! [`FixtureAuditSource`] hier (keine Berechtigung, kein Kernel — Teil der
//! normalen API, nicht hinter `#[cfg(test)]`, weil AW2-15 sie für seine
//! eigenen Tests braucht) und
//! [`crate::socket::NetlinkAuditSource`] (`#[cfg(target_os = "linux")]`,
//! braucht `CAP_AUDIT_READ`/root).
//!
//! # Exportierte Typen
//! [`AuditSource`], [`FixtureAuditSource`].
//!
//! # Nebenläufigkeit
//! `AuditSource: Send + Sync` ist Teil des Vertrags — ein Sentinel darf eine
//! Quelle aus einem beliebigen Thread pollen. [`FixtureAuditSource`] hält
//! ihre Records unveränderlich (kein `Mutex`, keine innere Veränderlichkeit)
//! und liefert bei jedem Aufruf denselben Inhalt zurück; das macht sie ohne
//! Sperren aus beliebig vielen Threads gleichzeitig lesbar.
//!
//! # Fehler
//! [`crate::error::NetlinkError`], siehe dort.
//!
//! # Examples
//! ```rust
//! use harw_dod_netlink::{AuditSource, FixtureAuditSource, RawRecord};
//! use std::time::Duration;
//!
//! let source = FixtureAuditSource::new([RawRecord::new("type=SYSCALL auid=1000")]);
//! let records = source.read_records(Duration::from_secs(1)).expect("Fixture scheitert nie");
//! assert_eq!(records.len(), 1);
//! ```

use std::time::Duration;

use crate::error::NetlinkError;
use crate::record::RawRecord;

/// Eine Quelle für Audit-Records.
///
/// # Description
/// Abstrahiert über den eigentlichen `AUDIT`-Netlink-Socket
/// ([`crate::socket::NetlinkAuditSource`], nur unter Linux und mit
/// Berechtigung verfügbar) und eine Testattrappe ([`FixtureAuditSource`]).
/// `harw-dod-authlog` programmiert ausschließlich gegen diesen Trait und
/// bindet sich nie an eine konkrete Implementierung.
pub trait AuditSource: Send + Sync {
    /// Liest die nächsten Records, blockierend bis `timeout`.
    ///
    /// # Arguments
    /// - `timeout` (`std::time::Duration`): maximale Wartezeit, bis
    ///   mindestens ein Record verfügbar ist. Ein Ablauf ohne neue Daten ist
    ///   kein Fehler — die Implementierung liefert dann einen leeren `Vec`.
    ///
    /// # Returns
    /// Die seit dem letzten Aufruf neu verfügbaren Records, in
    /// Empfangsreihenfolge. Kann leer sein.
    ///
    /// # Errors
    /// - [`NetlinkError`]: quellenspezifisch — bei
    ///   [`crate::socket::NetlinkAuditSource`] insbesondere
    ///   [`harw_dod_cap::SensorError::SourceUnavailable`], wenn der Socket
    ///   nicht (mehr) benutzbar ist.
    fn read_records(&self, timeout: Duration) -> Result<Vec<RawRecord>, NetlinkError>;
}

/// Eine Testattrappe, die eine feste Liste von Records zurückgibt.
///
/// # Description
/// **Keine Wegwerfattrappe:** Teil der normalen, öffentlichen API dieser
/// Crate, weil `harw-dod-authlog` sie für seine eigenen Tests braucht — nicht
/// hinter `#[cfg(test)]` versteckt. Jeder Aufruf von [`Self::read_records`]
/// gibt dieselbe, bei [`Self::new`] übergebene Liste zurück (kein
/// Verbrauchen, kein interner Zustand); `timeout` wird ignoriert, weil eine
/// Attrappe nie blockiert.
#[derive(Debug, Clone, Default)]
pub struct FixtureAuditSource {
    records: Vec<RawRecord>,
}

impl FixtureAuditSource {
    /// Baut eine Attrappe aus einer festen Liste von Records.
    ///
    /// # Arguments
    /// - `records` (`impl IntoIterator<Item = RawRecord>`): die Records, die
    ///   jeder [`Self::read_records`]-Aufruf zurückgibt.
    ///
    /// # Returns
    /// Eine `FixtureAuditSource`, deren [`Self::read_records`] stets exakt
    /// diese Records liefert.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_netlink::{FixtureAuditSource, RawRecord};
    ///
    /// let source = FixtureAuditSource::new([RawRecord::new("type=SYSCALL")]);
    /// ```
    #[must_use]
    pub fn new(records: impl IntoIterator<Item = RawRecord>) -> Self {
        Self {
            records: records.into_iter().collect(),
        }
    }
}

impl AuditSource for FixtureAuditSource {
    /// Liefert die bei [`Self::new`] eingesetzten Records zurück.
    ///
    /// # Errors
    /// Keine — diese Implementierung schlägt nie fehl.
    fn read_records(&self, _timeout: Duration) -> Result<Vec<RawRecord>, NetlinkError> {
        Ok(self.records.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::{AuditSource, FixtureAuditSource};
    use crate::record::RawRecord;
    use crate::test_support::TestResult;
    use std::time::Duration;

    #[test]
    fn test_fixture_audit_source_returns_inserted_records() -> TestResult {
        let inserted = vec![
            RawRecord::new("type=SYSCALL auid=1000"),
            RawRecord::new("type=USER_AUTH auid=0"),
        ];
        let source = FixtureAuditSource::new(inserted.clone());

        let read = source.read_records(Duration::from_millis(1))?;

        assert_eq!(read, inserted);
        Ok(())
    }

    #[test]
    fn test_fixture_audit_source_returns_same_records_on_repeated_calls() -> TestResult {
        let source = FixtureAuditSource::new([RawRecord::new("type=SYSCALL")]);

        let first = source.read_records(Duration::ZERO)?;
        let second = source.read_records(Duration::ZERO)?;

        assert_eq!(first, second);
        Ok(())
    }

    #[test]
    fn test_fixture_audit_source_empty_fixture_returns_empty_vec() -> TestResult {
        let source = FixtureAuditSource::new(Vec::<RawRecord>::new());
        let read = source.read_records(Duration::ZERO)?;
        assert!(read.is_empty());
        Ok(())
    }
}
