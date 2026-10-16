//! Tool-Index: eine kompakte, aus der Registry **abgeleitete** Übersicht, welche
//! Werkzeuge es gibt, wozu sie gehören und in welchen Profilen sie verfügbar sind.
//!
//! # Verantwortung
//! Beantwortet für Mensch und Modell „welches Tool für was?“, ohne alle
//! Schemas zu laden. Quellen sind ausschließlich vorhandene Daten:
//! - [`crate::capability_catalog::CATALOG`]: Name, Provider, Fähigkeitsklasse;
//! - [`RegistryProfile::registered_tool_names`]: in welchen Profilen ein Tool
//!   registriert ist;
//! - optional eine Kurzbeschreibung, die der Aufrufer aus den echten
//!   `ToolSpec`-Beschreibungen liefert (erster Satz). Nichts davon wird hier
//!   von Hand dupliziert, der Index kann also nicht von der Registry abweichen.
//!
//! Die Familie ist strukturell der Namenspräfix vor dem ersten `.`; Namen ohne
//! `.` gehören zur Familie `core`. Der Index vergibt keine Rechte: er zeigt nur,
//! was die Registry ohnehin beschreibt.

use crate::capability_catalog::{CATALOG, CapabilityClass, ToolPattern};
use crate::profile::RegistryProfile;

/// Kurzname eines Profils, abgeleitet aus dem Variantennamen (`ReadOnlyExplore`
/// wird zu `read-only-explore`). Quelle für Profile und Namen ist
/// [`RegistryProfile::ALL`]; es gibt keine zweite Liste, die eine neue
/// Variante auslassen könnte.
fn profile_label(profile: RegistryProfile) -> String {
    let name = format!("{profile:?}");
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Eine Zeile des Index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolIndexEntry {
    /// Vollständiger Werkzeugname (`fs.read`).
    pub name: &'static str,
    /// Familie: Namenspräfix vor dem ersten `.`, sonst `core`.
    pub family: &'static str,
    /// Stabile Provider-Id (`fs`, `web`, …).
    pub provider: &'static str,
    /// Implementierendes Crate.
    pub crate_name: &'static str,
    /// Fähigkeitsklasse (was das Tool bewirkt).
    pub class: CapabilityClass,
    /// Von jedem Runner ohne eigenes Feature bedient.
    pub always_available: bool,
    /// Profile (Kurzname), in denen das Tool registriert ist.
    pub profiles: Vec<String>,
    /// Erster Satz der echten Beschreibung, falls geliefert.
    pub summary: Option<String>,
}

/// Der Index über alle exakt benannten Katalogzeilen. Der Speicher ist privat:
/// die Sortierung nach Familie, dann Name, ist eine Invariante (Familien sind
/// zusammenhängend) und wird nur von [`ToolIndex::build`] hergestellt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolIndex {
    entries: Vec<ToolIndexEntry>,
}

/// Familie eines Namens: Präfix vor dem ersten `.`, sonst `core`.
#[must_use]
pub fn family_of(name: &str) -> &str {
    match name.split_once('.') {
        Some((family, _)) if !family.is_empty() => family,
        _ => "core",
    }
}

/// Erster Satz einer Beschreibung (bis zum ersten `.`, `!` oder `?` vor
/// Leerraum bzw. Textende), Leerraum und Zeilenumbrüche normalisiert, auf 140
/// Zeichen gekürzt.
#[must_use]
pub fn first_sentence(description: &str) -> String {
    const MAX: usize = 140;
    let text = description.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut end = text.len();
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(c, '.' | '!' | '?') && chars.peek().is_none_or(|(_, n)| n.is_whitespace()) {
            end = i + c.len_utf8();
            break;
        }
    }
    let sentence = &text[..end];
    match sentence.char_indices().nth(MAX) {
        None => sentence.to_owned(),
        Some(_) => sentence.chars().take(MAX - 1).collect::<String>() + "…",
    }
}

impl ToolIndex {
    /// Baut den Index aus Katalog und Profilen. `describe` liefert optional die
    /// Beschreibung eines Tools aus den echten Specs.
    #[must_use]
    pub fn build(describe: &dyn Fn(&str) -> Option<String>) -> Self {
        let mut entries: Vec<ToolIndexEntry> = CATALOG
            .iter()
            .filter_map(|row| match row.pattern {
                ToolPattern::Exact(name) => Some((name, row)),
                ToolPattern::Prefix(_) => None,
            })
            .map(|(name, row)| ToolIndexEntry {
                name,
                family: family_of(name),
                provider: row.provider.id,
                crate_name: row.provider.crate_name,
                class: row.class,
                always_available: row.always_available,
                profiles: RegistryProfile::ALL
                    .iter()
                    .filter(|p| p.registered_tool_names().contains(&name))
                    .map(|&p| profile_label(p))
                    .collect(),
                summary: describe(name).map(|d| first_sentence(&d)),
            })
            .collect();
        entries.sort_by(|a, b| (a.family, a.name).cmp(&(b.family, b.name)));
        Self { entries }
    }

    /// Alle Einträge, sortiert nach Familie, dann Name (nur lesend).
    #[must_use]
    pub fn entries(&self) -> &[ToolIndexEntry] {
        &self.entries
    }

    /// Einträge einer Familie.
    #[must_use]
    pub fn family(&self, family: &str) -> Vec<&ToolIndexEntry> {
        self.entries.iter().filter(|e| e.family == family).collect()
    }

