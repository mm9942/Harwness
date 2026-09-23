//! Projekterkennung über den gesamten Baum.
//!
//! [`detect_projects`] ist an **kein** Ökosystem gebunden: jedes Verzeichnis
//! (inklusive der Explorer-Wurzel selbst) wird unabhängig auf bekannte
//! Manifeste geprüft, sodass beliebig viele, beliebig verschachtelte Projekte
//! erkannt werden (Cargo-Workspace im Git-Repo neben einem npm-Monorepo …).
//!
//! Erkannt werden:
//! - Cargo: `Cargo.toml` mit `[workspace]` (Mitglieder per Glob aufgelöst,
//!   abzüglich `workspace.exclude`) und/oder `[package]`,
//! - Node: `package.json` (npm/yarn-`workspaces`, `pnpm-workspace.yaml`),
//! - Python: `pyproject.toml`, `setup.cfg`, `setup.py`,
//! - Go: `go.mod`,
//! - Git: Verzeichnisse mit `.git` (direkt im Dateisystem geprüft, da der
//!   Walk `.git` nie auflistet),
//! - Dokumentsammlungen: Ordner, deren direkte Dateien überwiegend PDF oder
//!   Markdown sind.
//!
//! Fehlerhafte Manifeste führen nie zu einem Fehler: sie werden per
//! `tracing::debug` protokolliert, der Name fällt auf den Ordnernamen zurück.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::{FileKind, Node, Project, ProjectKind};

/// Mindestanzahl direkter Dateien für eine Dokumentsammlung.
const DOCUMENTS_MIN_FILES: usize = 3;
/// Mindestanteil (in Prozent) an PDF/Markdown unter den direkten Dateien.
const DOCUMENTS_MIN_PERCENT: usize = 60;

/// Erkennt alle Projekte unterhalb von `root` anhand der vom Walk gelieferten
/// `nodes` (Pfade relativ zu `root`).
///
/// Geprüft wird jedes Verzeichnis aus `nodes` sowie die Wurzel selbst (leerer
/// Pfad). Ergebnis ist nach `(root, kind.label())` sortiert und dedupliziert.
#[must_use]
pub fn detect_projects(root: &Path, nodes: &[Node]) -> Vec<Project> {
    let tree = Tree::new(nodes);
    let mut projects = Vec::new();

    for dir in &tree.dirs {
        detect_manifest_projects(root, dir, &tree, &mut projects);
        if root.join(dir).join(".git").exists() {
            projects.push(Project {
                root: dir.clone(),
                kind: ProjectKind::Git,
                name: dir_name(root, dir),
                members: Vec::new(),
                manifest: None,
            });
        }
    }

    let project_roots: BTreeSet<PathBuf> = projects
        .iter()
        .map(|project| project.root.clone())
        .collect();
    for dir in &tree.dirs {
        if !project_roots.contains(dir) && tree.is_document_folder(dir) {
            projects.push(Project {
                root: dir.clone(),
                kind: ProjectKind::Documents,
                name: dir_name(root, dir),
                members: Vec::new(),
                manifest: None,
            });
        }
    }

    projects.sort_by(|a, b| {
        a.root
            .cmp(&b.root)
            .then_with(|| a.kind.label().cmp(b.kind.label()))
    });
    projects.dedup_by(|a, b| a.root == b.root && a.kind == b.kind);
    projects
}

/// Aus `nodes` abgeleitete Sicht: Verzeichnisse und ihre direkten Dateien.
struct Tree<'a> {
    /// Alle Verzeichnisse inklusive der Wurzel (leerer Pfad).
    dirs: BTreeSet<PathBuf>,
    /// Verzeichnis → direkte Dateien (Name, Art).
    files: BTreeMap<PathBuf, Vec<(String, &'a FileKind)>>,
}

