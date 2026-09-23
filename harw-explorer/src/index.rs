//! Aufbau und Abfrage des [`ExplorerIndex`].
//!
//! [`ExplorerIndex::build`] verbindet Durchlauf ([`crate::walk`]),
//! Projekterkennung ([`crate::projects`]) und Relationssuche
//! ([`crate::relations`]). Die Abfragen (`tree`, `find`, `children`,
//! `project_at`, `summary`) arbeiten ausschließlich auf den bereits
//! gesammelten, nach Pfad sortierten Knoten und nutzen dort, wo es geht,
//! binäre Suche statt linearer Durchläufe.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use globset::GlobBuilder;

use crate::{
    ExplorerError, ExplorerIndex, ExplorerOptions, ExplorerResult, FileKind, Node, Project,
};

/// Wie viele Dateiarten [`ExplorerIndex::summary`] höchstens einzeln nennt.
const SUMMARY_KINDS: usize = 8;

impl ExplorerIndex {
    /// Baut den Index für `root`.
    ///
    /// Die Wurzel wird kanonisiert; existiert sie nicht oder ist sie kein
    /// Verzeichnis, liefert die Funktion [`ExplorerError::InvalidRoot`].
    ///
    /// # Errors
    ///
    /// [`ExplorerError::InvalidRoot`] für eine ungültige Wurzel, sonst die
    /// Fehler des Durchlaufs.
    pub fn build(root: &Path, opts: &ExplorerOptions) -> ExplorerResult<Self> {
        let root = std::fs::canonicalize(root)
            .map_err(|_| ExplorerError::InvalidRoot(root.to_path_buf()))?;
        if !root.is_dir() {
            return Err(ExplorerError::InvalidRoot(root));
        }
        let (mut nodes, truncated) = crate::walk::walk(&root, opts)?;
        if !nodes.windows(2).all(|pair| pair[0].path <= pair[1].path) {
            nodes.sort_by(|a, b| a.path.cmp(&b.path));
        }
        let projects = crate::projects::detect_projects(&root, &nodes);
        let relations = crate::relations::find_relations(&root, &projects, &nodes);
        Ok(Self {
            root,
            nodes,
            projects,
            relations,
            truncated,
        })
    }