    /// Alle Familien, sortiert, mit Anzahl.
    #[must_use]
    pub fn families(&self) -> Vec<(&'static str, usize)> {
        let mut out: Vec<(&'static str, usize)> = Vec::new();
        for e in &self.entries {
            match out.last_mut() {
                Some((f, n)) if *f == e.family => *n += 1,
                _ => out.push((e.family, 1)),
            }
        }
        out
    }

    /// Kompakte Textform. Ohne Filter nur die Familienübersicht, mit Familie die
    /// Werkzeuge samt Klasse, Profilen und Kurzbeschreibung. Unbekannte Familie
    /// liefert eine Fehlermeldung mit den vorhandenen Familien.
    #[must_use]
    pub fn render(&self, family: Option<&str>) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        match family {
            None => {
                out.push_str("Tool families (call with a family name for details):\n");
                for (f, n) in self.families() {
                    let _ = writeln!(out, "- {f} ({n})");
                }
            }
            Some(f) => {
                let rows = self.family(f);
                if rows.is_empty() {
                    let names: Vec<&str> = self.families().iter().map(|&(n, _)| n).collect();
                    let _ = write!(out, "Unknown family {f:?}. Known: {}", names.join(", "));
                    return out;
                }
                for e in rows {
                    let _ = write!(out, "{} [{}]", e.name, e.class.as_str());
                    if let Some(s) = &e.summary {
                        let _ = write!(out, " {s}");
                    }
                    if e.always_available {
                        out.push_str(" (always available)");
                    } else if !e.profiles.is_empty() {
                        let _ = write!(out, " (profiles: {})", e.profiles.join(", "));
                    }
                    out.push('\n');
                }
            }
        }
        out
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

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn every_exact_catalog_row_is_indexed_once() -> TestResult {
        let idx = ToolIndex::build(&none);
        let exact = CATALOG
            .iter()
            .filter(|r| matches!(r.pattern, ToolPattern::Exact(_)))
            .count();
        ensure(idx.entries().len() == exact, "one entry per exact row")?;
        let mut names: Vec<_> = idx.entries().iter().map(|e| e.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        ensure(names.len() == before, "no duplicate names")
    }

    #[test]
    fn every_registry_profile_is_covered() -> TestResult {
        let idx = ToolIndex::build(&none);
        for p in RegistryProfile::ALL {
            let label = profile_label(*p);
            for tool in p.registered_tool_names() {
                let Some(entry) = idx.entries().iter().find(|e| e.name == tool) else {
                    continue; // prefix-only tools are not exact catalog rows
                };
                ensure(
                    entry.profiles.contains(&label),
                    "profile missing from an indexed tool it registers",
                )?;
            }
        }
        ensure(
            profile_label(RegistryProfile::ReadOnlyExplore) == "read-only-explore",
            "kebab case",
        )
    }

    #[test]
    fn family_is_structural() -> TestResult {
        ensure(family_of("fs.read") == "fs", "prefix")?;
        ensure(family_of("delegate_wave") == "core", "no dot")?;
        ensure(family_of(".x") == "core", "empty prefix")
    }

    #[test]
    fn profiles_come_from_the_registry() -> TestResult {
        let idx = ToolIndex::build(&none);
        let read = idx
            .entries
            .iter()
            .find(|e| e.name == "fs.read")
            .ok_or(TestError::Missing("fs.read"))?;
        ensure(read.family == "fs" && read.provider == "fs", "provider")?;
        ensure(
            read.profiles.iter().any(|p| p == "full"),
            "registered in the full profile",
        )?;
        ensure(!read.profiles.is_empty(), "has profiles")
    }

    #[test]
    fn summary_uses_only_the_first_sentence() -> TestResult {
        ensure(
            first_sentence("Reads a file. Returns text.\nMore.") == "Reads a file.",
            "sentence",
        )?;
        ensure(
            first_sentence("No period here") == "No period here",
            "no period",
        )?;
        ensure(
            first_sentence("v1.2 is fine. Next") == "v1.2 is fine.",
            "dot inside token",
        )?;
        ensure(first_sentence("Run it! More") == "Run it!", "exclamation")?;
        ensure(first_sentence("Is it ok? Yes.") == "Is it ok?", "question")?;
        ensure(
            first_sentence("Reads a long\nwrapped line. Next") == "Reads a long wrapped line.",
            "wrapped first sentence",
        )?;
        ensure(
            first_sentence("  spaced   out . x") == "spaced out .",
            "whitespace is normalized",
        )?;
        let long = "x".repeat(300);
        ensure(first_sentence(&long).chars().count() == 140, "capped")
    }

    #[test]
    fn render_overview_and_family_and_unknown() -> TestResult {
        let idx = ToolIndex::build(&|n| (n == "fs.read").then(|| "Reads a file. More.".to_owned()));
        let overview = idx.render(None);
        ensure(
            overview.contains("- fs (") && overview.contains("Tool families"),
            "overview",
        )?;
        let fs = idx.render(Some("fs"));
        ensure(fs.contains("fs.read [read] Reads a file."), "summary shown")?;
        ensure(fs.contains("fs.write [write]"), "class shown")?;
        let unknown = idx.render(Some("nope"));
        ensure(
            unknown.starts_with("Unknown family") && unknown.contains("fs"),
            "unknown lists families",
        )
    }

    #[test]
    fn families_are_sorted_and_counted() -> TestResult {
        let idx = ToolIndex::build(&none);
        let fams = idx.families();
        let mut sorted = fams.clone();
        sorted.sort_by_key(|&(f, _)| f);
        ensure(fams == sorted, "sorted")?;
        ensure(
            fams.iter().map(|&(_, n)| n).sum::<usize>() == idx.entries().len(),
            "counts add up",
        )
    }
}
