//! Einlesen des eigenen Cargo-Workspace zu einem Abhängigkeitsgraphen.
//!
//! Liest ausschließlich TOML-Dateien (`Cargo.toml` der Workspace-Wurzel und
//! jedes Members); es wird kein `cargo`-Subprozess gestartet. Siehe AP
//! W1-14..17 für die vollständige Spezifikation von [`WorkspaceGraph`] und
//! [`CrateNode`].
//!
//! ## `[package].version`-Formen
//! Seit AW0-00 tragen alle Member-Manifeste `version.workspace = true`
//! (TOML-intern eine Tabelle `{ workspace = true }`) statt einer wörtlichen
//! Versions-Zeichenkette. [`RawVersion`] liest deshalb drei gültige Formen:
//! eine Zeichenkette (`version = "0.2.0"`), die Vererbung
//! (`version.workspace = true`) und das vollständige Fehlen des Feldes. Jede
//! vierte Form — etwa `{ workspace = false }` oder ein anderer Tabelleninhalt
//! — ist ein **Parsefehler**, kein stiller leerer Wert. Genau dieses stille
//! Übergehen war der eigentliche Fehler: `RawPackageSection::version` war als
//! `Option<String>` deklariert, `serde` konnte die Tabellenform gar nicht in
//! eine Zeichenkette einlesen und scheiterte deshalb an **jedem** der 95
//! Member-Manifeste — wodurch sämtliche Gates, die auf `WorkspaceGraph::load`
//! aufbauen, monatelang blind liefen, ohne dass ein grünes Ergebnis das
//! anzeigte.
//!
//! `dependencies`, `dev-dependencies` und `build-dependencies` sind vom
//! `version`-Deserialisierungsfehler **nicht** betroffen: sie sind als
//! `toml::Table` deklariert (siehe [`RawMemberManifest`]), `serde` erzwingt
//! dort also keine feste Form pro Wert, und ein Eintrag wie
//! `foo = { workspace = true }` scheitert nie beim Parsen.
//!
//! ## Klassifikation intern/extern (zweiter, unabhängiger Befund)
//! [`classify_dependencies`] entschied vor dieser Reparatur fälschlich
//! anhand des bloßen Vorhandenseins eines `path`-Feldes, ob eine
//! Abhängigkeit workspace-intern ist. Das ist falsch: ein `path`-Feld sagt
//! nur „lokal ausgecheckt", nicht „ist ein Member dieses Workspace" — z. B.
//! zeigt `harw-secrets`s Abhängigkeit `crypt_guard = { path =
//! "../../crypt_guard" }` auf ein Sibling-Repo außerhalb dieses Workspace.
//! Wurde ein solcher Name fälschlich als intern gezählt, tauchte er nie als
//! Knoten in `crates` auf, [`compute_levels`] konnte ihn nie aus der
//! Restmenge auflösen, und jeder transitiv abhängige Knoten (hier:
//! `harw-channel` → `harw-channel-telegram` →
//! `harw-channel-telegram-transport` → `harw-cli`) blieb mit ihm zusammen
//! unauflösbar — was `xtask gates` als `CycleDetected` meldete, obwohl kein
//! Zyklus vorlag, sondern eine Kante auf einen Nicht-Member. Maßgeblich für
//! „intern" ist jetzt ausschließlich die Mitgliedschaft in `member_names`;
//! zusätzlich prüft [`compute_levels`] vor dem eigentlichen Kahn-Lauf, ob
//! jede genannte interne Abhängigkeit als Knoten existiert, und meldet einen
//! fehlenden Namen explizit als [`CodeGraphError::MemberMissing`] statt ihn
//! stillschweigend in die Zyklus-Restmenge fallen zu lassen.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{CodeGraphError, CodeGraphResult};

