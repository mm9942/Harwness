//! Durchlauf des Verzeichnisbaums ab einer Wurzel.
//!
//! Standardmäßig werden `.gitignore`, `.ignore`, `.git/info/exclude` und
//! versteckte Einträge beachtet (auch außerhalb eines Git-Repositorys).
//! `.git/` wird nie betreten und nie gelistet. Schwergewichtige
//! Artefakt-Ordner ([`HEAVY_DIRS`]) erscheinen selbst als Knoten, ihr Inhalt
//! wird aber nur mit [`ExplorerOptions::include_ignored`] durchlaufen.
//!
//! Mit `include_ignored` wird zusätzlich ungefiltert gelaufen; alle Einträge,
//! die der gefilterte Lauf nicht geliefert hätte, tragen [`Node::ignored`].

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use ignore::{DirEntry, WalkBuilder};

use crate::{ExplorerError, ExplorerOptions, ExplorerResult, FileKind, Node};

/// Ordner, die selbst gelistet, aber ohne `include_ignored` nicht betreten
/// werden (Build-Artefakte, Abhängigkeiten, virtuelle Umgebungen).
pub const HEAVY_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".venv",
    "__pycache__",
    "dist",
    "build",
];

/// Läuft ab `root` durch den Baum und liefert alle Einträge (ohne die Wurzel)
/// sortiert nach Pfad – Verzeichnisse stehen vor ihrem Inhalt – sowie ob
/// [`ExplorerOptions::max_nodes`] erreicht wurde.
///
/// # Errors
/// [`ExplorerError::InvalidRoot`], wenn `root` kein Verzeichnis ist.
pub fn walk(root: &Path, opts: &ExplorerOptions) -> ExplorerResult<(Vec<Node>, bool)> {
    if !root.is_dir() {
        return Err(ExplorerError::InvalidRoot(root.to_path_buf()));
    }

    let (mut nodes, truncated) = if opts.include_ignored {
        // Beide Läufe sind nach Dateinamen sortiert; der ungefilterte ist eine
        // Obermenge in gleicher Reihenfolge. Daher genügt es, vom gefilterten
        // Lauf höchstens `max_nodes` Pfade zu sammeln.
        let mut visible = HashSet::new();
        run(root, opts, true, |rel, _| {
            visible.insert(rel.to_path_buf());
            visible.len() < opts.max_nodes
        });
        collect(root, opts, false, |rel| !visible.contains(rel))
    } else {
        collect(root, opts, true, |_| false)
    };

    nodes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((nodes, truncated))
}

/// Führt einen Lauf aus und baut die Knoten; `is_ignored` entscheidet über
/// [`Node::ignored`].
fn collect(
    root: &Path,
    opts: &ExplorerOptions,
    filtered: bool,
    is_ignored: impl Fn(&Path) -> bool,
) -> (Vec<Node>, bool) {
    let mut nodes = Vec::new();
    let mut truncated = false;
    if opts.max_nodes == 0 {
        return (nodes, has_any_entry(root));
    }
    run(root, opts, filtered, |rel, entry| {
        nodes.push(make_node(rel, entry, is_ignored(rel)));
        if nodes.len() >= opts.max_nodes {
            truncated = true;
            return false;
        }
        true
    });
    // Genau `max_nodes` Einträge ohne weitere wären kein echter Abbruch –
    // der Einfachheit halber gilt Erreichen der Grenze als abgeschnitten.
    (nodes, truncated)
}

/// Prüft, ob die Wurzel überhaupt Einträge hat (für `max_nodes == 0`).
fn has_any_entry(root: &Path) -> bool {
    std::fs::read_dir(root).is_ok_and(|mut entries| entries.next().is_some())
}

