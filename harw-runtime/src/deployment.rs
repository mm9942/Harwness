//! Deployment-/Kompositionsprofile (Cloud-Home-Plan, Schritt H1/W1).
//!
//! # Verantwortung
//! Ein [`DeploymentProfile`] wählt, **welche Dienste ein Prozess montiert**
//! (Coding, Cloud Hub, Worker Hub, Container Hub, Compile Hub). Es ist kein
//! Agenten-Rechteprofil und ersetzt weder `EntryProfile` noch `RegistryProfile`
//! noch die Agenten-IR: es legt nur fest, welche Backends ein Dienstprozess
//! überhaupt besitzen darf (Obergrenze). Rechte entstehen weiter aus dem
//! Schnitt von Aufrufer, Manifest, Knoten-Policy, Job und Sandbox.
//!
//! Zwei feste Tabellen: [`DeploymentProfile::mounts`] (montierte Dienste) und
//! [`DeploymentProfile::denied`] (ausdrücklich verbotene Dienste). Die Tests
//! erzwingen, dass sich beide nie überschneiden und dass der Cloud Hub keinen
//! Ausführungs-Backend-Dienst besitzt.

use std::fmt;

/// Ein montierbarer Dienst, grob nach Autoritätsklasse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Service {
    /// Terminal-/Attach-Oberfläche (reiner Client).
    Tui,
    /// Modell-Provider-Clients.
    ProviderClients,
    /// Client für `SessionPort` (Attach/Reconnect).
    SessionPortClient,
    /// Lokaler Job-Client/-Manager.
    JobClient,
    /// Entwicklungs-Werkzeugregister (fs, Suche, Shell im Sandbox-Rahmen).
    DevToolRegistry,
    /// Identität und Policy-Verwaltung des Hubs.
    IdentityPolicy,
    /// Sitzungsverzeichnis und Routing.
    SessionDirectory,
    /// Knotenregister.
    NodeRegistry,
    /// Hostet Sitzungen (Single Writer) über `SessionPort`.
    SessionHost,
    /// Worker-Boot und Agenten-/Prozess-Worker.
    WorkerBoot,
    /// Rootless-Container-/Zell-Lebenszyklus.
    ContainerRuntime,
    /// Build-/Test-/Lint-Lauf mit eigener Lane.
    CompileLane,
    /// Schreibzugriff auf einen Workspace.
    WorkspaceWrite,
    /// Generische Host-Shell außerhalb einer Sandbox.
    HostShell,
    /// Container-Engine-Socket (Zugriff auf die Engine selbst).
    ContainerEngineSocket,
}

impl Service {
    /// `true` für Dienste, die Code ausführen oder Host-Zustand verändern können.
    #[must_use]
    pub const fn is_execution_backend(self) -> bool {
        matches!(
            self,
            Self::WorkerBoot
                | Self::ContainerRuntime
                | Self::CompileLane
                | Self::WorkspaceWrite
                | Self::HostShell
                | Self::ContainerEngineSocket
        )
    }
}

/// Kompositionsprofil eines Prozesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeploymentProfile {
    /// Interaktive Entwicklung auf dem eigenen Gerät.
    Coding,
    /// Identität, Verzeichnis, Policy, Routing; keine Ausführung.
    CloudHub,
    /// Beschränkte Agenten-/Prozess-Jobs.
    WorkerHub,
    /// Rootless-Container-Zellen.
    ContainerHub,
    /// Reproduzierbare Builds, Tests, Lint.
    CompileHub,
}

impl DeploymentProfile {
    /// Alle Profile; ein Test hält die Liste vollständig.
    pub const ALL: [Self; 5] = [
        Self::Coding,
        Self::CloudHub,
        Self::WorkerHub,
        Self::ContainerHub,
        Self::CompileHub,
    ];

    /// Stabiler Kurzname (Konfiguration, Logs).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Coding => "coding",
            Self::CloudHub => "cloud-hub",
            Self::WorkerHub => "worker-hub",
            Self::ContainerHub => "container-hub",
            Self::CompileHub => "compile-hub",
        }
    }

    /// Parst den Kurznamen exakt (keine Aliase, keine Groß-/Kleinschreibung).
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == raw)
    }

    /// Dienste, die dieses Profil montiert.
    #[must_use]
    pub const fn mounts(self) -> &'static [Service] {
        match self {
            Self::Coding => &[
                Service::Tui,
                Service::ProviderClients,
                Service::SessionPortClient,
                Service::JobClient,
                Service::DevToolRegistry,
                Service::WorkspaceWrite,
            ],
            Self::CloudHub => &[
                Service::IdentityPolicy,
                Service::SessionDirectory,
                Service::NodeRegistry,
                Service::SessionHost,
            ],
            Self::WorkerHub => &[Service::JobClient, Service::WorkerBoot],
            Self::ContainerHub => &[
                Service::JobClient,
                Service::WorkerBoot,
                Service::ContainerRuntime,
            ],
            Self::CompileHub => &[Service::JobClient, Service::CompileLane],
        }
    }

    /// Dienste, die dieses Profil nie besitzen darf, auch nicht über Konfiguration.
    #[must_use]
    pub const fn denied(self) -> &'static [Service] {
        match self {
            Self::Coding => &[Service::ContainerEngineSocket],
            Self::CloudHub => &[
                Service::WorkerBoot,
                Service::ContainerRuntime,
                Service::CompileLane,
                Service::WorkspaceWrite,
                Service::HostShell,
                Service::ContainerEngineSocket,
            ],
            Self::WorkerHub | Self::CompileHub => &[
                Service::HostShell,
                Service::ContainerEngineSocket,
                Service::IdentityPolicy,
            ],
            Self::ContainerHub => &[Service::HostShell, Service::ContainerEngineSocket],
        }
    }

    /// `true`, wenn dieses Profil `service` montiert.
    #[must_use]
    pub fn mounts_service(self, service: Service) -> bool {
        self.mounts().contains(&service)
    }

    /// Ob zwei Profile im selben Prozess kombiniert werden dürfen. Der Cloud Hub
    /// wird nie mit einem Ausführungsprofil kombiniert; ein Profil ist stets mit
    /// sich selbst verträglich. Jede Kombination wird zusätzlich gegen die
    /// Verbotslisten beider Seiten geprüft.
    #[must_use]
    pub fn compatible_with(self, other: Self) -> bool {
        if self == other {
            return true;
        }
        if self == Self::CloudHub || other == Self::CloudHub {
            return false;
        }
        let clash = |a: Self, b: Self| a.mounts().iter().any(|s| b.denied().contains(s));
        !clash(self, other) && !clash(other, self)
    }
}

