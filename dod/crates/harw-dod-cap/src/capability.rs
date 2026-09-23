//! Zugriffsvokabular: welche Fähigkeit ein Sensor benutzt und in welche
//! Berechtigungsklasse sie fällt.
//!
//! # Verantwortungsbereich
//! [`Capability`] ist ein geschlossenes Enum mit den vierzehn Fähigkeiten aus
//! Vertrag Abschnitt F. Eine neue Fähigkeit ist damit immer eine bewusste
//! Entscheidung mit Eintrag in der Rechtematrix, kein freier String, den eine
//! Sensor-Crate sich selbst ausdenken könnte. [`CapabilityClass`] bestimmt,
//! in welches Binary eine Crate anhand ihrer Fähigkeit gehören darf.
//!
//! # Exportierte Typen
//! [`Capability`], [`CapabilityClass`].
//!
//! # Nebenläufigkeit
//! Beide Typen sind reine, kopierbare Werte (`Copy`) ohne innere
//! Veränderlichkeit: `Send + Sync`, beliebig zwischen Threads teilbar.
//!
//! # Fehler
//! Keine — `class()` und `probe()` sind totale, panikfreie `const fn`.
//!
//! # Examples
//! ```rust
//! use harw_dod_cap::{Capability, CapabilityClass};
//!
//! let cap = Capability::ReadAuditNetlink;
//! assert_eq!(cap.class(), CapabilityClass::Netlink);
//! assert_eq!(cap.probe(), "audit-netlink");
//! ```

/// Was eine Crate lesen darf.
///
/// # Description
/// Geschlossen: eine neue Fähigkeit ist eine Entscheidung mit Eintrag in der
/// Rechtematrix, kein freier String. Jede der vierzehn Varianten entspricht
/// genau einer Sensor-Crate im AW0-Ausbauprogramm.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    /// Thermalzonen unter `/sys/class/thermal`.
    ReadSysfsThermal,
    /// CPU-Zeitstatistik aus `/proc/stat`.
    ReadProcStat,
    /// Speicherstatistik aus `/proc/meminfo`.
    ReadProcMeminfo,
    /// Blockgeräte unter `/sys/block`.
    ReadSysfsBlock,
    /// Netzwerk-Interface-Zähler aus `/proc/net/dev`.
    ReadProcNetDev,
    /// GPU-/DRM-Zustand unter `/sys/class/drm`.
    ReadSysfsDrm,
    /// Cgroup-v2-Hierarchie unter `/sys/fs/cgroup`.
    ReadCgroupV2,
    /// Allgemeine Netzwerktabellen unter `/proc/net`.
    ReadProcNet,
    /// Systemd-Journal-Einträge.
    ReadJournal,
    /// Audit-Ereignisse über den `AUDIT`-Netlink-Kanal.
    ReadAuditNetlink,
    /// Bereits erzeugte Scan-Berichte des Harness.
    ReadScanReports,
    /// Der Workspace-Abhängigkeitsgraph des Harness.
    ReadWorkspaceGraph,
    /// Dateisystem-Änderungsereignisse (fanotify/inotify).
    WatchFilesystem,
    /// Laden eines BPF-Programms in den Kernel.
    LoadBpfProgram,
}

/// Die Berechtigungsklasse einer Fähigkeit.
///
/// # Description
/// Bestimmt, in welches Binary eine Crate gehören darf: ein unprivilegierter
/// Datei-/Sysfs-Leser gehört in ein anderes Binary als ein Sensor, der einen
/// Netlink-Socket öffnet, einen Dateisystem-Watcher registriert oder ein
/// BPF-Programm lädt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CapabilityClass {
    /// Reines Lesen aus `/proc` oder `/sys`; keine besondere Kernel-Fähigkeit.
    Unprivileged,
    /// Erfordert einen Netlink-Socket (z. B. `AUDIT`).
    Netlink,
    /// Erfordert Dateisystem-Beobachtung (fanotify/inotify).
    FileWatch,
    /// Erfordert das Laden eines BPF-Programms in den Kernel.
    Bpf,
}