/// Ein einzelnes Crate im Workspace samt seiner klassifizierten Abhängigkeiten.
#[derive(Debug, Clone)]
pub struct CrateNode {
    /// `[package].name` aus dem Member-Manifest.
    pub name: String,
    /// `[package].version` als Zeichenkette. `"0.0.0"`, falls das Feld fehlt
    /// oder per `version.workspace = true` aus der Workspace-Wurzel geerbt
    /// wird (die tatsächliche geerbte Versionsnummer wird hier bewusst nicht
    /// aufgelöst, siehe [`RawVersion`]).
    pub version: String,
    /// Absoluter Pfad zum `Cargo.toml` dieses Crates.
    pub manifest_path: PathBuf,
    /// Absolutes Verzeichnis dieses Crates.
    pub dir: PathBuf,
    /// Namen der workspace-internen `[dependencies]` (per Member-Name oder
    /// `path`-Feld erkannt).
    pub deps: Vec<String>,
    /// Alle Namen aus `[dev-dependencies]` (intern und extern gemischt).
    pub dev_deps: Vec<String>,
    /// Alle Namen aus `[build-dependencies]` (intern und extern gemischt).
    pub build_deps: Vec<String>,
    /// Namen der externen (nicht workspace-internen) `[dependencies]`, für
    /// spätere Recherche-Fragen an die Registry.
    pub external_deps: Vec<String>,
    /// `true`, wenn `deps` leer ist (keine internen normalen Abhängigkeiten).
    pub is_leaf: bool,
    /// Ebene von unten: `0` für Leaves, sonst `1 + max(level der internen deps)`.
    pub level: u32,
}

/// Der vollständige (oder ein per [`WorkspaceGraph::subgraph`] reduzierter)
/// Abhängigkeitsgraph eines Cargo-Workspace.
#[derive(Debug, Clone)]
pub struct WorkspaceGraph {
    /// Wurzelverzeichnis des Workspace (übergeben an [`WorkspaceGraph::load`]).
    pub root: PathBuf,
    /// Alle Crates dieses Graphen.
    pub crates: Vec<CrateNode>,
}

/// Rohform der Workspace-Wurzel (`[workspace]`-Abschnitt, `[package]` fehlt
/// bei einem rein virtuellen Workspace).
#[derive(Debug, Deserialize)]
struct RawRootManifest {
    #[serde(default)]
    workspace: Option<RawWorkspaceSection>,
}

#[derive(Debug, Deserialize)]
struct RawWorkspaceSection {
    #[serde(default)]
    members: Option<Vec<String>>,
}

/// Rohform eines Member-Manifests. `package` ist bewusst nicht `Option`:
/// fehlt der Abschnitt, ist das Manifest für den Graphen unbrauchbar und das
/// Parsen schlägt mit `CodeGraphError::Toml` fehl.
#[derive(Debug, Deserialize)]
struct RawMemberManifest {
    package: RawPackageSection,
    #[serde(default)]
    dependencies: toml::Table,
    #[serde(default, rename = "dev-dependencies")]
    dev_dependencies: toml::Table,
    #[serde(default, rename = "build-dependencies")]
    build_dependencies: toml::Table,
}

#[derive(Debug, Deserialize)]
struct RawPackageSection {
    name: String,
    #[serde(default)]
    version: Option<RawVersion>,
}

/// Rohform von `[package].version`. Siehe den Modul-Kommentar für die drei
/// gültigen Formen; jede unbekannte Form liefert beim Deserialisieren einen
/// Fehler (`serde::de::Error::custom`), der über `toml::from_str` als
/// `toml::de::Error` aufsteigt und per `#[from]` zu
/// `CodeGraphError::Toml` wird — kein stiller leerer Wert.
#[derive(Debug)]
enum RawVersion {
    /// `version = "0.2.0"`.
    Literal(String),
    /// `version.workspace = true`, TOML-intern `{ workspace = true }`.
    WorkspaceInherited,
}

