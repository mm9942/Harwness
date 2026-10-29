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
//!
//! # Werkzeug → Skill (PL-97 B2, PL-95 S4)
//! [`ToolSkillMap`] verknüpft ein Werkzeug mit dem gebündelten Skill, der es
//! erklärt. Quellen: die `tools = [...]`-Listen der `skill.toml` (über einen
//! [`SkillIndex`]) und die Provider-Bindungen `#[tool(skill = "...")]`
//! (`TOOL_SKILLS`, gesammelt in [`crate::profile::provider_tool_skills`]).
//! [`ToolIndex::with_skills`] trägt die Verknüpfung in die Einträge ein.
//!
//! # Verbraucher
//! Das Modell-Werkzeug `tool.index` (`harw-ops/src/tool_index_op.rs`),
//! `harw tool list|describe` (`harw-cli/src/tool_cmd.rs`) und der
//! System-Prompt-Block `## Tool families` ([`families_of`], übergeben an
//! `AgentIdentity::with_tool_families` in `profile.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use harw_catalog::SkillIndex;

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
    /// The bundled skill that explains this tool ([`ToolSkillMap::skill_for`]),
    /// set by [`ToolIndex::with_skills`]; `None` without a link. Descriptive
    /// only, never a permission.
    pub skill: Option<String>,
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

/// The tool families of `names` with their tool counts, sorted by family
/// ([`family_of`]); duplicate names count once.
///
/// This is the data of the system-prompt block `## Tool families`
/// (`AgentIdentity::with_tool_families`): it depends only on the set of
/// names, never on their order, so the prompt prefix stays cache-stable.
///
/// # Examples
/// ```rust
/// use harw_registry_defaults::tool_index::families_of;
///
/// let names = ["git.log", "fs.read", "fs.write", "delegate_wave"];
/// assert_eq!(
///     families_of(&names),
///     vec![("core".to_owned(), 1), ("fs".to_owned(), 2), ("git".to_owned(), 1)]
/// );
/// ```
#[must_use]
pub fn families_of<S: AsRef<str>>(names: &[S]) -> Vec<(String, usize)> {
    let unique: BTreeSet<&str> = names.iter().map(AsRef::as_ref).collect();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for name in unique {
        *counts.entry(family_of(name)).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(family, count)| (family.to_owned(), count))
        .collect()
}

/// The names of every exactly named catalog row
/// ([`crate::capability_catalog::CATALOG`]), in catalog order: every tool
/// name a session of this binary can register, apart from the prefix rows
/// (`transfer_to_*`).
#[must_use]
pub fn catalog_tool_names() -> Vec<&'static str> {
    CATALOG
        .iter()
        .filter_map(|row| match row.pattern {
            ToolPattern::Exact(name) => Some(name),
            ToolPattern::Prefix(_) => None,
        })
        .collect()
}

/// Tool → skill links (PL-97 B2, PL-95 S4): which skill explains a tool.
///
/// # Sources
/// - the `tools = [...]` list of every skill manifest (`skill.toml`) in a
///   [`SkillIndex`] (the reverse map: tool → declaring skills);
/// - the provider bindings `#[tool(skill = "...")]` (`TOOL_SKILLS` of the
///   concrete providers, [`crate::profile::provider_tool_skills`]).
///
/// A binding wins. Without one, the link is the **only** skill that declares
/// the tool; a tool several skills declare (e.g. `web.fetch`) has no link.
/// [`Self::disagreements`] lists bindings the manifests contradict. A link is
/// descriptive: it never grants a tool or a right.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolSkillMap {
    /// Werkzeug → Skills, deren Manifest es in `tools` nennt.
    declared: BTreeMap<String, BTreeSet<String>>,
    /// Werkzeug → Skill aus `TOOL_SKILLS`.
    bound: BTreeMap<String, String>,
}

impl ToolSkillMap {
    /// The links of the skills in `index` plus the provider `bindings`
    /// (`(tool, skill)`).
    #[must_use]
    pub fn new(index: &SkillIndex, bindings: &[(&str, &str)]) -> Self {
        let mut declared: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for entry in index.entries() {
            for tool in &entry.tools {
                declared
                    .entry(tool.clone())
                    .or_default()
                    .insert(entry.name.clone());
            }
        }
        let bound = bindings
            .iter()
            .map(|(tool, skill)| ((*tool).to_owned(), (*skill).to_owned()))
            .collect();
        Self { declared, bound }
    }

