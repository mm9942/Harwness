//! Root-Space-Auflösung und Scaffolding für den `harw`-Harness.
//!
//! Dieses Crate besitzt die Antwort auf die Frage „wo lebt `harw`?" und legt
//! den Root-Space `~/.harw` bei Bedarf an. Es spiegelt bewusst das
//! codex/hermes/openclaw-Muster:
//!
//! - **Pfadauflösung** ([`paths`]): Env-Override `HARW_HOME` (Default
//!   `~/.harw`), aktives Profil über `HARW_PROFILE`/`active_profile`, **kein
//!   eager `mkdir`** bei reiner Auflösung.
//! - **Scaffolding** ([`scaffold`]): idempotentes Anlegen von Verzeichnissen
//!   und Default-Dateien; existierende Dateien werden nie überschrieben.
//!   Secrets ausschließlich in `auth.toml` (chmod 600).
//! - **Startausstattung** ([`bundle`]): die mit dem Binary ausgelieferte
//!   Agenten-Delegationshierarchie und ihre Skills, die das Scaffolding nach
//!   `~/.harw/agents` bzw. `~/.harw/skills` schreibt.
//! - **Layer-Zusammenstellung** ([`paths::config_layers`],
//!   [`paths::config_layers_report`]): baut die aufsteigende Präzedenzkette
//!   für `harw_config::discover_config`. Ein repo-lokales `./.harw` ist nur
//!   dann Layer, wenn das Projekt freigegeben ist.
//! - **Projekt-Trust** ([`trust`]): `trusted-projects.toml` mit kanonischem
//!   Root, Eigentümer-UID und BLAKE3-Digest der sicherheitsrelevanten
//!   `.harw`-Dateien.
//!
//! # Verantwortungsabgrenzung
//! Dieses Crate löst **keine** Secrets auf und lädt **keine** Config — es
//! liefert nur Pfade und schreibt Default-Templates. Das Laden/Validieren
//! bleibt bei `harw-config`, die Secret-Auflösung beim Aufrufer.
//!
//! # Concurrency
//! Alle Funktionen sind zustandslos und `Send + Sync`; sie synchronisieren
//! nicht gegen konkurrente Scaffolds. Ein `harw`-Prozess pro Root-Space ist
//! die erwartete Nutzung.
//!
//! # Errors
//! Alle fallierbaren Operationen liefern [`HomeError`].
//!
//! # Examples
//! ```rust,no_run
//! let home = harw_home::home_dir()?;
//! let report = harw_home::ensure_home(&home)?;
//! let layers = harw_home::config_layers(&home)?;
//! # Ok::<(), harw_home::HomeError>(())
//! ```

#![forbid(unsafe_code)]

pub mod bundle;
pub mod error;
pub mod paths;
pub mod project;
pub mod scaffold;
pub mod trust;

#[cfg(test)]
mod test_support;

pub use bundle::{BundledFile, bundled_files};
pub use error::{HomeError, HomeResult};
pub use paths::{
    LayerReport, active_profile_name, active_profile_path, auth_path, config_layers,
    config_layers_report, config_layers_report_at, home_dir, logs_dir, profile_dir,
};
pub use project::{
    ProjectHome, ProjectKind, ProjectRoot, discover_project, project_key, project_settings_dir,
};
pub use scaffold::{Scaffolded, ensure_home};
pub use trust::{
    TrustRecord, TrustStatus, TrustStore, project_trust_status, trust_project, untrust_project,
};

/// Immutable filesystem scope selected at the runtime boundary.
#[derive(Debug, Clone)]
pub struct ResolvedHomeContext {
    pub home: std::path::PathBuf,
    pub profile_name: String,
    pub profile_dir: std::path::PathBuf,
    pub project: ProjectRoot,
    pub project_home: ProjectHome,
    pub project_settings_path: std::path::PathBuf,
}

impl ResolvedHomeContext {
    /// Bind paths once; consumers must not resolve process environment again.
    pub fn new(
        home: &std::path::Path,
        profile_name: String,
        project: ProjectRoot,
    ) -> HomeResult<Self> {
        let home = std::path::absolute(home).map_err(|error| HomeError::io(home, error))?;
        let profile_dir = profile_dir(&home, &profile_name)?;
        let project_settings_path =
            project_settings_dir(&home, &profile_name, &project_key(&project.root))?
                .join("settings.toml");
        let project_home = ProjectHome::at(&project);
        Ok(Self {
            home,
            profile_name,
            profile_dir,
            project,
            project_home,
            project_settings_path,
        })
    }
}