impl<'de> Deserialize<'de> for RawVersion {
    /// Liest den rohen TOML-Wert von `[package].version` und klassifiziert
    /// ihn in eine der zwei bekannten Formen. Ein anderer Wert (z. B.
    /// `{ workspace = false }`, eine Zahl oder eine Liste) erzeugt einen
    /// Deserialisierungsfehler statt eines übergangenen leeren Werts.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = toml::Value::deserialize(deserializer)?;
        match value {
            toml::Value::String(literal) => Ok(RawVersion::Literal(literal)),
            toml::Value::Table(table) if table.get("workspace") == Some(&toml::Value::Boolean(true)) => {
                Ok(RawVersion::WorkspaceInherited)
            }
            other => Err(serde::de::Error::custom(format!(
                "unbekannte Form von [package].version: {other:?} (erwartet eine Zeichenkette oder {{ workspace = true }})"
            ))),
        }
    }
}

impl WorkspaceGraph {
    /// Liest `<root>/Cargo.toml` (`[workspace].members`, Globs wie `crates/*`
    /// werden aufgelöst) sowie jedes Member-Manifest und berechnet Ebenen und
    /// Leaf-Flags. Siehe AP W1-14..17, Abschnitt `workspace.rs`.
    pub fn load(root: &Path) -> CodeGraphResult<Self> {
        let root_manifest_path = root.join("Cargo.toml");
        if !root_manifest_path.is_file() {
            return Err(CodeGraphError::ManifestMissing {
                path: root_manifest_path.display().to_string(),
            });
        }
        let root_content = fs::read_to_string(&root_manifest_path)?;
        let root_manifest: RawRootManifest = toml::from_str(&root_content)?;
        let workspace_section =
            root_manifest
                .workspace
                .ok_or_else(|| CodeGraphError::NoWorkspaceSection {
                    path: root_manifest_path.display().to_string(),
                })?;

        let member_patterns = workspace_section.members.unwrap_or_default();
        let member_dirs = resolve_member_dirs(root, &member_patterns)?;

        // Erster Durchlauf: alle Member-Manifeste vollständig einlesen, damit
        // im zweiten Durchlauf interne gegen externe Abhängigkeiten anhand
        // der vollständigen Namensmenge klassifiziert werden können.
        let mut parsed: Vec<(PathBuf, PathBuf, RawMemberManifest)> =
            Vec::with_capacity(member_dirs.len());
        for dir in &member_dirs {
            let manifest_path = dir.join("Cargo.toml");
            if !manifest_path.is_file() {
                return Err(CodeGraphError::ManifestMissing {
                    path: manifest_path.display().to_string(),
                });
            }
            let content = fs::read_to_string(&manifest_path)?;
            let manifest: RawMemberManifest = toml::from_str(&content)?;
            parsed.push((dir.clone(), manifest_path, manifest));
        }

        let member_names: HashSet<String> = parsed
            .iter()
            .map(|(_, _, manifest)| manifest.package.name.clone())
            .collect();

        let mut crates: Vec<CrateNode> = Vec::with_capacity(parsed.len());
        for (dir, manifest_path, manifest) in parsed {
            let (deps, external_deps) =
                classify_dependencies(&manifest.dependencies, &member_names);
            let is_leaf = deps.is_empty();
            crates.push(CrateNode {
                name: manifest.package.name,
                version: match manifest.package.version {
                    Some(RawVersion::Literal(literal)) => literal,
                    Some(RawVersion::WorkspaceInherited) | None => "0.0.0".to_owned(),
                },
                manifest_path,
                dir,
                deps,
                dev_deps: dependency_names(&manifest.dev_dependencies),
                build_deps: dependency_names(&manifest.build_dependencies),
                external_deps,
                is_leaf,
                level: 0,
            });
        }

        let level_map = compute_levels(&crates)?;
        for node in &mut crates {
            node.level = level_map.get(&node.name).copied().unwrap_or(0);
        }

        Ok(Self {
            root: root.to_path_buf(),
            crates,
        })
    }