    /// The links of this binary: the bundled skills (`SkillIndex::build`
    /// without layers) plus [`crate::profile::provider_tool_skills`]. Built
    /// once per process.
    #[must_use]
    pub fn bundled() -> &'static Self {
        static BUNDLED: OnceLock<ToolSkillMap> = OnceLock::new();
        BUNDLED.get_or_init(|| {
            Self::new(
                &SkillIndex::build(&[]),
                &crate::profile::provider_tool_skills(),
            )
        })
    }

    /// The skill linked to `tool`: its provider binding, otherwise the only
    /// skill whose manifest declares it.
    #[must_use]
    pub fn skill_for(&self, tool: &str) -> Option<&str> {
        if let Some(skill) = self.bound.get(tool) {
            return Some(skill.as_str());
        }
        let skills = self.declared.get(tool)?;
        match (skills.len(), skills.iter().next()) {
            (1, Some(skill)) => Some(skill.as_str()),
            _ => None,
        }
    }

    /// The skills whose manifest declares `tool`, sorted.
    #[must_use]
    pub fn declaring(&self, tool: &str) -> Vec<&str> {
        self.declared
            .get(tool)
            .map(|skills| skills.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    /// The provider bindings as `(tool, skill)`, sorted by tool.
    #[must_use]
    pub fn bindings(&self) -> Vec<(&str, &str)> {
        self.bound
            .iter()
            .map(|(tool, skill)| (tool.as_str(), skill.as_str()))
            .collect()
    }

    /// Provider bindings the manifests contradict, as `(tool, bound skill)`:
    /// the bound skill does not declare the tool in its `tools` list (or is
    /// not in the index at all). Empty when both sources agree.
    #[must_use]
    pub fn disagreements(&self) -> Vec<(&str, &str)> {
        self.bound
            .iter()
            .filter(|(tool, skill)| {
                !self
                    .declared
                    .get(tool.as_str())
                    .is_some_and(|skills| skills.contains(skill.as_str()))
            })
            .map(|(tool, skill)| (tool.as_str(), skill.as_str()))
            .collect()
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
                skill: None,
            })
            .collect();
        entries.sort_by(|a, b| (a.family, a.name).cmp(&(b.family, b.name)));
        Self { entries }
    }

    /// Fills [`ToolIndexEntry::skill`] of every entry from `skills`
    /// ([`ToolSkillMap::skill_for`]).
    #[must_use]
    pub fn with_skills(mut self, skills: &ToolSkillMap) -> Self {
        for entry in &mut self.entries {
            entry.skill = skills.skill_for(entry.name).map(str::to_owned);
        }
        self
    }

    /// Alle Einträge, sortiert nach Familie, dann Name (nur lesend).
    #[must_use]
    pub fn entries(&self) -> &[ToolIndexEntry] {
        &self.entries
    }

    /// The entry of the tool `name`, if the catalog names it exactly.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ToolIndexEntry> {
        self.entries.iter().find(|entry| entry.name == name)
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
                    // Klammerzusatz: verknüpfter Skill, dann Verfügbarkeit.
                    let mut notes: Vec<String> = Vec::new();
                    if let Some(skill) = &e.skill {
                        notes.push(format!("skill: {skill}"));
                    }
                    if e.always_available {
                        notes.push("always available".to_owned());
                    } else if !e.profiles.is_empty() {
                        notes.push(format!("profiles: {}", e.profiles.join(", ")));
                    }
                    if !notes.is_empty() {
                        let _ = write!(out, " ({})", notes.join("; "));
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

    /// An index over an in-memory bundle: `(name, tools)`.
    fn skill_index_of(skills: &[(&str, &[&str])]) -> SkillIndex {
        let mut files: Vec<(String, String)> = Vec::new();
        for (name, tools) in skills {
            let tools = tools
                .iter()
                .map(|tool| format!("\"{tool}\""))
                .collect::<Vec<_>>()
                .join(", ");
            files.push((
                format!("skills/{name}/skill.toml"),
                format!(
                    "name = \"{name}\"\ndescription = \"Use when {name} matters.\"\ntools = [{tools}]\n"
                ),
            ));
            files.push((
                format!("skills/{name}/instructions.md"),
                format!("# {name}\n\nBody.\n"),
            ));
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, content)| (path.as_str(), content.as_str()))
            .collect();
        SkillIndex::build_with_bundle(&[], &refs)
    }

    #[test]
    fn skill_map_links_the_only_declaring_skill_and_prefers_bindings() -> TestResult {
        let index = skill_index_of(&[
            ("sys-tools", &["sys.ps", "sys.free"]),
            ("research-a", &["web.fetch"]),
            ("research-b", &["web.fetch", "web.search"]),
        ]);
        let map = ToolSkillMap::new(&index, &[]);
        ensure(
            map.skill_for("sys.ps") == Some("sys-tools"),
            "unique declarer",
        )?;
        ensure(map.skill_for("web.search") == Some("research-b"), "unique")?;
        ensure(map.skill_for("web.fetch").is_none(), "ambiguous: no link")?;
        ensure(
            map.declaring("web.fetch") == ["research-a", "research-b"],
            "declarers sorted",
        )?;
        ensure(map.skill_for("fs.read").is_none(), "undeclared")?;
        ensure(map.disagreements().is_empty(), "no bindings")?;

        // Eine Bindung entscheidet die Mehrdeutigkeit, muss aber zum Manifest
        // passen.
        let bound = ToolSkillMap::new(&index, &[("web.fetch", "research-a")]);
        ensure(
            bound.skill_for("web.fetch") == Some("research-a"),
            "binding",
        )?;
        ensure(bound.disagreements().is_empty(), "binding agrees")?;
        let wrong = ToolSkillMap::new(&index, &[("sys.ps", "research-a"), ("fs.read", "fs-tools")]);
        ensure(
            wrong.disagreements() == [("fs.read", "fs-tools"), ("sys.ps", "research-a")],
            "contradicting and dangling bindings are reported",
        )
    }

    /// PL-97 B2: beide Quellen der Werkzeug→Skill-Verknüpfung stimmen
    /// überein — jede Provider-Bindung (`TOOL_SKILLS` der konkreten Provider)
    /// nennt einen gebündelten Skill, dessen `skill.toml` das Werkzeug in
    /// `tools` führt; umgekehrt verweist jedes Werkzeug der vier gebündelten
    /// Werkzeug-Skills eindeutig auf seinen Skill und ist ein echtes,
    /// katalogisiertes Werkzeug. (Heute bindet noch kein Provider per
    /// `#[tool(skill = ...)]`; der Test hält künftige Bindungen ehrlich.)
    #[test]
    fn provider_bindings_and_bundled_skill_manifests_agree() -> TestResult {
        let bundled = SkillIndex::build(&[]);
        let bindings = crate::profile::provider_tool_skills();
        let map = ToolSkillMap::new(&bundled, &bindings);
        ensure(
            map.disagreements().is_empty(),
            &format!("bindings contradict skill.toml: {:?}", map.disagreements()),
        )?;
        for (tool, skill) in &bindings {
            ensure(
                map.skill_for(tool) == Some(*skill),
                &format!("{tool}: binding {skill}"),
            )?;
        }
        let catalog = catalog_tool_names();
        for skill in ["sys-tools", "fsread-tools", "git-read-tools", "cargo-tools"] {
            let entry = bundled
                .get(skill)
                .ok_or_else(|| TestError::Unexpected(format!("{skill} not bundled")))?;
            ensure(!entry.tools.is_empty(), skill)?;
            for tool in &entry.tools {
                ensure(
                    map.skill_for(tool) == Some(skill),
                    &format!(
                        "{tool} must link {skill}, declared by {:?}",
                        map.declaring(tool)
                    ),
                )?;
                ensure(
                    catalog.contains(&tool.as_str()),
                    &format!("{skill} declares {tool}, which no catalog row names"),
                )?;
            }
        }
        ensure(
            ToolSkillMap::bundled().skill_for("sys.ps") == Some("sys-tools"),
            "bundled map",
        )
    }

    #[test]
    fn index_entries_carry_the_skill_link_and_render_it() -> TestResult {
        let idx = ToolIndex::build(&none).with_skills(ToolSkillMap::bundled());
        let ps = idx.get("sys.ps").ok_or(TestError::Missing("sys.ps"))?;
        ensure(ps.skill.as_deref() == Some("sys-tools"), "linked")?;
        let read = idx.get("fs.read").ok_or(TestError::Missing("fs.read"))?;
        ensure(read.skill.is_none(), "fs.read has no skill")?;
        let sys = idx.render(Some("sys"));
        ensure(
            sys.lines()
                .any(|line| line.starts_with("sys.ps [") && line.contains("skill: sys-tools")),
            &format!("skill rendered: {sys}"),
        )?;
        ensure(
            ToolIndex::build(&none)
                .get("sys.ps")
                .is_some_and(|e| e.skill.is_none()),
            "build alone links nothing",
        )
    }

    #[test]
    fn families_of_ignores_order_and_duplicates() -> TestResult {
        let a = families_of(&["fs.read", "git.log", "fs.write", "fs.read"]);
        let b = families_of(&["git.log", "fs.write", "fs.read"]);
        ensure(a == b, "order and duplicates do not matter")?;
        ensure(
            a == [("fs".to_owned(), 2), ("git".to_owned(), 1)],
            "counts per family",
        )?;
        ensure(families_of::<&str>(&[]).is_empty(), "empty")
    }

    #[test]
    fn catalog_tool_names_are_the_exact_rows() -> TestResult {
        let names = catalog_tool_names();
        ensure(names.contains(&"fs.read"), "fs.read")?;
        ensure(
            !names.iter().any(|name| name.ends_with('*')),
            "no prefix rows",
        )?;
        ensure(
            names.len() == ToolIndex::build(&none).entries().len(),
            "one name per index entry",
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
