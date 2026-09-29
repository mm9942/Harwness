//! `[builder]` — Container-Worker-Bau (P2 Builder-API-Contract).
//!
//! # Beschreibung
//! | Schlüssel | Vorgabe | erlaubt | Bedeutung |
//! |---|---|---|---|
//! | `template` | `hermetic` | `hermetic`/`rootless-podman` | Worker-Vorlage: hermetisch (Bwrap, kein Netz, kein Root-Cargo) oder Rootless-Podman-Container |
//! | `build.jobs` | 1 | 1–64 | parallele Cargo-Jobs (RAM-Schutz) |
//! | `build.ram_mib` | 4096 | 512–65536 | RAM-Obergrenze des Worker-Prozesses |
//! | `mounts` | — | 0–8 Einträge | Ziel-/Toolchain-Bind-Mounts (MountSpec) |
//! | `remote.endpoint` | — | — | SSH-Endpunkt eines Pi-Workers (`mm29942-raspi`: 100.123.51.33, mia@) |
//!
//! Hermetische Defaults: kein Netz-Zugriff fürs Template `hermetic`, keine
//! Root-Cargo-Aufrufe (`cargo`-Aufrufe laufen als Workspace-Nutzer), kein
//! Schreibzugriff außerhalb der Workspace-Binds. Remote-Worker (Pi) erben
//! dieselben Grenzen und werden über SSH/Podman auf dem Ziel gefahren.
//!
//! # Merge-Regel
//! Nur vertraute Layer (Home und Profil) setzen frei; ungültige Werte werden
//! in den erlaubten Bereich geklemmt, nie abgelehnt (Konsistenz mit
//! `shell_limits`).
//!
//! # Nebenläufigkeit
//! Reine Datentypen und Funktionen.

use serde::{Deserialize, Serialize};

/// Vorgabe für `[builder] build.jobs` (RAM-Schutz, eine Vervollständigungs­pipeline).
pub const DEFAULT_BUILDER_JOBS: u32 = 1;
/// Erlaubter Bereich für `[builder] build.jobs`.
pub const BUILDER_JOBS_RANGE: (u32, u32) = (1, 64);
/// Vorgabe für `[builder] build.ram_mib` (4 GiB).
pub const DEFAULT_BUILDER_RAM_MIB: u32 = 4096;
/// Erlaubter Bereich für `[builder] build.ram_mib`.
pub const BUILDER_RAM_MIB_RANGE: (u32, u32) = (512, 65_536);

/// Worker-Vorlage eines Builder-Workers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerTemplate {
    /// Hermetisch: Bwrap-Sandbox, kein Netz, kein Root-Cargo.
    #[default]
    Hermetic,
    /// Rootless-Podman-Container (lokal oder auf dem Pi-Worker).
    RootlessPodman,
}

/// Ein Bind-Mount eines Builder-Workers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MountSpec {
    /// Quelle auf dem Host (oder dem Pi, bei Remote-Workern).
    pub source: String,
    /// Ziel im Worker.
    pub target: String,
    /// Nur lesend? Vorgabe: nein.
    #[serde(default)]
    pub read_only: bool,
}

/// Konfig-Sektion `[builder]` für Container-Worker.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuilderToml {
    /// Worker-Vorlage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<WorkerTemplate>,
    /// Builds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<BuilderBuildToml>,
    /// Mounts
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<MountSpec>,
    /// Remote-Worker (Pi).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<BuilderRemoteToml>,
}

/// Build-Grenzen eines Builder-Workers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuilderBuildToml {
    /// Parallele Cargo-Jobs (RAM-Schutz).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jobs: Option<u32>,
    /// RAM-Obergrenze in MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_mib: Option<u32>,
}

/// Remote-Worker-Ziel (Pi-Worker über SSH).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuilderRemoteToml {
    /// SSH-Endpunkt, z. B. `mia@100.123.51.33`.
    pub endpoint: String,
    /// Container-Backend auf dem Ziel; Vorgabe: `rootless-podman`.
    #[serde(default)]
    pub backend: WorkerTemplate,
    /// Pfad zum SSH-Identität-Key (z. B. `~/.ssh/id_ed25519_…`); ohne Angabe
    /// nutzt Podman den Default-Key des Nutzers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// Optionale mTLS-Sektion (Crypt Guard Erweiterung): gegenseitige
    /// Authentifizierung zwischen Host und Pi; SSH bleibt Vorgabe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<BuilderRemoteTlsToml>,
}