    /// Sucht ein Crate anhand seines Namens.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&CrateNode> {
        self.crates.iter().find(|node| node.name == name)
    }

    /// Gruppiert alle Crates in Ebenen von unten: Index `0` sind die Leaves.
    /// Erkennt Zyklen unabhängig von einer bereits in [`Self::load`]
    /// erfolgten Berechnung erneut und meldet sie als `CycleDetected`.
    pub fn topological_levels(&self) -> CodeGraphResult<Vec<Vec<&CrateNode>>> {
        if self.crates.is_empty() {
            return Ok(Vec::new());
        }
        let level_map = compute_levels(&self.crates)?;
        let max_level = level_map.values().copied().max().unwrap_or(0);
        let mut levels: Vec<Vec<&CrateNode>> =
            (0..=max_level).map(|_| Vec::new()).collect();
        for node in &self.crates {
            let level = level_map.get(&node.name).copied().unwrap_or(0);
            levels[level as usize].push(node);
        }
        for bucket in &mut levels {
            bucket.sort_by(|a, b| a.name.cmp(&b.name));
        }
        Ok(levels)
    }

    /// Alle Crates, die `name` direkt (über `deps`) konsumieren, sortiert
    /// nach Name.
    #[must_use]
    pub fn consumers_of(&self, name: &str) -> Vec<&CrateNode> {
        let mut consumers: Vec<&CrateNode> = self
            .crates
            .iter()
            .filter(|node| node.deps.iter().any(|dep| dep == name))
            .collect();
        consumers.sort_by(|a, b| a.name.cmp(&b.name));
        consumers
    }

    /// Flache Leaf-first-Reihenfolge, stabil sortiert nach Ebene, dann Name.
    pub fn leaf_first_order(&self) -> CodeGraphResult<Vec<&CrateNode>> {
        Ok(self.topological_levels()?.into_iter().flatten().collect())
    }

    /// Teilgraph aus `name` und allen transitiven internen Abhängigkeiten
    /// (iterative Traversierung, kein rekursiver Aufruf).
    pub fn subgraph(&self, name: &str) -> CodeGraphResult<Self> {
        let root_node = self
            .get(name)
            .ok_or_else(|| CodeGraphError::MemberMissing {
                name: name.to_owned(),
            })?;

        let mut included: HashSet<String> = HashSet::new();
        let mut stack: Vec<String> = vec![root_node.name.clone()];
        while let Some(current) = stack.pop() {
            if !included.insert(current.clone()) {
                continue;
            }
            if let Some(node) = self.get(&current) {
                for dep in &node.deps {
                    if !included.contains(dep) {
                        stack.push(dep.clone());
                    }
                }
            }
        }

        let crates: Vec<CrateNode> = self
            .crates
            .iter()
            .filter(|node| included.contains(&node.name))
            .cloned()
            .collect();

        Ok(Self {
            root: self.root.clone(),
            crates,
        })
    }

    /// Kompakte Textprojektion (`Ebene N: crate-a, crate-b`) für Prompts und
    /// Berichte.
    pub fn render_levels(&self) -> CodeGraphResult<String> {
        let levels = self.topological_levels()?;
        let mut out = String::new();
        for (index, level_crates) in levels.iter().enumerate() {
            let names: Vec<&str> = level_crates.iter().map(|node| node.name.as_str()).collect();
            out.push_str(&format!("Ebene {index}: {}\n", names.join(", ")));
        }
        Ok(out)
    }
}

/// Löst `[workspace].members`-Einträge zu absoluten Crate-Verzeichnissen auf.
/// Unterstützt literale relative Pfade sowie das Glob-Muster `<prefix>/*`
/// (ein Verzeichnis-Level, nur Unterverzeichnisse mit `Cargo.toml`). Andere
/// Glob-Formen sind nicht unterstützt und ergeben `InvalidPath`.
fn resolve_member_dirs(root: &Path, patterns: &[String]) -> CodeGraphResult<Vec<PathBuf>> {
    let mut resolved = Vec::new();
    for pattern in patterns {
        if let Some(prefix) = pattern.strip_suffix("/*") {
            let base = root.join(prefix);
            let entries = fs::read_dir(&base)?;
            let mut matched: Vec<PathBuf> = Vec::new();
            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() && path.join("Cargo.toml").is_file() {
                    matched.push(path);
                }
            }
            matched.sort();
            resolved.extend(matched);
        } else if pattern.contains('*') {
            return Err(CodeGraphError::InvalidPath {
                path: pattern.clone(),
            });
        } else {
            resolved.push(root.join(pattern));
        }
    }
    Ok(resolved)
}

