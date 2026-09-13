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

pub mod error;
pub mod paths;
pub mod scaffold;
pub mod trust;

pub use error::{HomeError, HomeResult};
pub use paths::{
    LayerReport, active_profile_name, active_profile_path, auth_path, config_layers,
    config_layers_report, home_dir, profile_dir,
};
pub use scaffold::{Scaffolded, ensure_home};
pub use trust::{
    TrustRecord, TrustStatus, TrustStore, project_trust_status, trust_project, untrust_project,
};