/// Optionale mTLS-Konfiguration für den Remote-Worker (Crypt Guard).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuilderRemoteTlsToml {
    /// Client-Zertifikat (PEM).
    pub cert: String,
    /// Client-Key (PEM).
    pub key: String,
    /// CA-Zertifikat (PEM), gegen das der Pi prüft.
    pub ca: String,
    /// Server-URL (z. B. `https://100.123.51.33:8080`); über Tailscale.
    pub url: String,
}

impl BuilderRemoteToml {
    /// Die wirksame Podman-Remote-URL: SSH-Vorgabe (mit Socket-Pfad) oder
    /// mTLS-URL, wenn `tls` konfiguriert ist.
    #[must_use]
    pub fn effective_podman_url(&self) -> String {
        match &self.tls {
            Some(tls) => tls.url.clone(),
            None => format!("ssh://{}/run/user/1000/podman/podman.sock", self.endpoint),
        }
    }
}

impl BuilderToml {
    /// Die wirksame Worker-Vorlage (Vorgabe: hermetisch).
    #[must_use]
    pub fn effective_template(&self) -> WorkerTemplate {
        self.template.unwrap_or_default()
    }

    /// Die wirksame Job-Zahl: Vorgabe 1, geklemmt auf 1–64.
    #[must_use]
    pub fn effective_jobs(&self) -> u32 {
        self.build
            .as_ref()
            .and_then(|b| b.jobs)
            .unwrap_or(DEFAULT_BUILDER_JOBS)
            .clamp(BUILDER_JOBS_RANGE.0, BUILDER_JOBS_RANGE.1)
    }

    /// Die wirksame RAM-Obergrenze in MiB: Vorgabe 4096, geklemmt auf
    /// 512–65536.
    #[must_use]
    pub fn effective_ram_mib(&self) -> u32 {
        self.build
            .as_ref()
            .and_then(|b| b.ram_mib)
            .unwrap_or(DEFAULT_BUILDER_RAM_MIB)
            .clamp(BUILDER_RAM_MIB_RANGE.0, BUILDER_RAM_MIB_RANGE.1)
    }

    /// Die wirksamen Mounts (höchstens 8; weitere werden verworfen).
    #[must_use]
    pub fn effective_mounts(&self) -> &[MountSpec] {
        let cut = self.mounts.len().min(8);
        &self.mounts[..cut]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults_are_hermetic() {
        let builder = BuilderToml::default();
        assert_eq!(builder.effective_template(), WorkerTemplate::Hermetic);
        assert_eq!(builder.effective_jobs(), 1);
        assert_eq!(builder.effective_ram_mib(), 4096);
        assert!(builder.effective_mounts().is_empty());
    }

    #[test]
    fn test_out_of_range_values_are_clamped() {
        let low = BuilderToml {
            build: Some(BuilderBuildToml {
                jobs: Some(0),
                ram_mib: Some(64),
            }),
            ..BuilderToml::default()
        };
        assert_eq!(low.effective_jobs(), 1);
        assert_eq!(low.effective_ram_mib(), 512);
        let high = BuilderToml {
            build: Some(BuilderBuildToml {
                jobs: Some(999),
                ram_mib: Some(u32::MAX),
            }),
            ..BuilderToml::default()
        };
        assert_eq!(high.effective_jobs(), 64);
        assert_eq!(high.effective_ram_mib(), 65_536);
    }

    #[test]
    fn test_mounts_are_capped_at_eight() {
        let builder = BuilderToml {
            mounts: (0..10)
                .map(|i| MountSpec {
                    source: format!("/src/{i}"),
                    target: format!("/dst/{i}"),
                    read_only: false,
                })
                .collect(),
            ..BuilderToml::default()
        };
        assert_eq!(builder.effective_mounts().len(), 8);
    }

    #[test]
    fn test_toml_roundtrip_kebab_case() {
        let toml_text = r#"
            template = "rootless-podman"
            [build]
            jobs = 2
            ram_mib = 8192
        "#;
        let builder: BuilderToml = toml::from_str(toml_text).expect("parse");
        assert_eq!(builder.effective_template(), WorkerTemplate::RootlessPodman);
        assert_eq!(builder.effective_jobs(), 2);
        assert_eq!(builder.effective_ram_mib(), 8192);
    }
}