/// Klassifiziert die Einträge einer `[dependencies]`-Tabelle in
/// workspace-interne (Schlüssel ist der Name eines tatsächlichen Members)
/// und externe Abhängigkeiten. Beide Listen sind nach Name sortiert.
///
/// Ein `path`-Feld allein macht eine Abhängigkeit **nicht** intern: ein
/// Member kann per `path` auf ein Crate außerhalb dieses Workspace zeigen
/// (z. B. `crypt_guard = { path = "../../crypt_guard" }` in
/// `harw-secrets/Cargo.toml`, ein Sibling-Repo, kein Member). Vor dieser
/// Reparatur wurde jede `path`-Abhängigkeit unabhängig vom Ziel als intern
/// gezählt; [`compute_levels`] konnte einen so klassifizierten, aber nie im
/// Graphen auftauchenden Namen naturgemäß nie auflösen, und jeder davon
/// abhängige Knoten blieb dauerhaft in der Restmenge — was fälschlich als
/// Abhängigkeitszyklus gemeldet wurde, obwohl es sich um eine Kante auf ein
/// Nicht-Member handelte. Maßgeblich ist daher ausschließlich die
/// Mitgliedschaft in `member_names`.
fn classify_dependencies(
    table: &toml::Table,
    member_names: &HashSet<String>,
) -> (Vec<String>, Vec<String>) {
    let mut internal = Vec::new();
    let mut external = Vec::new();
    for dep_name in table.keys() {
        if member_names.contains(dep_name) {
            internal.push(dep_name.clone());
        } else {
            external.push(dep_name.clone());
        }
    }
    internal.sort();
    external.sort();
    (internal, external)
}

/// Liefert alle Schlüssel einer Dependency-Tabelle als sortierte Namensliste,
/// ohne intern/extern zu unterscheiden (für `dev_deps`/`build_deps`).
fn dependency_names(table: &toml::Table) -> Vec<String> {
    let mut names: Vec<String> = table.keys().cloned().collect();
    names.sort();
    names
}

