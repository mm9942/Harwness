//! Auflösung lokaler Cargo-Registry-Quellverzeichnisse unter
//! `$CARGO_HOME/registry/src` sowie Hilfs-URLs (docs.rs, crates.io) für
//! Recherche-Fragen. Es wird ausschließlich das Dateisystem gelesen, kein
//! Netzwerkzugriff und kein `cargo`-Subprozess. Siehe AP W1-14..17,
//! Abschnitt `registry_locator.rs`.

use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::{CodeGraphError, CodeGraphResult};

/// Findet lokale Registry-Quellverzeichnisse unterhalb eines `CARGO_HOME`.
#[derive(Debug, Clone)]
pub struct RegistrySourceLocator {
    cargo_home: PathBuf,
}

impl RegistrySourceLocator {
    /// Liest `$CARGO_HOME`; ist die Variable nicht gesetzt, wird
    /// `$HOME/.cargo` verwendet. Ist auch `$HOME` nicht auflösbar, wird
    /// `CargoHomeUnavailable` gemeldet.
    pub fn from_env() -> CodeGraphResult<Self> {
        if let Ok(cargo_home) = env::var("CARGO_HOME") {
            return Ok(Self::with_home(PathBuf::from(cargo_home)));
        }
        let home = env::var("HOME").map_err(|_| CodeGraphError::CargoHomeUnavailable)?;
        Ok(Self::with_home(PathBuf::from(home).join(".cargo")))
    }

    /// Baut den Locator mit einem explizit angegebenen `CARGO_HOME`-Pfad
    /// (z. B. für Tests mit einem Fixture-Verzeichnis).
    #[must_use]
    pub fn with_home(cargo_home: PathBuf) -> Self {
        Self { cargo_home }
    }

    /// `<cargo_home>/registry/src`.
    fn registry_src_root(&self) -> PathBuf {
        self.cargo_home.join("registry").join("src")
    }

    /// Alle Index-Verzeichnisse unter `registry/src` (z. B.
    /// `index.crates.io-<hash>`). Fehlt `registry/src` ganz, wird eine leere
    /// Liste geliefert statt eines Fehlers.
    fn index_dirs(&self) -> CodeGraphResult<Vec<PathBuf>> {
        let src_root = self.registry_src_root();
        let entries = match fs::read_dir(&src_root) {
            Ok(entries) => entries,
            Err(_) => return Ok(Vec::new()),
        };
        let mut dirs = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            }
        }
        dirs.sort();
        Ok(dirs)
    }

    /// Sucht `<cargo_home>/registry/src/<index-dir>/<name>-<version>` über
    /// alle Index-Verzeichnisse hinweg. Meldet `RegistrySourceNotFound`,
    /// falls kein Index-Verzeichnis einen passenden Ordner enthält.
    pub fn resolve(&self, crate_name: &str, version: &str) -> CodeGraphResult<PathBuf> {
        let expected = format!("{crate_name}-{version}");
        for index_dir in self.index_dirs()? {
            let candidate = index_dir.join(&expected);
            if candidate.is_dir() {
                return Ok(candidate);
            }
        }
        Err(CodeGraphError::RegistrySourceNotFound {
            crate_name: crate_name.to_owned(),
            version: version.to_owned(),
        })
    }

    /// Alle im lokalen Cache verfügbaren Versionen eines Crates, ermittelt
    /// durch Parsen der Verzeichnisnamen `<name>-<version>` in jedem
    /// Index-Verzeichnis.
    pub fn available_versions(&self, crate_name: &str) -> CodeGraphResult<Vec<String>> {
        let prefix = format!("{crate_name}-");
        let mut versions = Vec::new();
        for index_dir in self.index_dirs()? {
            let entries = match fs::read_dir(&index_dir) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries {
                let entry = entry?;
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                if let Some(version) = name.strip_prefix(prefix.as_str()) {
                    versions.push(version.to_owned());
                }
            }
        }
        versions.sort();
        versions.dedup();
        Ok(versions)
    }

    /// Wie [`Self::resolve`], zusätzlich mit Pfad-Containment-Prüfung: der um
    /// `relative` erweiterte Ergebnispfad muss (rein lexikalisch, ohne dass
    /// er existieren muss) unterhalb von `<cargo_home>/registry/src` liegen.
    /// Traversal via `..` wird als `InvalidPath` abgelehnt.
    pub fn resolve_contained(
        &self,
        crate_name: &str,
        version: &str,
        relative: &Path,
    ) -> CodeGraphResult<PathBuf> {
        let base = self.resolve(crate_name, version)?;
        let joined = base.join(relative);
        let normalized = normalize_lexically(&joined);
        let src_root = normalize_lexically(&self.registry_src_root());
        if normalized.starts_with(&src_root) {
            Ok(normalized)
        } else {
            Err(CodeGraphError::InvalidPath {
                path: joined.display().to_string(),
            })
        }
    }

    /// Baut die docs.rs-URL für ein Crate/Item. Bindestriche im Crate-Namen
    /// werden im Modulpfad-Segment zu Unterstrichen; ohne Version wird
    /// `latest` verwendet.
    #[must_use]
    pub fn docs_rs_url(crate_name: &str, version: Option<&str>, item_path: Option<&str>) -> String {
        let version_segment = version.unwrap_or("latest");
        let module_path = crate_name.replace('-', "_");
        match item_path {
            Some(item) => format!("https://docs.rs/{crate_name}/{version_segment}/{module_path}/{item}"),
            None => format!("https://docs.rs/{crate_name}/{version_segment}/{module_path}/"),
        }
    }

    /// Baut die crates.io-API-URL für ein Crate.
    #[must_use]
    pub fn crates_io_api_url(crate_name: &str) -> String {
        format!("https://crates.io/api/v1/crates/{crate_name}")
    }
}