    /// Eingerückter Textbaum unterhalb von `under` (relativ, leer = Wurzel).
    ///
    /// Verzeichnisse stehen vor Dateien, jeweils nach Namen sortiert;
    /// Projektwurzeln tragen Plaketten `[art name]`, Dateien Art und Größe.
    /// Nach `max_entries` Zeilen wird mit `… (N weitere)` abgebrochen. Ein
    /// unbekanntes `under` ergibt `(nicht gefunden)`.
    #[must_use]
    pub fn tree(&self, under: &Path, max_depth: usize, max_entries: usize) -> String {
        let under = self.relative(under);
        let under: &Path = &under;
        let max_depth = max_depth.max(1);
        let badges = self.badge_map();

        if !under.as_os_str().is_empty() {
            match self.node(under) {
                None => return "(nicht gefunden)".to_owned(),
                Some(node) if node.kind != FileKind::Dir => {
                    let mut out = String::new();
                    push_line(&mut out, node, 0, &badges);
                    out.pop();
                    return out;
                }
                Some(_) => {}
            }
        }

        let mut out = String::new();
        let mut printed = 0usize;
        let mut full = false;
        self.tree_rec(
            under,
            1,
            max_depth,
            max_entries,
            &badges,
            &mut out,
            &mut printed,
            &mut full,
        );

        if printed == 0 && !full {
            return "(leer)".to_owned();
        }
        if full {
            let base = path_depth(under);
            let (start, end) = self.subtree_range(under);
            let total = self.nodes[start..end]
                .iter()
                .filter(|n| path_depth(&n.path) > base && path_depth(&n.path) <= base + max_depth)
                .count();
            let rest = total.saturating_sub(printed);
            if rest > 0 {
                let _ = writeln!(out, "… ({rest} weitere)");
            }
        }
        while out.ends_with('\n') {
            out.pop();
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn tree_rec(
        &self,
        dir: &Path,
        level: usize,
        max_depth: usize,
        max_entries: usize,
        badges: &HashMap<&Path, Vec<&Project>>,
        out: &mut String,
        printed: &mut usize,
        full: &mut bool,
    ) {
        for child in self.children(dir) {
            if *printed >= max_entries {
                *full = true;
                return;
            }
            push_line(out, child, level - 1, badges);
            *printed += 1;
            if child.kind == FileKind::Dir && level < max_depth {
                self.tree_rec(
                    &child.path,
                    level + 1,
                    max_depth,
                    max_entries,
                    badges,
                    out,
                    printed,
                    full,
                );
                if *full {
                    return;
                }
            }
        }
    }

    /// Sucht Einträge nach `query`.
    ///
    /// Enthält `query` Glob-Zeichen (`*`, `?`, `[`), wird es als Glob gegen
    /// den relativen Pfad und den Dateinamen geprüft, sonst als Teilstring
    /// (jeweils ohne Groß-/Kleinschreibung). Exakte Dateinamen-Treffer
    /// stehen vorn, danach kürzere Pfade.
    #[must_use]
    pub fn find(&self, query: &str, limit: usize) -> Vec<&Node> {
        let query = query.trim();
        if query.is_empty() || limit == 0 {
            return Vec::new();
        }
        let needle = query.to_lowercase();
        let glob = if query.contains(['*', '?', '[']) {
            GlobBuilder::new(query)
                .case_insensitive(true)
                .literal_separator(false)
                .build()
                .ok()
                .map(|g| g.compile_matcher())
        } else {
            None
        };

        let mut hits: Vec<(u8, usize, &Node)> = Vec::new();
        for node in &self.nodes {
            let path = slash_path(&node.path);
            let name = node
                .path
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default();
            let (matched, exact) = match &glob {
                Some(glob) => {
                    let by_name = glob.is_match(name.as_ref());
                    (by_name || glob.is_match(&path), by_name)
                }
                None => (
                    path.to_lowercase().contains(&needle),
                    name.to_lowercase() == needle,
                ),
            };
            if matched {
                hits.push((u8::from(!exact), path.len(), node));
            }
        }
        hits.sort_by(|a, b| {
            (a.0, a.1)
                .cmp(&(b.0, b.1))
                .then_with(|| a.2.path.cmp(&b.2.path))
        });
        hits.into_iter()
            .take(limit)
            .map(|(_, _, node)| node)
            .collect()
    }

    /// Innerstes Projekt, dessen Wurzel `path` enthält oder gleich ist.
    ///
    /// Bei mehreren Projekten mit derselben Wurzel gewinnt das zuerst
    /// erkannte.
    #[must_use]
    pub fn project_at(&self, path: &Path) -> Option<&Project> {
        let path = self.relative(path);
        let mut best: Option<(usize, &Project)> = None;
        for project in &self.projects {
            if !path.starts_with(&project.root) {
                continue;
            }
            let depth = path_depth(&project.root);
            if best.is_none_or(|(d, _)| depth > d) {
                best = Some((depth, project));
            }
        }
        best.map(|(_, project)| project)
    }

    /// Direkte Kinder von `dir` (relativ, leer = Wurzel): Verzeichnisse
    /// zuerst, dann nach Namen.
    #[must_use]
    pub fn children(&self, dir: &Path) -> Vec<&Node> {
        let dir = self.relative(dir);
        let dir: &Path = &dir;
        let child_depth = path_depth(dir) + 1;
        let (start, end) = self.subtree_range(dir);
        let mut out = Vec::new();
        let mut i = start;
        while i < end {
            let node = &self.nodes[i];
            if node.path.parent() == Some(dir) && path_depth(&node.path) == child_depth {
                out.push(node);
            }
            // Den Teilbaum dieses Eintrags überspringen (zusammenhängend,
            // da nach Pfadkomponenten sortiert).
            let rest = &self.nodes[i + 1..end];
            i += 1 + rest.partition_point(|n| n.path.starts_with(&node.path));
        }
        out.sort_by_key(|n| n.kind != FileKind::Dir);
        out
    }

    /// Einzeilige Übersicht: Einträge je Art, Projekte je Art, ggf.
    /// `(gekürzt)`.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut kinds: HashMap<&str, usize> = HashMap::new();
        for node in &self.nodes {
            *kinds.entry(node.kind.label()).or_default() += 1;
        }
        let mut projects: HashMap<&str, usize> = HashMap::new();
        for project in &self.projects {
            *projects.entry(project.kind.label()).or_default() += 1;
        }

        let mut out = format!("{} Einträge", self.nodes.len());
        let kinds = ranked(kinds);
        if !kinds.is_empty() {
            let mut parts: Vec<String> = kinds
                .iter()
                .take(SUMMARY_KINDS)
                .map(|(label, count)| format!("{label} {count}"))
                .collect();
            if kinds.len() > SUMMARY_KINDS {
                parts.push("…".to_owned());
            }
            let _ = write!(out, " ({})", parts.join(", "));
        }
        let _ = write!(out, " · {} Projekte", self.projects.len());
        let projects = ranked(projects);
        if !projects.is_empty() {
            let parts: Vec<String> = projects
                .iter()
                .map(|(label, count)| format!("{label} {count}"))
                .collect();
            let _ = write!(out, " ({})", parts.join(", "));
        }
        if self.truncated {
            out.push_str(" (gekürzt)");
        }
        out
    }

    /// Normalisiert einen Anfragepfad auf „relativ zur Wurzel“: absolute
    /// Pfade unterhalb der Wurzel werden gekürzt, `.`-Komponenten entfernt.
    fn relative<'a>(&self, path: &'a Path) -> Cow<'a, Path> {
        let path = path.strip_prefix(&self.root).unwrap_or(path);
        if path.components().any(|c| matches!(c, Component::CurDir)) {
            Cow::Owned(
                path.components()
                    .filter(|c| !matches!(c, Component::CurDir))
                    .collect::<PathBuf>(),
            )
        } else {
            Cow::Borrowed(path)
        }
    }

    /// Knoten mit exakt diesem Pfad (binäre Suche).
    fn node(&self, path: &Path) -> Option<&Node> {
        self.nodes
            .binary_search_by(|n| n.path.as_path().cmp(path))
            .ok()
            .map(|i| &self.nodes[i])
    }

    /// Indexbereich aller echten Nachfahren von `dir` in `nodes`.
    fn subtree_range(&self, dir: &Path) -> (usize, usize) {
        if dir.as_os_str().is_empty() {
            return (0, self.nodes.len());
        }
        let start = self.nodes.partition_point(|n| n.path.as_path() <= dir);
        let end = start + self.nodes[start..].partition_point(|n| n.path.starts_with(dir));
        (start, end)
    }

    fn badge_map(&self) -> HashMap<&Path, Vec<&Project>> {
        let mut map: HashMap<&Path, Vec<&Project>> = HashMap::new();
        for project in &self.projects {
            map.entry(project.root.as_path()).or_default().push(project);
        }
        map
    }
}