impl<'a> Tree<'a> {
    fn new(nodes: &'a [Node]) -> Self {
        let mut dirs = BTreeSet::new();
        dirs.insert(PathBuf::new());
        let mut files: BTreeMap<PathBuf, Vec<(String, &'a FileKind)>> = BTreeMap::new();
        for node in nodes {
            if node.kind == FileKind::Dir {
                dirs.insert(node.path.clone());
            } else if let Some(name) = node.path.file_name() {
                let parent = node
                    .path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                files
                    .entry(parent)
                    .or_default()
                    .push((name.to_string_lossy().into_owned(), &node.kind));
            }
        }
        Self { dirs, files }
    }

    /// `true`, wenn `dir` eine direkte Datei namens `name` enthält.
    fn has_file(&self, dir: &Path, name: &str) -> bool {
        self.files
            .get(dir)
            .is_some_and(|entries| entries.iter().any(|(file, _)| file == name))
    }

    /// Verzeichnisse, die eine direkte Datei `name` enthalten.
    fn dirs_with_file(&self, name: &str) -> BTreeSet<PathBuf> {
        self.dirs
            .iter()
            .filter(|dir| self.has_file(dir, name))
            .cloned()
            .collect()
    }

    fn is_document_folder(&self, dir: &Path) -> bool {
        let Some(entries) = self.files.get(dir) else {
            return false;
        };
        let total = entries.len();
        let documents = entries
            .iter()
            .filter(|(_, kind)| matches!(kind, FileKind::Pdf | FileKind::Markdown))
            .count();
        total >= DOCUMENTS_MIN_FILES && documents * 100 >= total * DOCUMENTS_MIN_PERCENT
    }
}

/// Prüft ein Verzeichnis auf alle manifestbasierten Projektarten.
fn detect_manifest_projects(root: &Path, dir: &Path, tree: &Tree<'_>, out: &mut Vec<Project>) {
    if tree.has_file(dir, "Cargo.toml") {
        detect_cargo(root, dir, tree, out);
    }
    if tree.has_file(dir, "package.json") {
        out.push(detect_node(root, dir, tree));
    }
    if let Some(project) = detect_python(root, dir, tree) {
        out.push(project);
    }
    if tree.has_file(dir, "go.mod") {
        let manifest = dir.join("go.mod");
        let name = read_manifest(root, &manifest)
            .and_then(|text| {
                text.lines().find_map(|line| {
                    let rest = line.trim().strip_prefix("module")?;
                    let module = rest.trim().trim_matches('"');
                    (rest.starts_with(char::is_whitespace) && !module.is_empty())
                        .then(|| module.to_owned())
                })
            })
            .unwrap_or_else(|| dir_name(root, dir));
        out.push(Project {
            root: dir.to_path_buf(),
            kind: ProjectKind::Go,
            name,
            members: Vec::new(),
            manifest: Some(manifest),
        });
    }
}

fn detect_cargo(root: &Path, dir: &Path, tree: &Tree<'_>, out: &mut Vec<Project>) {
    let manifest = dir.join("Cargo.toml");
    let Some(table) = read_toml(root, &manifest) else {
        // Unlesbar: als Crate mit Ordnernamen führen, damit das Projekt sichtbar bleibt.
        out.push(simple_project(
            root,
            dir,
            ProjectKind::CargoCrate,
            None,
            manifest,
        ));
        return;
    };

    if let Some(workspace) = table.get("workspace").and_then(toml::Value::as_table) {
        let members = string_list(workspace.get("members"));
        let exclude = string_list(workspace.get("exclude"));
        let candidates = tree.dirs_with_file("Cargo.toml");
        let resolved = resolve_members(dir, &members, &exclude, &candidates);
        out.push(Project {
            root: dir.to_path_buf(),
            kind: ProjectKind::CargoWorkspace,
            name: dir_name(root, dir),
            members: resolved,
            manifest: Some(manifest.clone()),
        });
    }

    if let Some(package) = table.get("package").and_then(toml::Value::as_table) {
        let name = package
            .get("name")
            .and_then(toml::Value::as_str)
            .map(str::to_owned);
        out.push(simple_project(
            root,
            dir,
            ProjectKind::CargoCrate,
            name,
            manifest,
        ));
    }
}

fn detect_node(root: &Path, dir: &Path, tree: &Tree<'_>) -> Project {
    let manifest = dir.join("package.json");
    let json = read_manifest(root, &manifest).and_then(|text| {
        serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|error| tracing::debug!(path = %manifest.display(), %error, "package.json nicht lesbar"))
            .ok()
    });