/// Kern-Durchlauf: ruft `visit` für jeden Eintrag (ohne Wurzel) mit relativem
/// Pfad auf; liefert `visit` `false`, wird abgebrochen.
fn run(
    root: &Path,
    opts: &ExplorerOptions,
    filtered: bool,
    mut visit: impl FnMut(&Path, &DirEntry) -> bool,
) {
    let filter_root = root.to_path_buf();
    let mut builder = WalkBuilder::new(root);
    builder
        .standard_filters(filtered)
        .require_git(false)
        .follow_links(false)
        .max_depth(Some(opts.max_depth))
        .sort_by_file_name(std::cmp::Ord::cmp)
        .filter_entry(move |entry| keep_entry(&filter_root, entry, filtered));

    for result in builder.build() {
        let entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                tracing::debug!(%error, "explorer: eintrag nicht lesbar, übersprungen");
                continue;
            }
        };
        if entry.depth() == 0 {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            tracing::debug!(path = %entry.path().display(), "explorer: eintrag außerhalb der wurzel");
            continue;
        };
        if !visit(rel, &entry) {
            break;
        }
    }
}

/// Filter für den Walker: `.git` nie; im gefilterten Lauf nichts unterhalb
/// eines [`HEAVY_DIRS`]-Ordners.
fn keep_entry(root: &Path, entry: &DirEntry, filtered: bool) -> bool {
    if entry.depth() > 0 && entry.file_name() == ".git" {
        return false;
    }
    if !filtered {
        return true;
    }
    let Ok(rel) = entry.path().strip_prefix(root) else {
        return true;
    };
    !inside_heavy_dir(rel)
}

/// `true`, wenn ein Vorfahre (nicht der Eintrag selbst) ein Heavy-Ordner ist.
fn inside_heavy_dir(rel: &Path) -> bool {
    let components: Vec<Component<'_>> = rel.components().collect();
    let ancestors = components.len().saturating_sub(1);
    components
        .iter()
        .take(ancestors)
        .any(|component| match component {
            Component::Normal(name) => name.to_str().is_some_and(|name| HEAVY_DIRS.contains(&name)),
            _ => false,
        })
}

