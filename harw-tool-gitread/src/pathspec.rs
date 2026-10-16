//! Literale Pfadfilter (`-- <pfad>…`).
//!
//! # Verantwortung
//! Ein [`Pathspec`] ist eine Liste von Verzeichnis-/Dateipräfixen relativ zur
//! Workspace-Wurzel. Ein Pfad passt, wenn er gleich einem Präfix ist oder
//! darunter liegt. Es gibt bewusst **keine** Glob- und Magic-Syntax: Zeichen
//! wie `*`, `?`, `[`, `\` und ein führendes `:` werden abgelehnt, damit aus
//! einem „Pfad“ nie ein Muster wird, das mehr trifft als gemeint.
//!
//! # Härtung
//! Absolute Pfade, `..`-Glieder, `.git`-Glieder, NUL und überlange Eingaben
//! werden abgelehnt (der Filter greift ohnehin nur auf Pfade, die im Repository
//! stehen, aber ein klarer Fehler ist besser als ein stilles Leerergebnis).

/// Höchstzahl Pfadfilter je Aufruf.
pub const MAX_PATHSPECS: usize = 64;

/// Höchstlänge eines Pfadfilters.
pub const MAX_PATHSPEC_BYTES: usize = 4096;

/// Eine Liste literaler Präfixe; leer = alles.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pathspec {
    prefixes: Vec<Vec<u8>>,
}

impl Pathspec {
    /// Passt auf alles.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    /// Parst Benutzereingaben.
    ///
    /// # Errors
    /// Meldung für Muster, absolute Pfade, `..`, `.git`, NUL und Übergröße.
    pub fn parse(items: &[String]) -> Result<Self, String> {
        if items.len() > MAX_PATHSPECS {
            return Err(format!("at most {MAX_PATHSPECS} paths are allowed"));
        }
        let mut prefixes: Vec<Vec<u8>> = Vec::new();
        for item in items {
            if item.len() > MAX_PATHSPEC_BYTES {
                return Err(format!("a path is longer than {MAX_PATHSPEC_BYTES} bytes"));
            }
            if item.is_empty() {
                return Err("empty path".to_owned());
            }
            if item.starts_with(':') || item.contains(['*', '?', '[', '\\', '\0']) {
                return Err(format!(
                    "path '{}' uses pattern or magic syntax; only literal paths are supported",
                    item.chars().take(64).collect::<String>()
                ));
            }
            if item.starts_with('/') {
                return Err(format!(
                    "path '{}' must be relative to the workspace root",
                    item.chars().take(64).collect::<String>()
                ));
            }
            let mut parts: Vec<&str> = Vec::new();
            for part in item.split('/') {
                match part {
                    "" | "." => {}
                    ".." => return Err("paths must not contain '..'".to_owned()),
                    other if other.eq_ignore_ascii_case(".git") => {
                        return Err("paths must not point into .git".to_owned());
                    }
                    other => parts.push(other),
                }
            }
            if parts.is_empty() {
                // `.` = das ganze Repository
                return Ok(Self::all());
            }
            prefixes.push(parts.join("/").into_bytes());
        }
        prefixes.sort();
        prefixes.dedup();
        Ok(Self { prefixes })
    }

    /// `true`, wenn kein Filter gesetzt ist.
    #[must_use]
    pub fn is_all(&self) -> bool {
        self.prefixes.is_empty()
    }

    /// Passt `path` (Bytes, mit `/` getrennt)?
    #[must_use]
    pub fn matches(&self, path: &[u8]) -> bool {
        self.is_all()
            || self.prefixes.iter().any(|prefix| {
                path.starts_with(prefix)
                    && (path.len() == prefix.len() || path.get(prefix.len()) == Some(&b'/'))
            })
    }

    /// Kann unter dem Verzeichnis `dir` (ohne Schluss-`/`) etwas passen?
    #[must_use]
    pub fn may_contain(&self, dir: &[u8]) -> bool {
        self.is_all()
            || self.prefixes.iter().any(|prefix| {
                // `dir` ist Vorfahr (oder gleich) des Präfixes, oder liegt darunter.
                let ancestor = prefix.starts_with(dir)
                    && (prefix.len() == dir.len() || prefix.get(dir.len()) == Some(&b'/'));
                ancestor || self.matches(dir)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn spec(items: &[&str]) -> Result<Pathspec, String> {
        Pathspec::parse(&items.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn literal_prefixes_match_on_component_boundaries() -> TestResult {
        let spec = spec(&["src", "docs/a.md"])?;
        assert!(spec.matches(b"src"));
        assert!(spec.matches(b"src/lib.rs"));
        assert!(!spec.matches(b"src2/lib.rs"));
        assert!(!spec.matches(b"srcx"));
        assert!(spec.matches(b"docs/a.md"));
        assert!(!spec.matches(b"docs/a.mdx"));
        assert!(!spec.matches(b"docs"));
        assert!(spec.may_contain(b"docs"));
        assert!(spec.may_contain(b"src/deep"));
        assert!(!spec.may_contain(b"other"));
        assert!(!spec.may_contain(b"do"));
        Ok(())
    }

    #[test]
    fn dot_and_empty_list_mean_everything_and_normalisation_works() -> TestResult {
        assert!(spec(&[])?.is_all());
        assert!(spec(&["."])?.is_all());
        assert!(spec(&["./src//x/"])?.matches(b"src/x/y"));
        assert!(Pathspec::all().matches(b"anything"));
        Ok(())
    }

    #[test]
    fn patterns_escapes_and_oversize_are_rejected() -> TestResult {
        for bad in [
            "*.rs",
            "a?b",
            "[a]",
            ":(top)x",
            "a\\b",
            "/etc/passwd",
            "../x",
            "a/../b",
            ".git/config",
            "x/.GIT/y",
            "",
        ] {
            assert!(spec(&[bad]).is_err(), "{bad} must be rejected");
        }
        let long = "a".repeat(MAX_PATHSPEC_BYTES + 1);
        assert!(Pathspec::parse(&[long]).is_err());
        let many: Vec<String> = (0..=MAX_PATHSPECS).map(|i| format!("p{i}")).collect();
        let error = Pathspec::parse(&many)
            .err()
            .ok_or(TestError::Missing("error"))?;
        assert!(error.contains("at most"));
        Ok(())
    }
}
