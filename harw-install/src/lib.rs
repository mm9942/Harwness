//! Install-/Betriebs-Lebenszyklus: Detection, Service-Verwaltung, doctor,
//! Update, Config-Migration und Uninstall.
//!
//! Siehe `docs/design/CONTRACT-setup-install.md`. Re-Exports werden nach dem
//! Fanout in der Integration ergänzt.
//!
//! # Deployment-Assets (Crypto-Masterplan v2 §22/§40, H10)
//! [`deployment`] bettet **jede** Datei unter `deploy/` (systemd-Units der
//! DoD- und Infrastruktur-Daemons, `sysusers.d`, `tmpfiles.d`) per
//! `include_str!` im Produktionscode ein — `deploy/` ist die einzige Quelle.
//! `harw install --print-systemd [UNIT]` gibt die eingebetteten Units aus;
//! `dod/scripts/install.sh` installiert dieselben Dateien direkt aus
//! `deploy/`. Paritätstests prüfen `deploy/` ↔ Einbettung ↔ DoD-Manifest.
//!
//! # Die vier DoD-Systemunits (Knoten AW7-03, abgeglichen in H10)
//! [`dod_units`] trägt die Rechtematrix der vier Ausbauprogramm-Binaries
//! (`harw-sentinel`, `harw-probe-fs`, `harw-probe-bpf`, `harw-warden`) und
//! prüft die eingebetteten Unit-Texte: unprivilegiert für den Sentinel,
//! `CAP_SYS_ADMIN` für die fanotify-Sonde, `CAP_BPF CAP_PERFMON` für die
//! eBPF-Sonde, `CAP_DAC_OVERRIDE CAP_NET_ADMIN` unter eigenem Nutzer für den
//! Warden. Die Landlock-Asymmetrie (Entscheidung Nr. 4) läuft ausschließlich
//! im jeweiligen Binary. **Wohin die Units gehören:** ausschließlich
//! Systemunits — eine Nutzerinstanz (`~/.config/systemd/user/`, der einzige
//! Ort, den [`service_systemd::SystemdServiceManager`] kennt) kann keine
//! Capabilities vergeben. Dieses Crate installiert, aktiviert und startet
//! keine Systemunit; es beschreibt und druckt sie nur.

#![forbid(unsafe_code)]

pub mod context;
pub mod deployment;
pub mod doctor;
pub mod dod_units;
pub mod error;
pub mod migration;
pub mod pathscope;
pub mod platform;
pub mod release;
pub mod service;
pub mod service_launchd;
pub mod service_schtasks;
pub mod service_systemd;
pub mod uninstall;
pub mod update;

pub use context::{InstallContext, InstallMethod};
pub use deployment::{
    AssetKind, DEPLOYMENT_ASSETS, DOD_PACKAGE_ASSETS, EmbeddedDeploymentAsset, RenderPaths,
    UnknownUnitError, print_systemd,
};
pub use doctor::{
    AuditIntegrityCheck, AuditIntegrityEvidence, CheckOutcome, DoctorCheck, KekFilePermsCheck,
    KekFilePermsEvidence, default_checks, run_all,
};
pub use dod_units::{ParsedUnit, UNIT_CLASSES, UnitClass};
pub use error::{DoctorError, InstallError, MigrationError, PathError, ServiceError, UpdateError};
pub use migration::{ConfigMigration, MigrationRunner};
pub use pathscope::PathScope;
pub use platform::{Os, Platform};
pub use release::{ReleaseAsset, ReleaseError, ReleaseInfo, validate_release_archive};
pub use service::{
    ServiceKind, ServiceManager, ServiceSpec, ServiceStatus, detect_service_manager,
};
pub use uninstall::{CleanupPlan, UninstallScope};
pub use update::{UpdateChecker, VersionInfo};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