impl Capability {
    /// Die Klasse dieser Fähigkeit.
    ///
    /// # Description
    /// Reine Zuordnungstabelle, siehe [`CapabilityClass`] für die Bedeutung
    /// jeder Klasse. Elf der vierzehn Fähigkeiten sind
    /// [`CapabilityClass::Unprivileged`]; je eine Fähigkeit fällt auf
    /// [`CapabilityClass::Netlink`], [`CapabilityClass::FileWatch`] und
    /// [`CapabilityClass::Bpf`].
    ///
    /// # Returns
    /// Die [`CapabilityClass`] dieser Fähigkeit.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::{Capability, CapabilityClass};
    ///
    /// assert_eq!(Capability::ReadProcStat.class(), CapabilityClass::Unprivileged);
    /// assert_eq!(Capability::WatchFilesystem.class(), CapabilityClass::FileWatch);
    /// assert_eq!(Capability::LoadBpfProgram.class(), CapabilityClass::Bpf);
    /// ```
    #[must_use]
    pub const fn class(&self) -> CapabilityClass {
        match self {
            Self::ReadSysfsThermal
            | Self::ReadProcStat
            | Self::ReadProcMeminfo
            | Self::ReadSysfsBlock
            | Self::ReadProcNetDev
            | Self::ReadSysfsDrm
            | Self::ReadCgroupV2
            | Self::ReadProcNet
            | Self::ReadJournal
            | Self::ReadScanReports
            | Self::ReadWorkspaceGraph => CapabilityClass::Unprivileged,
            Self::ReadAuditNetlink => CapabilityClass::Netlink,
            Self::WatchFilesystem => CapabilityClass::FileWatch,
            Self::LoadBpfProgram => CapabilityClass::Bpf,
        }
    }

    /// Der Pfad oder die Ressource, die diese Fähigkeit erschließt.
    ///
    /// # Description
    /// Für dateisystembasierte Fähigkeiten ist dies der kanonische Wurzelpfad
    /// unter `/proc` oder `/sys`. Für Fähigkeiten ohne Dateisystempfad
    /// (Netlink-Sockets, interne Harness-Ressourcen) ist es ein stabiler,
    /// symbolischer Ressourcenname statt eines Pfads. Der Wert ist rein
    /// informativ (Diagnose, Registrierung, Anzeige) und wird von dieser
    /// Crate an keiner Stelle zur Zugriffsentscheidung herangezogen — das
    /// leistet ausschließlich [`crate::scope::ReadScope`].
    ///
    /// # Returns
    /// Einen statischen, nicht-leeren Bezeichner der Ressource.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::Capability;
    ///
    /// assert_eq!(Capability::ReadProcMeminfo.probe(), "/proc/meminfo");
    /// assert_eq!(Capability::ReadAuditNetlink.probe(), "audit-netlink");
    /// ```
    #[must_use]
    pub const fn probe(&self) -> &'static str {
        match self {
            Self::ReadSysfsThermal => "/sys/class/thermal",
            Self::ReadProcStat => "/proc/stat",
            Self::ReadProcMeminfo => "/proc/meminfo",
            Self::ReadSysfsBlock => "/sys/block",
            Self::ReadProcNetDev => "/proc/net/dev",
            Self::ReadSysfsDrm => "/sys/class/drm",
            Self::ReadCgroupV2 => "/sys/fs/cgroup",
            Self::ReadProcNet => "/proc/net",
            Self::ReadJournal => "/var/log/journal",
            Self::ReadAuditNetlink => "audit-netlink",
            Self::ReadScanReports => "workspace-scan-reports",
            Self::ReadWorkspaceGraph => "workspace-graph",
            Self::WatchFilesystem => "fanotify",
            Self::LoadBpfProgram => "/sys/fs/bpf",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Capability, CapabilityClass};
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_class_read_sysfs_thermal_is_unprivileged() {
        assert_eq!(
            Capability::ReadSysfsThermal.class(),
            CapabilityClass::Unprivileged
        );
    }