    let name = json
        .as_ref()
        .and_then(|value| value.get("name"))
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);

    let mut patterns: Vec<String> = Vec::new();
    if let Some(workspaces) = json.as_ref().and_then(|value| value.get("workspaces")) {
        let list = workspaces.as_array().or_else(|| {
            workspaces
                .get("packages")
                .and_then(serde_json::Value::as_array)
        });
        if let Some(list) = list {
            patterns.extend(
                list.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned),
            );
        }
    }
    if tree.has_file(dir, "pnpm-workspace.yaml") {
        if let Some(text) = read_manifest(root, &dir.join("pnpm-workspace.yaml")) {
            patterns.extend(parse_pnpm_packages(&text));
        }
    }

    let (exclude, include): (Vec<String>, Vec<String>) =
        patterns.into_iter().partition(|p| p.starts_with('!'));
    let exclude: Vec<String> = exclude
        .iter()
        .map(|p| p.trim_start_matches('!').to_owned())
        .collect();
    let members = if include.is_empty() {
        Vec::new()
    } else {
        resolve_members(
            dir,
            &include,
            &exclude,
            &tree.dirs_with_file("package.json"),
        )
    };

    Project {
        root: dir.to_path_buf(),
        kind: ProjectKind::Node,
        name: name.unwrap_or_else(|| dir_name(root, dir)),
        members,
        manifest: Some(manifest),
    }
}

fn detect_python(root: &Path, dir: &Path, tree: &Tree<'_>) -> Option<Project> {
    if tree.has_file(dir, "pyproject.toml") {
        let manifest = dir.join("pyproject.toml");
        let name = read_toml(root, &manifest).and_then(|table| {
            let project = table.get("project").and_then(|p| p.get("name"));
            let poetry = table
                .get("tool")
                .and_then(|tool| tool.get("poetry"))
                .and_then(|poetry| poetry.get("name"));
            project
                .or(poetry)
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
        });
        return Some(simple_project(
            root,
            dir,
            ProjectKind::Python,
            name,
            manifest,
        ));
    }
    if tree.has_file(dir, "setup.cfg") {
        let manifest = dir.join("setup.cfg");
        let name = read_manifest(root, &manifest).and_then(|text| parse_setup_cfg_name(&text));
        return Some(simple_project(
            root,
            dir,
            ProjectKind::Python,
            name,
            manifest,
        ));
    }
    if tree.has_file(dir, "setup.py") {
        return Some(simple_project(
            root,
            dir,
            ProjectKind::Python,
            None,
            dir.join("setup.py"),
        ));
    }
    None
}

fn simple_project(
    root: &Path,
    dir: &Path,
    kind: ProjectKind,
    name: Option<String>,
    manifest: PathBuf,
) -> Project {
    Project {
        root: dir.to_path_buf(),
        kind,
        name: name
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| dir_name(root, dir)),
        members: Vec::new(),
        manifest: Some(manifest),
    }
}

/// Liest `name` aus dem Abschnitt `[metadata]` einer `setup.cfg`.
fn parse_setup_cfg_name(text: &str) -> Option<String> {
    let mut in_metadata = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_metadata = line == "[metadata]";
            continue;
        }
        if !in_metadata {
            continue;
        }
        if let Some((key, value)) = line.split_once(['=', ':']) {
            if key.trim() == "name" {
                let value = value.trim();
                return (!value.is_empty()).then(|| value.to_owned());
            }
        }
    }
    None
}

/// Liest die einfache Listenform `packages:` aus einer `pnpm-workspace.yaml`
/// (`  - 'packages/*'`); andere Schlüssel beenden die Liste.
fn parse_pnpm_packages(text: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    let mut in_packages = false;
    for raw in text.lines() {
        let line = raw.split(" #").next().unwrap_or_default().trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !raw.starts_with([' ', '\t', '-']) {
            in_packages = trimmed == "packages:";
            continue;
        }
        if in_packages {
            if let Some(item) = trimmed.strip_prefix('-') {
                let item = item.trim().trim_matches(|c| c == '\'' || c == '"');
                if !item.is_empty() {
                    patterns.push(item.to_owned());
                }
            }
        }
    }
    patterns
}