/// Eine Baumzeile für `node` mit `indent` Ebenen Einrückung.
fn push_line(out: &mut String, node: &Node, indent: usize, badges: &HashMap<&Path, Vec<&Project>>) {
    let name = node
        .path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    for _ in 0..indent {
        out.push_str("  ");
    }
    if node.kind == FileKind::Dir {
        let _ = write!(out, "{name}/");
        if let Some(projects) = badges.get(node.path.as_path()) {
            out.push(' ');
            for project in projects {
                let _ = write!(out, " [{} {}]", project.kind.label(), project.name);
            }
        }
    } else {
        let _ = write!(
            out,
            "{name}  ({}, {})",
            node.kind.label(),
            human_size(node.size)
        );
    }
    if node.ignored {
        out.push_str("  ·ignoriert");
    }
    out.push('\n');
}

/// Anzahl normaler Komponenten eines relativen Pfads.
fn path_depth(path: &Path) -> usize {
    path.components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count()
}

/// Relativer Pfad mit `/` als Trenner.
fn slash_path(path: &Path) -> String {
    let mut out = String::new();
    for (i, comp) in path.components().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(&comp.as_os_str().to_string_lossy());
    }
    out
}

/// Menschenlesbare Größe (`512 B`, `3.2 KiB`, `1.0 MiB`, …).
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Sortiert Zählungen absteigend nach Anzahl, bei Gleichstand nach Label.
fn ranked(counts: HashMap<&str, usize>) -> Vec<(&str, usize)> {
    let mut sorted: Vec<(&str, usize)> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProjectKind;

    fn dir(path: &str) -> Node {
        Node {
            path: PathBuf::from(path),
            kind: FileKind::Dir,
            size: 0,
            depth: u16::try_from(path.split('/').count()).unwrap_or(u16::MAX),
            ignored: false,
        }
    }

    fn file(path: &str, kind: FileKind, size: u64) -> Node {
        Node {
            kind,
            size,
            ..dir(path)
        }
    }

    fn rust() -> FileKind {
        FileKind::Source {
            lang: "rust".into(),
        }
    }

    fn project(root: &str, kind: ProjectKind, name: &str) -> Project {
        Project {
            root: PathBuf::from(root),
            kind,
            name: name.into(),
            members: Vec::new(),
            manifest: None,
        }
    }

    fn fixture() -> ExplorerIndex {
        let mut nodes = vec![
            file("Cargo.toml", FileKind::Config, 300),
            file("README.md", FileKind::Markdown, 1200),
            dir("docs"),
            file("docs/guide.pdf", FileKind::Pdf, 3 * 1024 * 1024),
            dir("harw-core"),
            file("harw-core/Cargo.toml", FileKind::Config, 200),
            dir("harw-core/src"),
            file("harw-core/src/lib.rs", rust(), 3277),
            file("harw-core/src/main.rs", rust(), 10),
            dir("harw-core/src/nested"),
            file("harw-core/src/nested/deep.rs", rust(), 5),
            dir("harw-core-extra"),
            file("harw-core-extra/lib.rs", rust(), 1),
        ];
        nodes.sort_by(|a, b| a.path.cmp(&b.path));
        ExplorerIndex {
            root: PathBuf::from("/abs/root"),
            nodes,
            projects: vec![
                project("", ProjectKind::CargoWorkspace, "ws"),
                project("", ProjectKind::Git, "root"),
                project("harw-core", ProjectKind::CargoCrate, "harw-core"),
            ],
            relations: Vec::new(),
            truncated: false,
        }
    }

    fn paths(nodes: &[&Node]) -> Vec<String> {
        nodes.iter().map(|n| slash_path(&n.path)).collect()
    }

    #[test]
    fn children_dirs_first_then_name() {
        let index = fixture();
        assert_eq!(
            paths(&index.children(Path::new(""))),
            [
                "docs",
                "harw-core",
                "harw-core-extra",
                "Cargo.toml",
                "README.md"
            ]
        );
        assert_eq!(
            paths(&index.children(Path::new("harw-core/src"))),
            [
                "harw-core/src/nested",
                "harw-core/src/lib.rs",
                "harw-core/src/main.rs"
            ]
        );
        // Kein Überlaufen in `harw-core-extra`.
        assert_eq!(
            paths(&index.children(Path::new("harw-core"))),
            ["harw-core/src", "harw-core/Cargo.toml"]
        );
        assert_eq!(
            paths(&index.children(Path::new("/abs/root/docs"))),
            ["docs/guide.pdf"]
        );
        assert!(index.children(Path::new("missing")).is_empty());
        assert!(index.children(Path::new("README.md")).is_empty());
    }

    #[test]
    fn tree_renders_badges_sizes_and_limits() {
        let index = fixture();
        let tree = index.tree(Path::new("harw-core"), 5, 100);
        assert_eq!(
            tree,
            "src/\n  nested/\n    deep.rs  (rust, 5 B)\n  lib.rs  (rust, 3.2 KiB)\n  main.rs  (rust, 10 B)\nCargo.toml  (config, 200 B)"
        );

        let top = index.tree(Path::new(""), 1, 100);
        assert!(
            top.starts_with("docs/\nharw-core/  [cargo-crate harw-core]\n"),
            "{top}"
        );
        assert!(!top.contains("guide.pdf"));

        let cut = index.tree(Path::new(""), 10, 3);
        assert_eq!(cut.lines().count(), 4, "{cut}");
        assert!(cut.ends_with("… (10 weitere)"), "{cut}");

        assert_eq!(index.tree(Path::new("nope"), 3, 10), "(nicht gefunden)");
        assert_eq!(
            index.tree(Path::new("docs/guide.pdf"), 3, 10),
            "guide.pdf  (pdf, 3.0 MiB)"
        );
    }

    #[test]
    fn find_substring_and_glob() {
        let index = fixture();
        let hits = index.find("LIB.RS", 10);
        assert_eq!(
            paths(&hits),
            ["harw-core/src/lib.rs", "harw-core-extra/lib.rs"]
        );

        let hits = index.find("core", 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(paths(&hits)[0], "harw-core");

        let hits = index.find("*.rs", 10);
        assert_eq!(hits.len(), 4);
        let hits = index.find("harw-core/src/*.rs", 10);
        assert_eq!(hits.len(), 3, "{:?}", paths(&hits));
        let hits = index.find("cargo.tom?", 10);
        assert_eq!(paths(&hits), ["Cargo.toml", "harw-core/Cargo.toml"]);

        assert!(index.find("", 10).is_empty());
        assert!(index.find("zzz", 10).is_empty());
    }

    #[test]
    fn project_at_picks_innermost() {
        let index = fixture();
        let at = |p: &str| index.project_at(Path::new(p)).map(|p| p.name.clone());
        assert_eq!(at("harw-core/src/lib.rs").as_deref(), Some("harw-core"));
        assert_eq!(at("harw-core").as_deref(), Some("harw-core"));
        assert_eq!(at("harw-core-extra/lib.rs").as_deref(), Some("ws"));
        assert_eq!(at("").as_deref(), Some("ws"));
        assert_eq!(at("/abs/root/harw-core/src").as_deref(), Some("harw-core"));

        let empty = ExplorerIndex {
            projects: Vec::new(),
            ..fixture()
        };
        assert!(empty.project_at(Path::new("docs")).is_none());
    }

    #[test]
    fn summary_is_deterministic() {
        let mut index = fixture();
        assert_eq!(
            index.summary(),
            "13 Einträge (dir 5, rust 4, config 2, markdown 1, pdf 1) · 3 Projekte (cargo-crate 1, cargo-workspace 1, git 1)"
        );
        index.truncated = true;
        assert!(index.summary().ends_with(" (gekürzt)"));
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024), "5.0 GiB");
    }

    #[test]
    fn build_smoke() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        std::fs::create_dir_all(tmp.path().join("crate/src"))?;
        std::fs::write(
            tmp.path().join("crate/Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )?;
        std::fs::write(tmp.path().join("crate/src/lib.rs"), "pub fn f() {}\n")?;
        std::fs::write(tmp.path().join("README.md"), "# Hallo\n")?;

        let index = ExplorerIndex::build(tmp.path(), &ExplorerOptions::default())?;
        assert_eq!(index.root, std::fs::canonicalize(tmp.path())?);
        assert!(index.nodes.windows(2).all(|w| w[0].path <= w[1].path));
        assert!(index.nodes.iter().all(|n| n.path.is_relative()));
        assert!(
            index
                .find("lib.rs", 5)
                .iter()
                .any(|n| n.path == Path::new("crate/src/lib.rs"))
        );
        let crate_project = index.project_at(Path::new("crate/src/lib.rs"));
        assert_eq!(crate_project.map(|p| p.name.as_str()), Some("demo"));
        assert!(
            index.summary().contains("cargo-crate 1"),
            "{}",
            index.summary()
        );
        let _ = index.tree(Path::new(""), 4, 50);

        let missing =
            ExplorerIndex::build(&tmp.path().join("missing"), &ExplorerOptions::default());
        assert!(matches!(missing, Err(ExplorerError::InvalidRoot(_))));
        let not_dir =
            ExplorerIndex::build(&tmp.path().join("README.md"), &ExplorerOptions::default());
        assert!(matches!(not_dir, Err(ExplorerError::InvalidRoot(_))));
        Ok(())
    }
}