    #[test]
    fn test_class_read_proc_stat_is_unprivileged() {
        assert_eq!(
            Capability::ReadProcStat.class(),
            CapabilityClass::Unprivileged
        );
    }

    #[test]
    fn test_class_read_proc_meminfo_is_unprivileged() {
        assert_eq!(
            Capability::ReadProcMeminfo.class(),
            CapabilityClass::Unprivileged
        );
    }

    #[test]
    fn test_class_read_sysfs_block_is_unprivileged() {
        assert_eq!(
            Capability::ReadSysfsBlock.class(),
            CapabilityClass::Unprivileged
        );
    }

    #[test]
    fn test_class_read_proc_net_dev_is_unprivileged() {
        assert_eq!(
            Capability::ReadProcNetDev.class(),
            CapabilityClass::Unprivileged
        );
    }

    #[test]
    fn test_class_read_sysfs_drm_is_unprivileged() {
        assert_eq!(
            Capability::ReadSysfsDrm.class(),
            CapabilityClass::Unprivileged
        );
    }

    #[test]
    fn test_class_read_cgroup_v2_is_unprivileged() {
        assert_eq!(
            Capability::ReadCgroupV2.class(),
            CapabilityClass::Unprivileged
        );
    }

    #[test]
    fn test_class_read_proc_net_is_unprivileged() {
        assert_eq!(
            Capability::ReadProcNet.class(),
            CapabilityClass::Unprivileged
        );
    }

    #[test]
    fn test_class_read_journal_is_unprivileged() {
        assert_eq!(
            Capability::ReadJournal.class(),
            CapabilityClass::Unprivileged
        );
    }

    #[test]
    fn test_class_read_audit_netlink_is_netlink() {
        assert_eq!(
            Capability::ReadAuditNetlink.class(),
            CapabilityClass::Netlink
        );
    }

    #[test]
    fn test_class_read_scan_reports_is_unprivileged() {
        assert_eq!(
            Capability::ReadScanReports.class(),
            CapabilityClass::Unprivileged
        );
    }

    #[test]
    fn test_class_read_workspace_graph_is_unprivileged() {
        assert_eq!(
            Capability::ReadWorkspaceGraph.class(),
            CapabilityClass::Unprivileged
        );
    }

    #[test]
    fn test_class_watch_filesystem_is_file_watch() {
        assert_eq!(
            Capability::WatchFilesystem.class(),
            CapabilityClass::FileWatch
        );
    }

    #[test]
    fn test_class_load_bpf_program_is_bpf() {
        assert_eq!(Capability::LoadBpfProgram.class(), CapabilityClass::Bpf);
    }

    #[test]
    fn test_probe_returns_nonempty_static_str_for_every_variant() {
        let all = [
            Capability::ReadSysfsThermal,
            Capability::ReadProcStat,
            Capability::ReadProcMeminfo,
            Capability::ReadSysfsBlock,
            Capability::ReadProcNetDev,
            Capability::ReadSysfsDrm,
            Capability::ReadCgroupV2,
            Capability::ReadProcNet,
            Capability::ReadJournal,
            Capability::ReadAuditNetlink,
            Capability::ReadScanReports,
            Capability::ReadWorkspaceGraph,
            Capability::WatchFilesystem,
            Capability::LoadBpfProgram,
        ];
        for cap in all {
            assert!(
                !cap.probe().is_empty(),
                "probe() muss nicht-leer sein: {cap:?}"
            );
        }
    }

    #[test]
    fn test_probe_read_proc_stat_matches_procfs_path() {
        assert_eq!(Capability::ReadProcStat.probe(), "/proc/stat");
    }

    #[test]
    fn test_serde_roundtrip_uses_kebab_case() -> TestResult {
        let json =
            serde_json::to_string(&Capability::ReadAuditNetlink).map_err(ctx("serialize"))?;
        assert_eq!(json, "\"read-audit-netlink\"");
        let back: Capability = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, Capability::ReadAuditNetlink);
        Ok(())
    }
}
