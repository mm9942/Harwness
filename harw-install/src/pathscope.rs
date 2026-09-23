//! Lexikalisch abgesicherter Pfad-Auflöser (Spec: CONTRACT-setup-install.md,
//! Crate `harw-install` → `src/pathscope.rs`).
//!
//! ## Zweck
//! Löst relative Pfade **innerhalb** eines festgelegten `root`-Verzeichnisses
//! auf und verhindert Directory-Traversal. Die Prüfung erfolgt **rein
//! lexikalisch** über [`std::path::Path::components`] — es wird bewusst **kein**
//! [`std::fs::canonicalize`] aufgerufen, um Symlinks nicht zu folgen und keinen
//! Dateisystem-Zugriff zu erfordern.
//!
//! ## Verantwortung
//! Dieses Modul besitzt ausschließlich die Traversal-Validierung und den
//! Root-relativen Join. Die Fehlerdefinition liegt bei [`crate::error`].
//!
//! ## Exportierte Typen
//! - [`PathScope`]: an einen `root` gebundener Auflöser.
//!
//! ## Fehler
//! - [`crate::error::PathError`] (Variante `Traversal`), wenn ein Eingabepfad
//!   den `root` verlassen würde.
//!
//! ## Nebenläufigkeit
//! [`PathScope`] hält nur einen [`std::path::PathBuf`] und ist damit `Send +
//! Sync`. Alle Methoden sind seiteneffektfrei und thread-sicher (kein I/O,
//! keine Locks).
//!
//! # Examples
//! ```rust,no_run
//! use harw_install::pathscope::PathScope;
//!
//! let scope = PathScope::new("/home/user/.harw");
//! let ok = scope.resolve("providers/openai.toml").expect("innerhalb root");
//! assert!(ok.starts_with("/home/user/.harw"));
//! assert!(scope.resolve("../etc/passwd").is_err());
//! ```

use std::path::{Component, Path, PathBuf};

use crate::error::PathError;

/// An ein `root`-Verzeichnis gebundener, traversal-sicherer Pfad-Auflöser.
///
/// # Description
/// Kapselt ein Wurzelverzeichnis und löst relative Eingaben zu absoluten (bzw.
/// root-präfixierten) Pfaden auf. Eingaben, die den `root` lexikalisch
/// verlassen, absolut sind oder eine `..`-Komponente enthalten, werden mit
/// [`PathError::Traversal`] abgelehnt.
///
/// # Concurrency
/// `Send + Sync`; enthält nur einen [`PathBuf`]. Keine interne Mutabilität.
#[derive(Debug, Clone)]
pub struct PathScope {
    /// Wurzelverzeichnis, das keine Auflösung verlassen darf.
    root: PathBuf,
}