impl fmt::Display for DeploymentProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn ensure(cond: bool, msg: &str) -> TestResult {
        if cond {
            Ok(())
        } else {
            Err(TestError::Unexpected(msg.to_owned()))
        }
    }

    #[test]
    fn all_lists_every_variant_and_names_round_trip() -> TestResult {
        // Exhaustiv: ein neues Profil muss hier und in `ALL` auftauchen.
        for p in DeploymentProfile::ALL {
            match p {
                DeploymentProfile::Coding
                | DeploymentProfile::CloudHub
                | DeploymentProfile::WorkerHub
                | DeploymentProfile::ContainerHub
                | DeploymentProfile::CompileHub => {}
            }
            ensure(
                DeploymentProfile::parse(p.as_str()) == Some(p),
                "round trip",
            )?;
        }
        ensure(
            DeploymentProfile::parse("Coding").is_none(),
            "case sensitive",
        )?;
        ensure(DeploymentProfile::parse("hub").is_none(), "no aliases")
    }

    #[test]
    fn mounts_and_denied_never_overlap() -> TestResult {
        for p in DeploymentProfile::ALL {
            for s in p.mounts() {
                ensure(!p.denied().contains(s), "a mounted service is also denied")?;
            }
        }
        Ok(())
    }

    #[test]
    fn cloud_hub_has_no_execution_backend() -> TestResult {
        let hub = DeploymentProfile::CloudHub;
        ensure(
            hub.mounts().iter().all(|s| !s.is_execution_backend()),
            "cloud hub mounts an execution backend",
        )?;
        for s in [
            Service::WorkerBoot,
            Service::ContainerRuntime,
            Service::CompileLane,
            Service::WorkspaceWrite,
            Service::HostShell,
            Service::ContainerEngineSocket,
        ] {
            ensure(
                hub.denied().contains(&s),
                "cloud hub must deny every execution backend",
            )?;
        }
        Ok(())
    }

    #[test]
    fn no_profile_mounts_host_shell_or_engine_socket() -> TestResult {
        for p in DeploymentProfile::ALL {
            ensure(!p.mounts_service(Service::HostShell), "host shell mounted")?;
            ensure(
                !p.mounts_service(Service::ContainerEngineSocket),
                "engine socket mounted",
            )?;
            ensure(
                p.denied().contains(&Service::ContainerEngineSocket),
                "engine socket not denied",
            )?;
        }
        Ok(())
    }

    #[test]
    fn only_the_intended_profile_mounts_its_privileged_service() -> TestResult {
        for p in DeploymentProfile::ALL {
            let compile = p.mounts_service(Service::CompileLane);
            ensure(
                compile == (p == DeploymentProfile::CompileHub),
                "compile lane",
            )?;
            let container = p.mounts_service(Service::ContainerRuntime);
            ensure(
                container == (p == DeploymentProfile::ContainerHub),
                "container runtime",
            )?;
            let directory = p.mounts_service(Service::SessionDirectory);
            ensure(
                directory == (p == DeploymentProfile::CloudHub),
                "session directory",
            )?;
        }
        Ok(())
    }

    #[test]
    fn combinations_follow_the_rules() -> TestResult {
        use DeploymentProfile::{CloudHub, Coding, CompileHub, ContainerHub, WorkerHub};
        for p in DeploymentProfile::ALL {
            ensure(p.compatible_with(p), "self compatible")?;
        }
        for p in [Coding, WorkerHub, ContainerHub, CompileHub] {
            ensure(
                !CloudHub.compatible_with(p) && !p.compatible_with(CloudHub),
                "cloud hub is never combined",
            )?;
        }
        ensure(
            WorkerHub.compatible_with(CompileHub),
            "worker + compile (e.g. the Pi)",
        )?;
        ensure(
            ContainerHub.compatible_with(WorkerHub),
            "container + worker",
        )?;
        for a in DeploymentProfile::ALL {
            for b in DeploymentProfile::ALL {
                ensure(a.compatible_with(b) == b.compatible_with(a), "symmetric")?;
            }
        }
        Ok(())
    }
}