/// Berechnet iterativ (Kahn, kein rekursiver Stack-Overflow) für jedes Crate
/// seine Ebene: `0` für Leaves, sonst `1 + max(Ebene der internen `deps`)`.
///
/// Bevor Kahn überhaupt anläuft, wird geprüft, dass jede in `deps`
/// genannte interne Abhängigkeit tatsächlich als Knoten in `crates`
/// existiert. Eine Kante auf einen unbekannten Namen würde in Kahn nie
/// aufgelöst und ihr Quellknoten (und alles, was transitiv von ihm abhängt)
/// bliebe für immer in der Restmenge — was ununterscheidbar von einem
/// echten Zyklus aussähe, obwohl die Ursache eine **unbekannte Kante** ist,
/// kein Kreis. Diese Prüfung meldet den fehlenden Namen deshalb explizit
/// als [`CodeGraphError::MemberMissing`], bevor Kahn ihn stillschweigend als
/// Zyklus fehldeutet.
///
/// Kann *nach* dieser Prüfung kein weiterer Knoten aufgelöst werden, bilden
/// die verbleibenden Knoten einen echten Abhängigkeitszyklus.
fn compute_levels(crates: &[CrateNode]) -> CodeGraphResult<HashMap<String, u32>> {
    let known_names: HashSet<&str> = crates.iter().map(|node| node.name.as_str()).collect();
    for node in crates {
        for dep in &node.deps {
            if !known_names.contains(dep.as_str()) {
                return Err(CodeGraphError::MemberMissing { name: dep.clone() });
            }
        }
    }

    let mut resolved: HashMap<String, u32> = HashMap::new();
    let mut remaining: Vec<&CrateNode> = crates.iter().collect();

    while !remaining.is_empty() {
        let mut next_remaining = Vec::new();
        let mut progressed = false;

        for node in remaining {
            if node.deps.iter().all(|dep| resolved.contains_key(dep)) {
                let level = node
                    .deps
                    .iter()
                    .filter_map(|dep| resolved.get(dep).copied())
                    .max()
                    .map_or(0, |max| max + 1);
                resolved.insert(node.name.clone(), level);
                progressed = true;
            } else {
                next_remaining.push(node);
            }
        }

        if !progressed {
            let crates_in_cycle = next_remaining
                .iter()
                .map(|node| node.name.clone())
                .collect();
            return Err(CodeGraphError::CycleDetected {
                crates: crates_in_cycle,
            });
        }
        remaining = next_remaining;
    }

    Ok(resolved)
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

    fn write_crate(root: &Path, name: &str, deps: &[&str]) {
        write_crate_with_version_line(root, name, deps, "version = \"0.1.0\"");
    }

    /// Wie [`write_crate`], erlaubt aber, die `version`-Zeile (oder ihr
    /// Fehlen, per leerer Zeichenkette) frei vorzugeben — damit sich alle
    /// drei gültigen Formen sowie unbekannte Formen als Fixture nachbauen
    /// lassen.
    fn write_crate_with_version_line(root: &Path, name: &str, deps: &[&str], version_line: &str) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).expect("Crate-Verzeichnis anlegen");
        let mut deps_section = String::new();
        for dep in deps {
            deps_section.push_str(&format!("{dep} = {{ path = \"../{dep}\" }}\n"));
        }
        let manifest = format!(
            "[package]\nname = \"{name}\"\n{version_line}\n\n[dependencies]\n{deps_section}"
        );
        fs::write(dir.join("Cargo.toml"), manifest).expect("Member-Cargo.toml schreiben");
    }

    fn write_root(root: &Path, members: &[&str]) {
        let members_toml = members
            .iter()
            .map(|member| format!("\"{member}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let manifest = format!("[workspace]\nmembers = [{members_toml}]\n");
        fs::write(root.join("Cargo.toml"), manifest).expect("Wurzel-Cargo.toml schreiben");
    }

    #[test]
    fn topological_levels_orders_leaf_first_for_a_b_c_chain() {
        let root = scratch_dir("levels");
        write_crate(&root, "a", &[]);
        write_crate(&root, "b", &["a"]);
        write_crate(&root, "c", &["b"]);
        write_root(&root, &["a", "b", "c"]);

        let graph = WorkspaceGraph::load(&root).expect("Workspace laden");
        let levels = graph.topological_levels().expect("Ebenen berechnen");

        let level_names: Vec<Vec<&str>> = levels
            .iter()
            .map(|level| level.iter().map(|node| node.name.as_str()).collect())
            .collect();
        assert_eq!(level_names, vec![vec!["a"], vec!["b"], vec!["c"]]);

        let node_a = graph.get("a").expect("Crate a gefunden");
        assert!(node_a.is_leaf);
        assert_eq!(node_a.level, 0);

        let consumers = graph.consumers_of("a");
        assert_eq!(consumers.len(), 1);
        assert_eq!(consumers[0].name, "b");

        let order = graph.leaf_first_order().expect("Leaf-first-Reihenfolge");
        let order_names: Vec<&str> = order.iter().map(|node| node.name.as_str()).collect();
        assert_eq!(order_names, vec!["a", "b", "c"]);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn topological_levels_detects_cycle() {
        let root = scratch_dir("cycle");
        write_crate(&root, "a", &["b"]);
        write_crate(&root, "b", &["a"]);
        write_root(&root, &["a", "b"]);

        let result = WorkspaceGraph::load(&root);
        assert!(matches!(result, Err(CodeGraphError::CycleDetected { .. })));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn subgraph_includes_only_transitive_internal_deps() {
        let root = scratch_dir("subgraph");
        write_crate(&root, "a", &[]);
        write_crate(&root, "b", &["a"]);
        write_crate(&root, "c", &[]);
        write_root(&root, &["a", "b", "c"]);

        let graph = WorkspaceGraph::load(&root).expect("Workspace laden");
        let sub = graph.subgraph("b").expect("Teilgraph bauen");
        let mut names: Vec<&str> = sub.crates.iter().map(|node| node.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["a", "b"]);

        assert!(matches!(
            graph.subgraph("does-not-exist"),
            Err(CodeGraphError::MemberMissing { .. })
        ));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_reads_version_workspace_true() {
        // Der wichtigste Regressionstest: vor der Reparatur scheiterte
        // `WorkspaceGraph::load` an genau dieser Form, weil `version` als
        // `Option<String>` deklariert war.
        let root = scratch_dir("version-workspace-true");
        write_crate_with_version_line(&root, "a", &[], "version.workspace = true");
        write_root(&root, &["a"]);

        let graph = WorkspaceGraph::load(&root).expect("Workspace mit version.workspace=true laden");
        let node_a = graph.get("a").expect("Crate a gefunden");
        assert_eq!(node_a.version, "0.0.0");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_reads_literal_version_string() {
        let root = scratch_dir("version-literal");
        write_crate_with_version_line(&root, "a", &[], "version = \"0.2.0\"");
        write_root(&root, &["a"]);

        let graph = WorkspaceGraph::load(&root).expect("Workspace mit wörtlicher Version laden");
        let node_a = graph.get("a").expect("Crate a gefunden");
        assert_eq!(node_a.version, "0.2.0");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_reads_missing_version_as_default() {
        let root = scratch_dir("version-missing");
        write_crate_with_version_line(&root, "a", &[], "");
        write_root(&root, &["a"]);

        let graph = WorkspaceGraph::load(&root).expect("Workspace ohne version-Feld laden");
        let node_a = graph.get("a").expect("Crate a gefunden");
        assert_eq!(node_a.version, "0.0.0");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_rejects_unknown_version_form() {
        let root = scratch_dir("version-unknown");
        write_crate_with_version_line(&root, "a", &[], "version = { workspace = false }");
        write_root(&root, &["a"]);

        let result = WorkspaceGraph::load(&root);
        assert!(
            matches!(result, Err(CodeGraphError::Toml(_))),
            "unbekannte version-Form muss einen Fehler liefern, kein stilles Standardwert-Ergebnis: {result:?}"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_reads_real_workspace_with_member_count_from_manifest() {
        // Lädt den tatsächlichen Repo-Workspace statt eines konstruierten
        // Fixtures. Die bisherigen Tests konstruierten sich stets einen
        // Workspace in der (inzwischen überholten) wörtlichen Versionsform
        // und hätten den AW0-00-Regressionsfehler nie gesehen.
        //
        // Die erwartete Mitgliederzahl wird bewusst NICHT als Literal
        // eingetragen: die Wurzel-`Cargo.toml` wächst und schrumpft mit dem
        // Workspace (zuletzt 95 → 68 Member), und ein hartkodierter Wert
        // veraltet bei jeder solchen Änderung stillschweigend. Stattdessen
        // liest dieser Test `[workspace].members` direkt aus derselben
        // Wurzel-Manifest-Datei, die auch [`WorkspaceGraph::load`] liest, und
        // löst sie über dieselbe [`resolve_member_dirs`]-Funktion auf — so
        // bleibt die Erwartung an die Quelle der Wahrheit gebunden, statt an
        // eine Momentaufnahme.
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir
            .parent()
            .expect("übergeordnetes Workspace-Verzeichnis von harw-code-graph");

        let root_content = fs::read_to_string(workspace_root.join("Cargo.toml"))
            .expect("Wurzel-Cargo.toml lesen");
        let root_manifest: RawRootManifest =
            toml::from_str(&root_content).expect("Wurzel-Cargo.toml parsen");
        let member_patterns = root_manifest
            .workspace
            .expect("[workspace]-Abschnitt in der Wurzel-Cargo.toml")
            .members
            .unwrap_or_default();
        let expected_member_dirs = resolve_member_dirs(workspace_root, &member_patterns)
            .expect("Member-Verzeichnisse aus der Wurzel-Cargo.toml auflösen");
        let expected_count = expected_member_dirs.len();

        let graph = WorkspaceGraph::load(workspace_root).expect("echten Workspace laden");
        assert_eq!(
            graph.crates.len(),
            expected_count,
            "erwartete {expected_count} Workspace-Member (aus Cargo.toml), gefunden: {:?}",
            graph.crates.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
        );

        // Laden allein hätte den `crypt_guard`-Fehlklassifikationsfehler
        // (Pfad-Abhängigkeit außerhalb des Workspace fälschlich als intern
        // gezählt) nicht gefangen — der schlägt erst hier zu, wenn Kahn
        // versucht, den Graphen topologisch zu sortieren.
        let levels = graph
            .topological_levels()
            .expect("echten Workspace topologisch sortieren, kein falscher Zyklus");
        let sorted_names: HashSet<&str> = levels
            .iter()
            .flatten()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(
            sorted_names.len(),
            expected_count,
            "alle {expected_count} Member müssen in genau einer Ebene auftauchen"
        );

        let secrets = graph.get("harw-secrets").expect("harw-secrets gefunden");
        assert!(
            secrets.external_deps.iter().any(|dep| dep == "crypt_guard"),
            "crypt_guard zeigt per path auf ein Sibling-Repo außerhalb des Workspace und muss extern klassifiziert werden, gefunden: {:?}",
            secrets.external_deps
        );
        assert!(
            !secrets.deps.iter().any(|dep| dep == "crypt_guard"),
            "crypt_guard darf nicht als interne Abhängigkeit gezählt werden"
        );
    }

    #[test]
    fn load_treats_path_dependency_outside_workspace_as_external() {
        // Nachbau des `crypt_guard`-Befunds in Miniatur: `b` hat neben der
        // echten internen Abhängigkeit `a` eine `path`-Abhängigkeit auf ein
        // Verzeichnis, das kein Workspace-Member ist.
        let root = scratch_dir("path-dep-outside-workspace");
        write_crate(&root, "a", &[]);
        let b_dir = root.join("b");
        fs::create_dir_all(&b_dir).expect("Crate-Verzeichnis b anlegen");
        fs::write(
            b_dir.join("Cargo.toml"),
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\n\n[dependencies]\na = { path = \"../a\" }\nexternal-lib = { path = \"../../external-lib\" }\n",
        )
        .expect("Cargo.toml von b schreiben");
        write_root(&root, &["a", "b"]);

        let graph = WorkspaceGraph::load(&root).expect("Workspace laden");
        let node_b = graph.get("b").expect("Crate b gefunden");
        assert_eq!(node_b.deps, vec!["a".to_owned()]);
        assert_eq!(node_b.external_deps, vec!["external-lib".to_owned()]);

        let levels = graph
            .topological_levels()
            .expect("darf keinen falschen Zyklus melden");
        assert_eq!(levels.len(), 2, "a auf Ebene 0, b auf Ebene 1");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn topological_levels_reports_member_missing_for_unknown_edge_instead_of_cycle() {
        // Direkt konstruierter Graph mit einer Kante auf einen Namen, der
        // als Knoten nicht existiert. Das darf NICHT als `CycleDetected`
        // erscheinen — genau diese Verwechslung führte `xtask gates` beim
        // `crypt_guard`-Befund auf eine Suche ins Leere (vermeintlicher
        // Zyklus über harw-secrets/harw-channel/.../harw-cli).
        let dangling = CrateNode {
            name: "b".to_owned(),
            version: "0.0.0".to_owned(),
            manifest_path: PathBuf::from("b/Cargo.toml"),
            dir: PathBuf::from("b"),
            deps: vec!["ghost".to_owned()],
            dev_deps: Vec::new(),
            build_deps: Vec::new(),
            external_deps: Vec::new(),
            is_leaf: false,
            level: 0,
        };
        let graph = WorkspaceGraph {
            root: PathBuf::from("."),
            crates: vec![dangling],
        };

        let result = graph.topological_levels();
        match result {
            Err(CodeGraphError::MemberMissing { name }) => assert_eq!(name, "ghost"),
            other => panic!(
                "eine Kante auf einen unbekannten Namen muss MemberMissing melden, kein CycleDetected: {other:?}"
            ),
        }
    }
}