fn make_node(rel: &Path, entry: &DirEntry, ignored: bool) -> Node {
    let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
    let (kind, size) = if is_dir {
        (FileKind::Dir, 0)
    } else {
        let size = match entry.metadata() {
            Ok(meta) => meta.len(),
            Err(error) => {
                tracing::debug!(%error, path = %rel.display(), "explorer: metadaten nicht lesbar");
                0
            }
        };
        (crate::filetype::classify_path(entry.path()), size)
    };
    let depth = u16::try_from(rel.components().count()).unwrap_or(u16::MAX);
    Node {
        path: PathBuf::from(rel),
        kind,
        size,
        depth,
        ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn paths(nodes: &[Node]) -> Vec<String> {
        nodes
            .iter()
            .map(|n| n.path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    fn find<'a>(nodes: &'a [Node], path: &str) -> Option<&'a Node> {
        nodes.iter().find(|n| n.path == Path::new(path))
    }

    #[test]
    fn invalid_root_is_error() -> TestResult {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("f.txt");
        fs::write(&file, "x")?;
        assert!(matches!(
            walk(&file, &ExplorerOptions::default()),
            Err(ExplorerError::InvalidRoot(_))
        ));
        assert!(matches!(
            walk(&dir.path().join("missing"), &ExplorerOptions::default()),
            Err(ExplorerError::InvalidRoot(_))
        ));
        Ok(())
    }

    #[test]
    fn gitignored_excluded_by_default_and_marked_when_included() -> TestResult {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        fs::write(root.join(".gitignore"), "secret.log\n")?;
        fs::write(root.join("secret.log"), "s")?;
        fs::create_dir(root.join("src"))?;
        fs::write(root.join("src/main.rs"), "fn main() {}\n")?;

        let (nodes, truncated) = walk(root, &ExplorerOptions::default())?;
        assert!(!truncated);
        assert_eq!(paths(&nodes), vec!["src", "src/main.rs"]);
        let main = find(&nodes, "src/main.rs").ok_or("main.rs fehlt")?;
        assert_eq!(main.depth, 2);
        assert_eq!(main.size, 13);
        assert!(!main.ignored);
        assert_eq!(find(&nodes, "src").ok_or("src fehlt")?.kind, FileKind::Dir);

        let opts = ExplorerOptions {
            include_ignored: true,
            ..ExplorerOptions::default()
        };
        let (nodes, _) = walk(root, &opts)?;
        let secret = find(&nodes, "secret.log").ok_or("secret.log fehlt")?;
        assert!(secret.ignored);
        // Versteckte Datei `.gitignore` ist nur mit include_ignored sichtbar.
        assert!(
            find(&nodes, ".gitignore")
                .ok_or(".gitignore fehlt")?
                .ignored
        );
        assert!(!find(&nodes, "src/main.rs").ok_or("main.rs fehlt")?.ignored);
        Ok(())
    }

    #[test]
    fn git_dir_never_listed() -> TestResult {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        fs::create_dir_all(root.join(".git/objects"))?;
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n")?;
        fs::write(root.join("a.txt"), "a")?;
        for include_ignored in [false, true] {
            let opts = ExplorerOptions {
                include_ignored,
                ..ExplorerOptions::default()
            };
            let (nodes, _) = walk(root, &opts)?;
            assert!(
                nodes.iter().all(|n| !n.path.starts_with(".git")),
                "{:?}",
                paths(&nodes)
            );
            assert!(find(&nodes, "a.txt").is_some());
        }
        Ok(())
    }

    #[test]
    fn max_nodes_truncates() -> TestResult {
        let dir = tempfile::tempdir()?;
        for i in 0..10 {
            fs::write(dir.path().join(format!("f{i}.txt")), "x")?;
        }
        let opts = ExplorerOptions {
            max_nodes: 3,
            ..ExplorerOptions::default()
        };
        let (nodes, truncated) = walk(dir.path(), &opts)?;
        assert!(truncated);
        assert_eq!(nodes.len(), 3);

        let opts = ExplorerOptions {
            max_nodes: 3,
            include_ignored: true,
            ..ExplorerOptions::default()
        };
        let (nodes, truncated) = walk(dir.path(), &opts)?;
        assert!(truncated);
        assert_eq!(nodes.len(), 3);
        assert!(nodes.iter().all(|n| !n.ignored));
        Ok(())
    }

    #[test]
    fn max_depth_limits() -> TestResult {
        let dir = tempfile::tempdir()?;
        fs::create_dir_all(dir.path().join("a/b/c"))?;
        let opts = ExplorerOptions {
            max_depth: 2,
            ..ExplorerOptions::default()
        };
        let (nodes, _) = walk(dir.path(), &opts)?;
        assert_eq!(paths(&nodes), vec!["a", "a/b"]);
        Ok(())
    }

    #[test]
    fn heavy_dir_listed_but_not_descended() -> TestResult {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        fs::create_dir_all(root.join("node_modules/pkg"))?;
        fs::write(root.join("node_modules/pkg/index.js"), "x")?;
        fs::write(root.join("package.json"), "{}")?;

        let (nodes, _) = walk(root, &ExplorerOptions::default())?;
        assert_eq!(paths(&nodes), vec!["node_modules", "package.json"]);
        assert_eq!(
            find(&nodes, "node_modules")
                .ok_or("node_modules fehlt")?
                .kind,
            FileKind::Dir
        );

        let opts = ExplorerOptions {
            include_ignored: true,
            ..ExplorerOptions::default()
        };
        let (nodes, _) = walk(root, &opts)?;
        assert!(
            !find(&nodes, "node_modules")
                .ok_or("node_modules fehlt")?
                .ignored
        );
        assert!(
            find(&nodes, "node_modules/pkg/index.js")
                .ok_or("index.js fehlt")?
                .ignored
        );
        Ok(())
    }

    #[test]
    fn sorted_dirs_before_contents() -> TestResult {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        fs::create_dir(root.join("a"))?;
        fs::write(root.join("a/z.txt"), "z")?;
        fs::write(root.join("a-b.txt"), "x")?;
        fs::write(root.join("0.txt"), "x")?;
        let (nodes, _) = walk(root, &ExplorerOptions::default())?;
        assert_eq!(paths(&nodes), vec!["0.txt", "a", "a/z.txt", "a-b.txt"]);
        Ok(())
    }
}
