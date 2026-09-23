//! Install-/Betriebs-Lebenszyklus: Detection, Service-Verwaltung, doctor,
//! Update, Config-Migration und Uninstall.
//!
//! Siehe `docs/design/CONTRACT-setup-install.md`. Re-Exports werden nach dem
//! Fanout in der Integration ergänzt.
//!
//! # Die vier DoD-Systemunits (Knoten AW7-03)
//! [`dod_units`] trägt die Zuordnung der vier Ausbauprogramm-Binaries
//! (`harw-sentinel`, `harw-probe-fs`, `harw-probe-bpf`, `harw-warden`) zu
//! ihren statischen Unit-Texten unter `deploy/systemd/` und prüft sie: je
//! Klasse genau die Fähigkeiten, die diese Klasse nennt — unprivilegiert
//! für den Sentinel, `CAP_SYS_ADMIN` für die fanotify-Sonde, `CAP_BPF` für
//! die eBPF-Sonde, `CAP_SYS_ADMIN` (eine Annahme dieses Knotens, siehe dort)
//! für den Warden. Die Landlock-Asymmetrie dieses Programms (Entscheidung
//! Nr. 4) läuft ausschließlich im jeweiligen Binary selbst: der Sentinel
//! **degradiert** bei fehlender Landlock-Unterstützung, die drei
//! privilegierten Binaries brechen **hart** ab — keine dieser Units erzwingt
//! diese Vorbedingung ein zweites Mal, sie beschreibt nur den Betrieb.
//! **Wohin die Units gehören:** ausschließlich `/etc/systemd/system/`
//! (Systemverwaltung) — eine Nutzerinstanz (`~/.config/systemd/user/`, der
//! einzige Ort, den [`service_systemd::SystemdServiceManager`] heute kennt)
//! kann keine Capabilities vergeben und stellt keinen für mehrere
//! Systemnutzer gemeinsam erreichbaren `/run/`-Pfad bereit. Das ist ein
//! **Befund**, kein bereits gelöster Zustand: dieses Crate installiert,
//! aktiviert und startet nichts — siehe [`dod_units`]-Moduldoku für die
//! vollständige Begründung.

#![forbid(unsafe_code)]

pub mod context;
pub mod dod_units;
pub mod doctor;
pub mod error;
pub mod migration;
pub mod pathscope;
pub mod platform;
pub mod service;
pub mod service_launchd;
pub mod service_schtasks;
pub mod service_systemd;
pub mod uninstall;
pub mod update;

pub use context::{InstallContext, InstallMethod};
pub use dod_units::{ParsedUnit, UnitClass, UNIT_CLASSES};
pub use doctor::{
    AuditIntegrityCheck, AuditIntegrityEvidence, CheckOutcome, DoctorCheck, KekFilePermsCheck,
    KekFilePermsEvidence, default_checks, run_all,
};
pub use error::{DoctorError, InstallError, MigrationError, PathError, ServiceError, UpdateError};
pub use migration::{ConfigMigration, MigrationRunner};
pub use pathscope::PathScope;
pub use platform::{Os, Platform};
pub use service::{
    ServiceKind, ServiceManager, ServiceSpec, ServiceStatus, detect_service_manager,
};
pub use uninstall::{CleanupPlan, UninstallScope};
pub use update::{UpdateChecker, VersionInfo};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