/// Normalisiert einen Pfad rein lexikalisch (kein Dateisystemzugriff, im
/// Gegensatz zu `Path::canonicalize`): `.`-Komponenten werden entfernt,
/// `..` hebt die vorige Komponente auf. Grundlage für die
/// Traversal-Schutzprüfung in [`RegistrySourceLocator::resolve_contained`],
/// da der Zielpfad nicht existieren muss.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("harw-code-graph-{}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("Scratch-Verzeichnis anlegen");
        dir
    }

    #[test]
    fn docs_rs_url_builds_expected_link() {
        let url = RegistrySourceLocator::docs_rs_url("harw-plan", None, Some("struct.Plan.html"));
        assert_eq!(
            url,
            "https://docs.rs/harw-plan/latest/harw_plan/struct.Plan.html"
        );
    }

    #[test]
    fn docs_rs_url_with_explicit_version_and_no_item() {
        let url = RegistrySourceLocator::docs_rs_url("toml", Some("1.1.3"), None);
        assert_eq!(url, "https://docs.rs/toml/1.1.3/toml/");
    }

    #[test]
    fn crates_io_api_url_builds_expected_link() {
        assert_eq!(
            RegistrySourceLocator::crates_io_api_url("serde"),
            "https://crates.io/api/v1/crates/serde"
        );
    }

    #[test]
    fn resolve_contained_rejects_parent_traversal() {
        let cargo_home = scratch_dir("registry-home");
        let index_dir = cargo_home
            .join("registry")
            .join("src")
            .join("index.crates.io-testhash");
        let crate_dir = index_dir.join("serde-1.0.228");
        fs::create_dir_all(&crate_dir).expect("Fixture-Crate-Verzeichnis anlegen");

        let locator = RegistrySourceLocator::with_home(cargo_home.clone());

        let resolved = locator.resolve("serde", "1.0.228").expect("Crate-Quelle gefunden");
        assert_eq!(resolved, crate_dir);

        let escape = locator.resolve_contained(
            "serde",
            "1.0.228",
            Path::new("../../../../../../etc/passwd"),
        );
        assert!(matches!(escape, Err(CodeGraphError::InvalidPath { .. })));

        let contained = locator
            .resolve_contained("serde", "1.0.228", Path::new("src/lib.rs"))
            .expect("enthaltener Pfad akzeptiert");
        assert_eq!(contained, crate_dir.join("src/lib.rs"));

        fs::remove_dir_all(&cargo_home).ok();
    }

    #[test]
    fn available_versions_lists_matching_directories() {
        let cargo_home = scratch_dir("registry-versions");
        let index_dir = cargo_home
            .join("registry")
            .join("src")
            .join("index.crates.io-testhash");
        fs::create_dir_all(index_dir.join("serde-1.0.228")).expect("Fixture 1 anlegen");
        fs::create_dir_all(index_dir.join("serde-1.0.219")).expect("Fixture 2 anlegen");
        fs::create_dir_all(index_dir.join("toml-1.1.3+spec-1.1.0")).expect("Fixture 3 anlegen");

        let locator = RegistrySourceLocator::with_home(cargo_home.clone());
        let mut versions = locator
            .available_versions("serde")
            .expect("Versionen auflisten");
        versions.sort();
        assert_eq!(versions, vec!["1.0.219".to_owned(), "1.0.228".to_owned()]);

        fs::remove_dir_all(&cargo_home).ok();
    }

    #[test]
    fn resolve_reports_missing_source() {
        let cargo_home = scratch_dir("registry-missing");
        fs::create_dir_all(&cargo_home).expect("cargo_home anlegen");
        let locator = RegistrySourceLocator::with_home(cargo_home.clone());

        let result = locator.resolve("does-not-exist", "9.9.9");
        assert!(matches!(
            result,
            Err(CodeGraphError::RegistrySourceNotFound { .. })
        ));

        fs::remove_dir_all(&cargo_home).ok();
    }
}
