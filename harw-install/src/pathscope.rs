//! Lexikalisch abgesicherter Pfad-Auflöser (Spec: CONTRACT-setup-install.md,
//! Crate `harw-install` → `src/pathscope.rs`).
//!
//! ## Zweck
//! Löst relative Pfade **innerhalb** eines festgelegten `root`-Verzeichnisses
//! auf und verhindert Directory-Traversal. Die Prüfung erfolgt **rein
//! lexikalisch** über [`std::path::Path::components`] — es wird bewusst **kein**
//! [`std::fs::canonicalize`] aufgerufen, um Symlinks nicht zu folgen und keinen
//! Dateisystem-Zugriff zu erfordern. Dadurch bleibt aber auch ungeprüft, ob
//! eine `Normal`-Komponente innerhalb von `root` selbst ein Symlink ist, der
//! auf dem Dateisystem aus `root` hinausführt — die lexikalische Ablehnung
//! jeder `..`-Komponente schützt nur gegen Traversal im Eingabetext, nicht
//! gegen Symlinks, die bereits unterhalb `root` liegen.
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
//!   absolut ist oder eine `..`-Komponente enthält — auch dann, wenn diese
//!   lexikalisch innerhalb von `root` bliebe.
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
/// root-präfixierten) Pfaden auf. Eingaben, die absolut sind oder auch nur
/// eine einzige `..`-Komponente enthalten, werden mit
/// [`PathError::Traversal`] abgelehnt — unabhängig davon, ob diese
/// `..`-Komponente lexikalisch innerhalb von `root` bliebe. Symlinks
/// unterhalb von `root` werden nicht aufgelöst und können auf Dateisystem-
/// Ebene weiterhin aus `root` hinausführen; das prüft dieser Typ nicht.
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
    /// [`Path::components`] durchlaufen. Dabei werden `.`-Komponenten
    /// verworfen; jede `..`-Komponente führt sofort zur Ablehnung, auch wenn
    /// sie lexikalisch innerhalb von `root` bliebe (z. B. `a/../b.toml`). Das
    /// ist strenger als eine reine Root-Grenzprüfung, gibt Aufrufenden aber
    /// eine feste Garantie: das Ergebnis enthält niemals eine `..`-Komponente
    /// aus der Eingabe. Ist `rel` absolut oder enthält eine
    /// Wurzel-/Präfix-Komponente, wird ebenfalls abgelehnt. Es findet **kein**
    /// Dateisystem-Zugriff statt; Symlinks unterhalb von `root` werden nicht
    /// aufgelöst und daher auch nicht auf ein Verlassen von `root` geprüft.
    ///
    /// # Arguments
    /// - `rel` (`&str`): der aufzulösende relative Pfad, geliehen.
    ///
    /// # Returns
    /// Den vollständigen, root-präfixierten [`PathBuf`] bei Erfolg.
    ///
    /// # Errors
    /// - [`PathError::Traversal`]: wenn `rel` absolut ist, eine
    ///   `..`-Komponente enthält (unabhängig von deren Tiefe), oder eine
    ///   Wurzel-/Präfix-Komponente enthält.
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

        // Lexikalischer Durchlauf relativ zum root. Jede ".."-Komponente wird
        // sofort abgelehnt, unabhängig davon, ob sie lexikalisch innerhalb
        // des root bliebe (siehe Doc-Kommentar oben) — es gibt daher keinen
        // Auf-/Abstiegszähler mehr.
        let mut normalized = self.root.clone();

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
                // ".." wird immer abgelehnt, auch innerhalb von root.
                Component::ParentDir => {
                    return Err(PathError::Traversal {
                        input: rel.to_owned(),
                    });
                }
                Component::Normal(part) => {
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
    fn test_resolve_inner_parent_dir_rejected() {
        // ".." wird auch abgelehnt, wenn sie lexikalisch innerhalb von root
        // bliebe — siehe Doc-Kommentar der Methode.
        let err = scope().resolve("a/../b.toml");
        assert!(matches!(err, Err(PathError::Traversal { .. })));
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