impl PathScope {
    /// Erstellt einen `PathScope` mit dem gegebenen Wurzelverzeichnis.
    ///
    /// # Arguments
    /// - `root` (`impl Into<PathBuf>`): das Wurzelverzeichnis; die Eigentümerschaft
    ///   wird übernommen. Wird nicht normalisiert oder kanonisiert.
    ///
    /// # Returns
    /// Ein neuer [`PathScope`].
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_install::pathscope::PathScope;
    /// let scope = PathScope::new("/srv/harw");
    /// ```
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Gibt das Wurzelverzeichnis zurück.
    ///
    /// # Returns
    /// Eine geliehene [`Path`]-Referenz auf den `root`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Löst einen relativen Pfad root-sicher und rein lexikalisch auf.
    ///
    /// # Description
    /// Der Eingabepfad wird an `root` angehängt und über
    /// [`Path::components`] normalisiert. Dabei werden `.`-Komponenten
    /// verworfen und `..`-Komponenten so weit möglich gegen zuvor akkumulierte
    /// Namenskomponenten aufgerechnet. Verlässt die Normalisierung den `root`
    /// (oder ist `rel` absolut / enthält eine Wurzel-/Präfix-Komponente), wird
    /// abgelehnt. Es findet **kein** Dateisystem-Zugriff statt; Symlinks werden
    /// nicht aufgelöst.
    ///
    /// # Arguments
    /// - `rel` (`&str`): der aufzulösende relative Pfad, geliehen.
    ///
    /// # Returns
    /// Den vollständigen, root-präfixierten [`PathBuf`] bei Erfolg.
    ///
    /// # Errors
    /// - [`PathError::Traversal`]: wenn `rel` absolut ist, eine `..`-Komponente
    ///   enthält, oder die lexikalische Normalisierung den `root` verlassen
    ///   würde.
    ///
    /// # Concurrency
    /// Seiteneffektfrei und thread-sicher (kein I/O, keine Locks).
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_install::pathscope::PathScope;
    /// let scope = PathScope::new("/srv/harw");
    /// assert!(scope.resolve("a/b.toml").is_ok());
    /// assert!(scope.resolve("../x").is_err());
    /// ```
    pub fn resolve(&self, rel: &str) -> Result<PathBuf, PathError> {
        let candidate = Path::new(rel);

        // Absolute Eingaben (mit Root-Dir oder Präfix, z. B. "/etc" oder "C:\")
        // verlassen den root sofort und sind unzulässig.
        if candidate.is_absolute() {
            return Err(PathError::Traversal {
                input: rel.to_owned(),
            });
        }

        // Lexikalische Normalisierung relativ zum root. `depth` zählt die
        // Namenskomponenten OBERHALB des root; ein negativer Stand bedeutet
        // einen Ausbruch aus dem root.
        let mut normalized = self.root.clone();
        let mut depth: usize = 0;

        for component in candidate.components() {
            match component {
                // Root/Präfix innerhalb einer relativen Eingabe (etwa nach
                // eingebettetem "/") ist unzulässig.
                Component::RootDir | Component::Prefix(_) => {
                    return Err(PathError::Traversal {
                        input: rel.to_owned(),
                    });
                }
                // "." ändert die Position nicht.
                Component::CurDir => {}
                // ".." darf niemals über den root hinaus aufsteigen.
                Component::ParentDir => {
                    if depth == 0 {
                        return Err(PathError::Traversal {
                            input: rel.to_owned(),
                        });
                    }
                    depth -= 1;
                    normalized.pop();
                }
                Component::Normal(part) => {
                    depth += 1;
                    normalized.push(part);
                }
            }
        }

        Ok(normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn scope() -> PathScope {
        PathScope::new("/srv/harw")
    }

    #[test]
    fn test_resolve_nested_relative_ok() -> TestResult {
        let got = scope()
            .resolve("a/b.toml")
            .map_err(ctx("innerhalb root erlaubt"))?;
        assert_eq!(got, PathBuf::from("/srv/harw/a/b.toml"));
        Ok(())
    }

    #[test]
    fn test_resolve_curdir_is_normalized() -> TestResult {
        let got = scope()
            .resolve("./a/./b.toml")
            .map_err(ctx("cur-dir erlaubt"))?;
        assert_eq!(got, PathBuf::from("/srv/harw/a/b.toml"));
        Ok(())
    }

    #[test]
    fn test_resolve_parent_dir_within_root_ok() -> TestResult {
        // Steigt einmal auf, bleibt aber im root.
        let got = scope()
            .resolve("a/../b.toml")
            .map_err(ctx("bleibt in root"))?;
        assert_eq!(got, PathBuf::from("/srv/harw/b.toml"));
        Ok(())
    }

    #[test]
    fn test_resolve_leading_parent_rejected() {
        let err = scope().resolve("../x");
        assert!(matches!(err, Err(PathError::Traversal { .. })));
    }

    #[test]
    fn test_resolve_absolute_rejected() {
        let err = scope().resolve("/etc/passwd");
        assert!(matches!(err, Err(PathError::Traversal { .. })));
    }

    #[test]
    fn test_resolve_escape_after_join_rejected() {
        let err = scope().resolve("a/../../x");
        assert!(matches!(err, Err(PathError::Traversal { .. })));
    }

    #[test]
    fn test_resolve_traversal_input_is_preserved() -> TestResult {
        match scope().resolve("a/../../x") {
            Err(PathError::Traversal { input }) => assert_eq!(input, "a/../../x"),
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartete Traversal, bekam {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_root_returns_configured_root() {
        assert_eq!(scope().root(), Path::new("/srv/harw"));
    }
}