/// Löst Workspace-Muster (relativ zu `base`) gegen `candidates` (relativ zur
/// Explorer-Wurzel) auf. Ausschlüsse treffen das Verzeichnis selbst und alles
/// darunter.
fn resolve_members(
    base: &Path,
    include: &[String],
    exclude: &[String],
    candidates: &BTreeSet<PathBuf>,
) -> Vec<PathBuf> {
    let Some(include) = build_globset(base, include) else {
        return Vec::new();
    };
    let exclude = build_globset(base, exclude);
    candidates
        .iter()
        .filter(|candidate| {
            let key = path_key(candidate);
            if !include.is_match(&key) {
                return false;
            }
            let excluded = exclude.as_ref().is_some_and(|set| {
                candidate
                    .ancestors()
                    .any(|ancestor| set.is_match(path_key(ancestor)))
            });
            !excluded
        })
        .cloned()
        .collect()
}

/// Baut ein `GlobSet` aus Mustern relativ zu `base`; `None`, wenn kein Muster
/// gültig ist.
fn build_globset(base: &Path, patterns: &[String]) -> Option<GlobSet> {
    let prefix = path_key(base);
    let mut builder = GlobSetBuilder::new();
    let mut any = false;
    for pattern in patterns {
        let normalized = normalize_pattern(pattern);
        // Führende `..` gegen den Basis-Pfad verrechnen.
        let mut base_parts: Vec<&str> = prefix.split('/').filter(|part| !part.is_empty()).collect();
        let mut rest = normalized.as_str();
        while let Some(stripped) = rest.strip_prefix("..") {
            if !(stripped.is_empty() || stripped.starts_with('/')) || base_parts.pop().is_none() {
                break;
            }
            rest = stripped.trim_start_matches('/');
        }
        if rest.starts_with("..") {
            tracing::debug!(%pattern, "Workspace-Muster verlässt die Explorer-Wurzel");
            continue;
        }
        let joined = base_parts
            .iter()
            .map(|part| globset::escape(part))
            .chain((!rest.is_empty()).then(|| rest.to_owned()))
            .collect::<Vec<_>>()
            .join("/");
        match GlobBuilder::new(&joined).literal_separator(true).build() {
            Ok(glob) => {
                builder.add(glob);
                any = true;
            }
            Err(error) => tracing::debug!(%pattern, %error, "ungültiges Workspace-Muster"),
        }
    }
    if !any {
        return None;
    }
    builder
        .build()
        .map_err(|error| tracing::debug!(%error, "Workspace-Muster nicht kompilierbar"))
        .ok()
}

