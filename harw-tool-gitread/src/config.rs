//! Minimaler Leser für `<git-dir>/config`.
//!
//! # Verantwortung
//! Liefert genau die Werte, die die Werkzeuge brauchen (Upstream-Zweig:
//! `branch.<name>.remote` und `branch.<name>.merge`). Das Format wird nach
//! `git-config(1)` gelesen: `[abschnitt]`, `[abschnitt "unter"]`,
//! `schlüssel = wert`, Kommentare mit `#`/`;`, Anführungszeichen.
//!
//! # Grenzen
//! `include`/`includeIf`, mehrzeilige Werte mit `\` am Zeilenende und die
//! Konfiguration außerhalb des Repositories (`~/.gitconfig`) werden **nicht**
//! gelesen. Werte werden nur intern genutzt; URLs (mögliche Zugangsdaten)
//! gibt keines der Werkzeuge aus.

use crate::repo::Repo;

/// Höchstgröße der Konfigurationsdatei.
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// Geparste Konfiguration (Abschnitt, Unterabschnitt, Schlüssel, Wert).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Config {
    entries: Vec<(String, Option<String>, String, String)>,
}

fn strip_comment(value: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' => {
                if let Some(next) = chars.next() {
                    out.push(match next {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                }
            }
            '#' | ';' if !quoted => break,
            other => out.push(other),
        }
    }
    out.trim().to_owned()
}

impl Config {
    /// Parst Konfigurationstext (tolerant: unlesbare Zeilen werden übersprungen).
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut entries = Vec::new();
        let mut section: Option<(String, Option<String>)> = None;
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(header) = line.strip_prefix('[') {
                let Some(end) = header.find(']') else {
                    section = None;
                    continue;
                };
                let inner = header[..end].trim();
                section = Some(match inner.split_once(char::is_whitespace) {
                    Some((name, sub)) => {
                        let sub = sub
                            .trim()
                            .trim_matches('"')
                            .replace("\\\"", "\"")
                            .replace("\\\\", "\\");
                        (name.to_ascii_lowercase(), Some(sub))
                    }
                    None => match inner.split_once('.') {
                        Some((name, sub)) => (name.to_ascii_lowercase(), Some(sub.to_owned())),
                        None => (inner.to_ascii_lowercase(), None),
                    },
                });
                // Wert hinter `]` auf derselben Zeile (`[a] k = v`).
                let tail = header[end + 1..].trim();
                if tail.is_empty() {
                    continue;
                }
                if let Some((name, sub)) = section.clone() {
                    push_pair(&mut entries, &name, sub, tail);
                }
                continue;
            }
            if let Some((name, sub)) = section.clone() {
                push_pair(&mut entries, &name, sub, line);
            }
        }
        Self { entries }
    }

    /// Liest die Konfiguration des Repositories (leer, wenn es sie nicht gibt).
    #[must_use]
    pub fn load(repo: &Repo) -> Self {
        match repo.read_git_file("config", MAX_CONFIG_BYTES) {
            Ok(bytes) => Self::parse(&String::from_utf8_lossy(&bytes)),
            Err(_) => Self::default(),
        }
    }

    /// Letzter Wert von `section[.sub].key`.
    #[must_use]
    pub fn get(&self, section: &str, sub: Option<&str>, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find(|(s, u, k, _)| s == section && u.as_deref() == sub && k.eq_ignore_ascii_case(key))
            .map(|(_, _, _, v)| v.as_str())
    }

    /// Upstream-Zweig von `branch` als Referenzname (`refs/remotes/origin/main`)
    /// und Kurzform (`origin/main`).
    #[must_use]
    pub fn upstream(&self, branch: &str) -> Option<(String, String)> {
        let remote = self.get("branch", Some(branch), "remote")?;
        let merge = self.get("branch", Some(branch), "merge")?;
        let short = merge.strip_prefix("refs/heads/")?;
        if remote == "." {
            return Some((merge.to_owned(), short.to_owned()));
        }
        Some((
            format!("refs/remotes/{remote}/{short}"),
            format!("{remote}/{short}"),
        ))
    }
}

fn push_pair(
    entries: &mut Vec<(String, Option<String>, String, String)>,
    section: &str,
    sub: Option<String>,
    line: &str,
) {
    let (key, value) = match line.split_once('=') {
        Some((key, value)) => (key.trim(), strip_comment(value)),
        None => (line.trim(), "true".to_owned()),
    };
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return;
    }
    entries.push((section.to_owned(), sub, key.to_ascii_lowercase(), value));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn parses_sections_subsections_comments_and_quotes() -> TestResult {
        let config = Config::parse(
            "[core]\n\tbare = false ; comment\n# note\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n[branch.dev]\n\tremote = .\n\tmerge = refs/heads/main\n[user]\n\tname = \"Ada # L\"\n[flag]\n\tbare\n",
        );
        assert_eq!(config.get("core", None, "bare"), Some("false"));
        assert_eq!(config.get("user", None, "name"), Some("Ada # L"));
        assert_eq!(config.get("flag", None, "bare"), Some("true"));
        assert_eq!(config.get("branch", Some("main"), "REMOTE"), Some("origin"));
        assert_eq!(
            config.upstream("main"),
            Some((
                "refs/remotes/origin/main".to_owned(),
                "origin/main".to_owned()
            ))
        );
        assert_eq!(
            config.upstream("dev"),
            Some(("refs/heads/main".to_owned(), "main".to_owned()))
        );
        assert_eq!(config.upstream("none"), None);
        Ok(())
    }

    #[test]
    fn later_values_win_and_garbage_is_skipped() -> TestResult {
        let config = Config::parse("[a]\nk = 1\n[a]\nk = 2\n=bad\n[broken\nx = y\n\u{0}\u{1}\n");
        assert_eq!(config.get("a", None, "k"), Some("2"));
        assert_eq!(config.get("broken", None, "x"), None);
        assert!(Config::parse("").get("a", None, "k").is_none());
        let none = Config::default().upstream("x");
        assert!(none.is_none());
        Ok(())
    }
}