/// Normalisiert ein Muster lexikalisch: `./`, doppelte und abschließende `/`
/// entfallen, `a/../b` wird zu `b`. Führende `..` bleiben erhalten.
fn normalize_pattern(pattern: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let unified = pattern.replace('\\', "/");
    for part in unified.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Pfad als `/`-getrennter Schlüssel für das Glob-Matching.
fn path_key(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn string_list(value: Option<&toml::Value>) -> Vec<String> {
    value
        .and_then(toml::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn read_manifest(root: &Path, manifest: &Path) -> Option<String> {
    std::fs::read_to_string(root.join(manifest))
        .map_err(
            |error| tracing::debug!(path = %manifest.display(), %error, "Manifest nicht lesbar"),
        )
        .ok()
}

fn read_toml(root: &Path, manifest: &Path) -> Option<toml::Table> {
    let text = read_manifest(root, manifest)?;
    toml::from_str::<toml::Table>(&text)
        .map_err(|error| tracing::debug!(path = %manifest.display(), %error, "TOML-Manifest nicht lesbar"))
        .ok()
}

/// Ordnername von `dir`; für die Wurzel selbst der Name der Explorer-Wurzel.
fn dir_name(root: &Path, dir: &Path) -> String {
    dir.file_name()
        .or_else(|| root.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Legt Dateien an und baut die passende Node-Liste von Hand.
    fn fixture(
        files: &[(&str, &str)],
    ) -> Result<(tempfile::TempDir, Vec<Node>), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let mut dirs = BTreeSet::new();
        let mut nodes = Vec::new();
        for (path, content) in files {
            let rel = PathBuf::from(path);
            let abs = dir.path().join(&rel);
            if let Some(parent) = abs.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&abs, content)?;
            for ancestor in rel.ancestors().skip(1) {
                if !ancestor.as_os_str().is_empty() {
                    dirs.insert(ancestor.to_path_buf());
                }
            }
            let kind = match rel.extension().and_then(|e| e.to_str()) {
                Some("md") => FileKind::Markdown,
                Some("pdf") => FileKind::Pdf,
                Some("toml" | "json" | "yaml" | "mod" | "cfg") => FileKind::Config,
                Some("rs") => FileKind::Source {
                    lang: "rust".into(),
                },
                Some("py") => FileKind::Source {
                    lang: "python".into(),
                },
                _ => FileKind::Text,
            };
            nodes.push(node(rel, kind));
        }
        nodes.extend(dirs.into_iter().map(|d| node(d, FileKind::Dir)));
        nodes.sort_by(|a, b| a.path.cmp(&b.path));
        Ok((dir, nodes))
    }

    fn node(path: PathBuf, kind: FileKind) -> Node {
        let depth = u16::try_from(path.components().count()).unwrap_or(u16::MAX);
        Node {
            path,
            kind,
            size: 0,
            depth,
            ignored: false,
        }
    }

    fn find<'a>(projects: &'a [Project], root: &str, kind: ProjectKind) -> Option<&'a Project> {
        projects
            .iter()
            .find(|p| p.root == Path::new(root) && p.kind == kind)
    }

    fn paths(items: &[&str]) -> Vec<PathBuf> {
        items.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn single_crate_at_root() -> TestResult {
        let (dir, nodes) = fixture(&[
            (
                "Cargo.toml",
                "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n",
            ),
            ("src/main.rs", "fn main() {}"),
        ])?;
        let projects = detect_projects(dir.path(), &nodes);
        assert_eq!(projects.len(), 1);
        let krate = find(&projects, "", ProjectKind::CargoCrate).ok_or("crate fehlt")?;
        assert_eq!(krate.name, "solo");
        assert_eq!(krate.manifest.as_deref(), Some(Path::new("Cargo.toml")));
        Ok(())
    }

    #[test]
    fn workspace_glob_with_exclude_and_root_package() -> TestResult {
        let (dir, nodes) = fixture(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\", \"tools/**\", \"libs/b-*\", \"app\"]\nexclude = [\"crates/skip\"]\n\n[package]\nname = \"root-pkg\"\n",
            ),
            ("crates/a/Cargo.toml", "[package]\nname = \"a\"\n"),
            ("crates/skip/Cargo.toml", "[package]\nname = \"skip\"\n"),
            ("crates/nomanifest/README.md", "x"),
            ("crates/a/deep/Cargo.toml", "[package]\nname = \"deep\"\n"),
            ("tools/x/y/Cargo.toml", "[package]\nname = \"y\"\n"),
            ("libs/b-one/Cargo.toml", "[package]\nname = \"b-one\"\n"),
            ("libs/c-two/Cargo.toml", "[package]\nname = \"c-two\"\n"),
            ("app/Cargo.toml", "[package]\nname = \"app\"\n"),
        ])?;
        let projects = detect_projects(dir.path(), &nodes);
        let ws = find(&projects, "", ProjectKind::CargoWorkspace).ok_or("workspace fehlt")?;
        assert_eq!(
            ws.members,
            paths(&["app", "crates/a", "libs/b-one", "tools/x/y"])
        );
        let root_crate = find(&projects, "", ProjectKind::CargoCrate).ok_or("root crate fehlt")?;
        assert_eq!(root_crate.name, "root-pkg");
        // Jede Cargo.toml mit [package] ist ein Crate, auch außerhalb der Mitglieder.
        assert!(find(&projects, "crates/skip", ProjectKind::CargoCrate).is_some());
        assert!(find(&projects, "libs/c-two", ProjectKind::CargoCrate).is_some());
        // Sortierung: Wurzel zuerst, cargo-crate vor cargo-workspace.
        assert_eq!(projects[0].kind, ProjectKind::CargoCrate);
        assert_eq!(projects[1].kind, ProjectKind::CargoWorkspace);
        Ok(())
    }

    #[test]
    fn nested_workspace_inside_workspace() -> TestResult {
        let (dir, nodes) = fixture(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"core\"]\nexclude = [\"vendor\"]\n",
            ),
            ("core/Cargo.toml", "[package]\nname = \"core\"\n"),
            (
                "vendor/inner/Cargo.toml",
                "[workspace]\nmembers = [\"parts/*\", \"../../core\"]\n",
            ),
            (
                "vendor/inner/parts/p1/Cargo.toml",
                "[package]\nname = \"p1\"\n",
            ),
            (
                "vendor/inner/parts/p2/Cargo.toml",
                "[package]\nname = \"p2\"\n",
            ),
        ])?;
        let projects = detect_projects(dir.path(), &nodes);
        let outer = find(&projects, "", ProjectKind::CargoWorkspace).ok_or("outer fehlt")?;
        assert_eq!(outer.members, paths(&["core"]));
        let inner =
            find(&projects, "vendor/inner", ProjectKind::CargoWorkspace).ok_or("inner fehlt")?;
        assert_eq!(inner.name, "inner");
        assert_eq!(
            inner.members,
            paths(&["core", "vendor/inner/parts/p1", "vendor/inner/parts/p2"])
        );
        Ok(())
    }

    #[test]
    fn npm_workspaces_array_and_object() -> TestResult {
        let (dir, nodes) = fixture(&[
            (
                "package.json",
                r#"{"name":"mono","workspaces":["packages/*","!packages/private"]}"#,
            ),
            ("packages/ui/package.json", r#"{"name":"@mono/ui"}"#),
            ("packages/private/package.json", r#"{"name":"secret"}"#),
            ("packages/empty/index.js", ""),
            (
                "other/package.json",
                r#"{"workspaces":{"packages":["mods/*"]}}"#,
            ),
            ("other/mods/m1/package.json", "{ kaputt"),
        ])?;
        let projects = detect_projects(dir.path(), &nodes);
        let mono = find(&projects, "", ProjectKind::Node).ok_or("mono fehlt")?;
        assert_eq!(mono.name, "mono");
        assert_eq!(mono.members, paths(&["packages/ui"]));
        let other = find(&projects, "other", ProjectKind::Node).ok_or("other fehlt")?;
        assert_eq!(other.name, "other");
        assert_eq!(other.members, paths(&["other/mods/m1"]));
        // Kaputtes Manifest: Ordnername, kein Fehler.
        assert_eq!(
            find(&projects, "other/mods/m1", ProjectKind::Node)
                .ok_or("m1 fehlt")?
                .name,
            "m1"
        );
        assert_eq!(
            find(&projects, "packages/ui", ProjectKind::Node)
                .ok_or("ui fehlt")?
                .name,
            "@mono/ui"
        );
        Ok(())
    }

    #[test]
    fn pnpm_workspace_yaml() -> TestResult {
        let (dir, nodes) = fixture(&[
            ("web/package.json", r#"{"name":"web-root"}"#),
            (
                "web/pnpm-workspace.yaml",
                "# comment\npackages:\n  - 'apps/*'\n  - \"libs/**\"\n  - '!libs/legacy'\ncatalog:\n  - 'ignored/*'\n",
            ),
            ("web/apps/site/package.json", "{}"),
            ("web/libs/a/b/package.json", "{}"),
            ("web/libs/legacy/package.json", "{}"),
            ("web/ignored/z/package.json", "{}"),
        ])?;
        let projects = detect_projects(dir.path(), &nodes);
        let web = find(&projects, "web", ProjectKind::Node).ok_or("web fehlt")?;
        assert_eq!(web.name, "web-root");
        assert_eq!(web.members, paths(&["web/apps/site", "web/libs/a/b"]));
        Ok(())
    }

    #[test]
    fn python_variants() -> TestResult {
        let (dir, nodes) = fixture(&[
            ("pep/pyproject.toml", "[project]\nname = \"pep-pkg\"\n"),
            (
                "poetry/pyproject.toml",
                "[tool.poetry]\nname = \"poetry-pkg\"\n",
            ),
            ("cfg/setup.cfg", "[metadata]\nname = cfg-pkg\nversion = 1\n"),
            ("legacy/setup.py", "from setuptools import setup\n"),
            ("broken/pyproject.toml", "[project\nname="),
        ])?;
        let projects = detect_projects(dir.path(), &nodes);
        let name = |root: &str| find(&projects, root, ProjectKind::Python).map(|p| p.name.clone());
        assert_eq!(name("pep").as_deref(), Some("pep-pkg"));
        assert_eq!(name("poetry").as_deref(), Some("poetry-pkg"));
        assert_eq!(name("cfg").as_deref(), Some("cfg-pkg"));
        assert_eq!(name("legacy").as_deref(), Some("legacy"));
        assert_eq!(name("broken").as_deref(), Some("broken"));
        Ok(())
    }

    #[test]
    fn go_module() -> TestResult {
        let (dir, nodes) = fixture(&[
            (
                "svc/go.mod",
                "// c\nmodule github.com/acme/svc\n\ngo 1.22\n",
            ),
            ("svc/main.go", "package main"),
        ])?;
        let projects = detect_projects(dir.path(), &nodes);
        let go = find(&projects, "svc", ProjectKind::Go).ok_or("go fehlt")?;
        assert_eq!(go.name, "github.com/acme/svc");
        assert_eq!(go.manifest.as_deref(), Some(Path::new("svc/go.mod")));
        Ok(())
    }

    #[test]
    fn git_repositories_at_root_and_nested() -> TestResult {
        let (dir, nodes) = fixture(&[("sub/readme.txt", "x"), ("plain/a.txt", "x")])?;
        fs::create_dir(dir.path().join(".git"))?;
        fs::create_dir(dir.path().join("sub/.git"))?;
        let projects = detect_projects(dir.path(), &nodes);
        let root_git = find(&projects, "", ProjectKind::Git).ok_or("root git fehlt")?;
        let expected = dir
            .path()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        assert_eq!(Some(root_git.name.clone()), expected);
        assert!(find(&projects, "sub", ProjectKind::Git).is_some());
        assert!(find(&projects, "plain", ProjectKind::Git).is_none());
        Ok(())
    }

    #[test]
    fn documents_folder() -> TestResult {
        let (dir, nodes) = fixture(&[
            ("docs/a.md", "#"),
            ("docs/b.pdf", "%PDF"),
            ("docs/c.md", "#"),
            ("docs/notes.txt", "x"),
            ("mixed/a.md", "#"),
            ("mixed/b.txt", ""),
            ("mixed/c.txt", ""),
            ("few/a.md", "#"),
            ("few/b.md", "#"),
            ("pkg/package.json", "{}"),
            ("pkg/a.md", "#"),
            ("pkg/b.md", "#"),
            ("pkg/c.md", "#"),
        ])?;
        let projects = detect_projects(dir.path(), &nodes);
        let docs = find(&projects, "docs", ProjectKind::Documents).ok_or("docs fehlt")?;
        assert_eq!(docs.name, "docs");
        assert!(find(&projects, "mixed", ProjectKind::Documents).is_none());
        assert!(find(&projects, "few", ProjectKind::Documents).is_none());
        // Bereits Projektwurzel → keine zusätzliche Dokumentsammlung.
        assert!(find(&projects, "pkg", ProjectKind::Documents).is_none());
        assert!(find(&projects, "pkg", ProjectKind::Node).is_some());
        Ok(())
    }

    #[test]
    fn empty_tree_yields_nothing() {
        let projects = detect_projects(Path::new("/nonexistent-explorer-root"), &[]);
        assert!(projects.is_empty());
    }
}
