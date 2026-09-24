//! Relationen zwischen Projekten und Dokumenten im Baum.
//!
//! [`find_relations`] leitet aus den erkannten [`Project`]s und den Einträgen
//! ([`Node`]) gerichtete Beziehungen ab:
//!
//! - **Workspace-Mitglied**: Workspace-Wurzel → deklariertes Mitglied.
//! - **Pfad-Abhängigkeit**: `path = "…"` in `Cargo.toml`, `file:`/`link:`/
//!   `workspace:<pfad>` in `package.json`, `path = "…"` in `pyproject.toml`
//!   (Poetry/uv).
//! - **Crate-Abhängigkeit**: namentliche Abhängigkeit, deren Name im Baum als
//!   Projekt gleicher Ökosystem-Art existiert.
//! - **Verschachteltes Projekt**: innerstes umgebendes Projekt → Projekt,
//!   sofern nicht bereits als Workspace-Mitglied erfasst.
//! - **Dokument-Link**: lokale Links in Markdown-Dateien, deren Ziel als
//!   Eintrag im Baum existiert.
//!
//! Alle Pfade sind relativ zur Explorer-Wurzel und werden rein lexikalisch
//! normalisiert (kein Auflösen von Symlinks). Nicht lesbare oder fehlerhafte
//! Manifeste werden per `tracing::debug` protokolliert und übersprungen.

use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};

use crate::{FileKind, Node, Project, ProjectKind, Relation, RelationKind};

/// Markdown-Dateien oberhalb dieser Größe werden nicht nach Links durchsucht.
const MAX_MARKDOWN_BYTES: u64 = 1024 * 1024;
/// Obergrenze an (eindeutigen) Dokument-Links über den gesamten Baum.
const MAX_DOC_LINKS: usize = 2000;
/// Maximale Länge eines Link-Labels in Zeichen.
const MAX_LABEL_CHARS: usize = 80;

/// Ermittelt alle Relationen im Baum unter `root`.
///
/// `projects` und `nodes` tragen Pfade relativ zu `root`. Das Ergebnis ist
/// nach `(from, to, kind)` dedupliziert (ein vorhandenes Label gewinnt über
/// ein fehlendes) und deterministisch sortiert.
#[must_use]
pub fn find_relations(root: &Path, projects: &[Project], nodes: &[Node]) -> Vec<Relation> {
    let mut sink = RelationSink::default();

    workspace_members(projects, &mut sink);
    for project in projects {
        match project.kind {
            ProjectKind::CargoCrate | ProjectKind::CargoWorkspace => {
                cargo_dependencies(root, project, projects, &mut sink);
            }
            ProjectKind::Node => npm_dependencies(root, project, projects, &mut sink),
            ProjectKind::Python => python_dependencies(root, project, &mut sink),
            ProjectKind::Go | ProjectKind::Git | ProjectKind::Documents => {}
        }
    }
    nested_projects(projects, &mut sink);
    doc_links(root, nodes, &mut sink);

    sink.finish()
}

// ---------------------------------------------------------------------------
// Sammeln, Deduplizieren, Sortieren
// ---------------------------------------------------------------------------

/// Sammelt Relationen dedupliziert nach `(from, to, kind)`.
#[derive(Default)]
struct RelationSink {
    entries: BTreeMap<(PathBuf, PathBuf, u8), Option<String>>,
}

impl RelationSink {
    /// Fügt eine Relation hinzu; Selbstbezüge werden verworfen. Gibt `true`
    /// zurück, wenn das Tripel neu war.
    fn push(
        &mut self,
        from: PathBuf,
        to: PathBuf,
        kind: RelationKind,
        label: Option<String>,
    ) -> bool {
        if from == to {
            return false;
        }
        let key = (from, to, kind_rank(kind));
        match self.entries.get_mut(&key) {
            Some(existing) => {
                if existing.is_none() && label.is_some() {
                    *existing = label;
                }
                false
            }
            None => {
                self.entries.insert(key, label);
                true
            }
        }
    }

    fn contains(&self, from: &Path, to: &Path, kind: RelationKind) -> bool {
        self.entries
            .contains_key(&(from.to_path_buf(), to.to_path_buf(), kind_rank(kind)))
    }

    fn finish(self) -> Vec<Relation> {
        self.entries
            .into_iter()
            .map(|((from, to, rank), label)| Relation {
                from,
                to,
                kind: kind_from_rank(rank),
                label,
            })
            .collect()
    }
}

const KINDS: [RelationKind; 5] = [
    RelationKind::WorkspaceMember,
    RelationKind::PathDependency,
    RelationKind::CrateDependency,
    RelationKind::NestedProject,
    RelationKind::DocLink,
];

fn kind_rank(kind: RelationKind) -> u8 {
    match kind {
        RelationKind::WorkspaceMember => 0,
        RelationKind::PathDependency => 1,
        RelationKind::CrateDependency => 2,
        RelationKind::NestedProject => 3,
        RelationKind::DocLink => 4,
    }
}

fn kind_from_rank(rank: u8) -> RelationKind {
    KINDS
        .get(usize::from(rank))
        .copied()
        .unwrap_or(RelationKind::DocLink)
}

// ---------------------------------------------------------------------------
// Pfade
// ---------------------------------------------------------------------------

/// Normalisiert einen relativen Pfad rein lexikalisch (`.` entfällt, `..`
/// entfernt die vorige Komponente). `None`, wenn der Pfad absolut ist oder
/// über seinen Anfang hinaus nach oben führt.
fn normalize_relative(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

/// Normalisiert einen absoluten Pfad lexikalisch (`..` an der Wurzel bleibt
/// an der Wurzel).
fn normalize_absolute(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if out.parent().is_some() {
                    out.pop();
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Löst `target` (aus einem Manifest oder Link) relativ zu `base` (relativ zur
/// Explorer-Wurzel) auf. Absolute Ziele werden gegen `root` abgeglichen.
/// Ergebnis relativ zur Wurzel, oder `None`, wenn das Ziel außerhalb liegt.
fn resolve(root: &Path, base: &Path, target: &str) -> Option<PathBuf> {
    let target_path = Path::new(target);
    if target_path.is_absolute() {
        let absolute = normalize_absolute(target_path);
        let root_norm = normalize_absolute(root);
        let rel = absolute
            .strip_prefix(&root_norm)
            .or_else(|_| absolute.strip_prefix(root))
            .ok()?;
        normalize_relative(rel)
    } else {
        normalize_relative(&base.join(target_path))
    }
}

/// Manifest-Pfad eines Projekts (relativ zur Wurzel): das deklarierte
/// Manifest, falls es `file_name` heißt, sonst `<root>/<file_name>`.
fn manifest_path(project: &Project, file_name: &str) -> PathBuf {
    match &project.manifest {
        Some(manifest) if manifest.file_name().is_some_and(|name| name == file_name) => {
            manifest.clone()
        }
        _ => project.root.join(file_name),
    }
}

fn read_manifest(root: &Path, rel: &Path) -> Option<String> {
    match std::fs::read_to_string(root.join(rel)) {
        Ok(text) => Some(text),
        Err(error) => {
            tracing::debug!(path = %rel.display(), %error, "manifest not readable");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Workspace-Mitglieder und Verschachtelung
// ---------------------------------------------------------------------------

/// Ökosystem-Verwandtschaft zweier Projektarten (für die Namenswahl).
fn same_ecosystem(a: ProjectKind, b: ProjectKind) -> bool {
    let cargo = |k| matches!(k, ProjectKind::CargoCrate | ProjectKind::CargoWorkspace);
    (cargo(a) && cargo(b)) || a == b
}

/// Bester Anzeigename für das Projekt an `root`: bevorzugt dieselbe
/// Ökosystem-Art, dann jede Art außer Git/Dokumente, dann irgendeine.
fn project_name_at<'a>(
    projects: &'a [Project],
    root: &Path,
    prefer: Option<ProjectKind>,
) -> Option<&'a str> {
    let at_root = || projects.iter().filter(move |p| p.root == root);
    let preferred = prefer.and_then(|kind| at_root().find(|p| same_ecosystem(p.kind, kind)));
    preferred
        .or_else(|| {
            at_root().find(|p| !matches!(p.kind, ProjectKind::Git | ProjectKind::Documents))
        })
        .or_else(|| at_root().next())
        .map(|p| p.name.as_str())
}

fn workspace_members(projects: &[Project], sink: &mut RelationSink) {
    for project in projects {
        for member in &project.members {
            let label = project_name_at(projects, member, Some(project.kind)).map(str::to_owned);
            sink.push(
                project.root.clone(),
                member.clone(),
                RelationKind::WorkspaceMember,
                label,
            );
        }
    }
}

fn nested_projects(projects: &[Project], sink: &mut RelationSink) {
    let mut roots: Vec<&Path> = projects.iter().map(|p| p.root.as_path()).collect();
    roots.sort();
    roots.dedup();

    for inner in &roots {
        let outer = roots
            .iter()
            .filter(|candidate| candidate != &inner && inner.starts_with(candidate))
            .max_by_key(|candidate| candidate.components().count());
        let Some(outer) = outer else { continue };
        if sink.contains(outer, inner, RelationKind::WorkspaceMember) {
            continue;
        }
        let label = project_name_at(projects, inner, None).map(str::to_owned);
        sink.push(
            outer.to_path_buf(),
            inner.to_path_buf(),
            RelationKind::NestedProject,
            label,
        );
    }
}

// ---------------------------------------------------------------------------
// Abhängigkeiten
// ---------------------------------------------------------------------------

/// Verweis einer einzelnen Abhängigkeit.
enum DepRef {
    Path(String),
    Name(String),
}

/// Trägt eine Pfad-Abhängigkeit ein, sofern das Ziel innerhalb der Wurzel
/// als Verzeichnis existiert.
fn push_path_dep(root: &Path, from: &Path, target: &str, label: &str, sink: &mut RelationSink) {
    let Some(to) = resolve(root, from, target) else {
        tracing::debug!(from = %from.display(), target, "path dependency outside explorer root");
        return;
    };
    if !root.join(&to).is_dir() {
        tracing::debug!(from = %from.display(), to = %to.display(), "path dependency target missing");
        return;
    }
    sink.push(
        from.to_path_buf(),
        to,
        RelationKind::PathDependency,
        Some(label.to_owned()),
    );
}

/// Trägt namentliche Abhängigkeiten auf passende Projekte der Art(en) ein.
fn push_name_dep(
    from: &Path,
    name: &str,
    projects: &[Project],
    kinds: &[ProjectKind],
    sink: &mut RelationSink,
) {
    for target in projects
        .iter()
        .filter(|p| kinds.contains(&p.kind) && p.name == name && p.root != from)
    {
        sink.push(
            from.to_path_buf(),
            target.root.clone(),
            RelationKind::CrateDependency,
            Some(name.to_owned()),
        );
    }
}

const CARGO_DEP_SECTIONS: [&str; 5] = [
    "dependencies",
    "dev-dependencies",
    "build-dependencies",
    "dev_dependencies",
    "build_dependencies",
];

/// Sammelt `(Schlüssel, Verweis)` aus allen Abhängigkeitstabellen eines
/// `Cargo.toml` (inkl. `[target.*.…]` und `[workspace.dependencies]`).
fn cargo_dep_refs(manifest: &toml::Table) -> Vec<(String, DepRef)> {
    let mut sections: Vec<&toml::Table> = Vec::new();
    push_sections(manifest, &mut sections);
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for target in targets.values().filter_map(toml::Value::as_table) {
            push_sections(target, &mut sections);
        }
    }
    if let Some(deps) = manifest
        .get("workspace")
        .and_then(toml::Value::as_table)
        .and_then(|ws| ws.get("dependencies"))
        .and_then(toml::Value::as_table)
    {
        sections.push(deps);
    }

    let mut refs = Vec::new();
    for section in sections {
        for (key, value) in section {
            let dep = match value {
                toml::Value::Table(spec) => match spec.get("path").and_then(toml::Value::as_str) {
                    Some(path) => DepRef::Path(path.to_owned()),
                    None => DepRef::Name(
                        spec.get("package")
                            .and_then(toml::Value::as_str)
                            .unwrap_or(key)
                            .to_owned(),
                    ),
                },
                _ => DepRef::Name(key.clone()),
            };
            refs.push((key.clone(), dep));
        }
    }
    refs
}

fn push_sections<'a>(table: &'a toml::Table, out: &mut Vec<&'a toml::Table>) {
    for name in CARGO_DEP_SECTIONS {
        if let Some(section) = table.get(name).and_then(toml::Value::as_table) {
            out.push(section);
        }
    }
}

fn cargo_dependencies(
    root: &Path,
    project: &Project,
    projects: &[Project],
    sink: &mut RelationSink,
) {
    let rel = manifest_path(project, "Cargo.toml");
    let Some(text) = read_manifest(root, &rel) else {
        return;
    };
    let manifest: toml::Table = match toml::from_str(&text) {
        Ok(table) => table,
        Err(error) => {
            tracing::debug!(path = %rel.display(), %error, "Cargo.toml not parseable");
            return;
        }
    };
    let cargo_kinds = [ProjectKind::CargoCrate];
    for (key, dep) in cargo_dep_refs(&manifest) {
        match dep {
            DepRef::Path(path) => push_path_dep(root, &project.root, &path, &key, sink),
            DepRef::Name(name) => push_name_dep(&project.root, &name, projects, &cargo_kinds, sink),
        }
    }
}

const NPM_DEP_SECTIONS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];

/// Deutet eine npm-Versionsangabe: lokale Pfade (`file:`, `link:`,
/// `workspace:<pfad>`) werden zu [`DepRef::Path`], alles andere zu
/// [`DepRef::Name`] mit dem Paketnamen.
fn npm_dep_ref(name: &str, spec: &str) -> DepRef {
    let spec = spec.trim();
    for prefix in ["file:", "link:"] {
        if let Some(path) = spec.strip_prefix(prefix) {
            return DepRef::Path(path.to_owned());
        }
    }
    if let Some(rest) = spec.strip_prefix("workspace:") {
        if rest.starts_with("./") || rest.starts_with("../") || rest.starts_with('/') {
            return DepRef::Path(rest.to_owned());
        }
    }
    DepRef::Name(name.to_owned())
}

fn npm_dependencies(root: &Path, project: &Project, projects: &[Project], sink: &mut RelationSink) {
    let rel = manifest_path(project, "package.json");
    let Some(text) = read_manifest(root, &rel) else {
        return;
    };
    let manifest: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            tracing::debug!(path = %rel.display(), %error, "package.json not parseable");
            return;
        }
    };
    let node_kinds = [ProjectKind::Node];
    for section in NPM_DEP_SECTIONS {
        let Some(deps) = manifest.get(section).and_then(serde_json::Value::as_object) else {
            continue;
        };
        for (name, spec) in deps {
            let Some(spec) = spec.as_str() else { continue };
            match npm_dep_ref(name, spec) {
                DepRef::Path(path) => push_path_dep(root, &project.root, &path, name, sink),
                DepRef::Name(name) => {
                    push_name_dep(&project.root, &name, projects, &node_kinds, sink)
                }
            }
        }
    }
}

/// Pfad-Abhängigkeiten aus `pyproject.toml`: `[tool.poetry.dependencies]`,
/// `[tool.poetry.group.*.dependencies]`, `[tool.poetry.dev-dependencies]` und
/// `[tool.uv.sources]` mit `path = "…"`.
fn python_dependencies(root: &Path, project: &Project, sink: &mut RelationSink) {
    let rel = manifest_path(project, "pyproject.toml");
    if !root.join(&rel).is_file() {
        return;
    }
    let Some(text) = read_manifest(root, &rel) else {
        return;
    };
    let manifest: toml::Table = match toml::from_str(&text) {
        Ok(table) => table,
        Err(error) => {
            tracing::debug!(path = %rel.display(), %error, "pyproject.toml not parseable");
            return;
        }
    };
    let Some(tool) = manifest.get("tool").and_then(toml::Value::as_table) else {
        return;
    };

    let mut sections: Vec<&toml::Table> = Vec::new();
    if let Some(poetry) = tool.get("poetry").and_then(toml::Value::as_table) {
        for name in ["dependencies", "dev-dependencies"] {
            if let Some(section) = poetry.get(name).and_then(toml::Value::as_table) {
                sections.push(section);
            }
        }
        if let Some(groups) = poetry.get("group").and_then(toml::Value::as_table) {
            for group in groups.values().filter_map(toml::Value::as_table) {
                if let Some(section) = group.get("dependencies").and_then(toml::Value::as_table) {
                    sections.push(section);
                }
            }
        }
    }
    if let Some(sources) = tool
        .get("uv")
        .and_then(toml::Value::as_table)
        .and_then(|uv| uv.get("sources"))
        .and_then(toml::Value::as_table)
    {
        sections.push(sources);
    }

    for section in sections {
        for (key, value) in section {
            if let Some(path) = value
                .as_table()
                .and_then(|spec| spec.get("path"))
                .and_then(toml::Value::as_str)
            {
                push_path_dep(root, &project.root, path, key, sink);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Dokument-Links
// ---------------------------------------------------------------------------

fn doc_links(root: &Path, nodes: &[Node], sink: &mut RelationSink) {
    let known: HashSet<&Path> = nodes.iter().map(|node| node.path.as_path()).collect();
    let mut markdown: Vec<&Node> = nodes
        .iter()
        .filter(|n| n.kind == FileKind::Markdown)
        .collect();
    markdown.sort_by(|a, b| a.path.cmp(&b.path));

    let mut count = 0usize;
    for node in markdown {
        if count >= MAX_DOC_LINKS {
            break;
        }
        if node.size > MAX_MARKDOWN_BYTES {
            continue;
        }
        let absolute = root.join(&node.path);
        match std::fs::metadata(&absolute) {
            Ok(meta) if meta.len() <= MAX_MARKDOWN_BYTES => {}
            Ok(_) => continue,
            Err(error) => {
                tracing::debug!(path = %node.path.display(), %error, "markdown not readable");
                continue;
            }
        }
        let text = match std::fs::read(&absolute) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) => {
                tracing::debug!(path = %node.path.display(), %error, "markdown not readable");
                continue;
            }
        };
        let base = node.path.parent().unwrap_or_else(|| Path::new(""));
        for (text, target) in extract_links(&text) {
            let Some(target) = local_link_target(&target) else {
                continue;
            };
            let resolved = match target.strip_prefix('/') {
                // Führender Schrägstrich: relativ zur Explorer-Wurzel (wie bei Git-Hostern).
                Some(from_root) => normalize_relative(Path::new(from_root)),
                None => normalize_relative(&base.join(&target)),
            };
            let Some(to) = resolved else { continue };
            if !to.as_os_str().is_empty() && !known.contains(to.as_path()) {
                continue;
            }
            if sink.push(
                node.path.clone(),
                to,
                RelationKind::DocLink,
                link_label(&text),
            ) {
                count += 1;
                if count >= MAX_DOC_LINKS {
                    break;
                }
            }
        }
    }
}

/// Bereinigt ein Link-Ziel: verwirft externe Schemata und reine Anker,
/// entfernt `#fragment` und `?query` und dekodiert Prozent-Kodierung.
fn local_link_target(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('#') || raw.starts_with("//") {
        return None;
    }
    if has_scheme(raw) {
        return None;
    }
    let end = raw.find(['#', '?']).unwrap_or(raw.len());
    let path = raw.get(..end)?.trim();
    if path.is_empty() {
        return None;
    }
    Some(percent_decode(path))
}

/// `true` für `scheme:` am Anfang (`http:`, `mailto:`, `data:` …).
fn has_scheme(target: &str) -> bool {
    let Some(colon) = target.find(':') else {
        return false;
    };
    let scheme = &target[..colon];
    scheme.len() > 1
        && scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn percent_decode(input: &str) -> String {
    if !input.contains('%') {
        return input.to_owned();
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(decoded) = hex {
                out.push(decoded);
                i += 3;
                continue;
            }
        }
        out.push(byte);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| input.to_owned())
}

/// Linktext → Label (getrimmt, höchstens [`MAX_LABEL_CHARS`] Zeichen).
fn link_label(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(MAX_LABEL_CHARS).collect())
}

/// Extrahiert `(Text, Ziel)` aus Inline-Links `[text](ziel)` (inkl. Bildern)
/// und Referenz-Definitionen `[id]: ziel`. Code-Blöcke und Inline-Code werden
/// übersprungen; Links werden zeilenweise erkannt.
fn extract_links(markdown: &str) -> Vec<(String, String)> {
    let mut links = Vec::new();
    let mut fence: Option<&str> = None;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        let marker = if trimmed.starts_with("```") {
            Some("```")
        } else if trimmed.starts_with("~~~") {
            Some("~~~")
        } else {
            None
        };
        match (fence, marker) {
            (None, Some(open)) => {
                fence = Some(open);
                continue;
            }
            (Some(open), Some(close)) if open == close => {
                fence = None;
                continue;
            }
            (Some(_), _) => continue,
            (None, None) => {}
        }

        let line = strip_inline_code(line);
        if let Some(link) = reference_definition(&line) {
            links.push(link);
            continue;
        }
        inline_links(&line, &mut links);
    }
    links
}

fn strip_inline_code(line: &str) -> String {
    if !line.contains('`') {
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len());
    let mut in_code = false;
    for c in line.chars() {
        if c == '`' {
            in_code = !in_code;
        } else if !in_code {
            out.push(c);
        }
    }
    out
}

/// `[id]: ziel "titel"` → `(id, ziel)`.
fn reference_definition(line: &str) -> Option<(String, String)> {
    let rest = line.trim_start().strip_prefix('[')?;
    let close = rest.find("]:")?;
    let id = &rest[..close];
    if id.is_empty() || id.contains(['[', ']']) || id.starts_with('^') {
        return None;
    }
    let target = rest[close + 2..].split_whitespace().next()?;
    let target = target
        .strip_prefix('<')
        .and_then(|t| t.strip_suffix('>'))
        .unwrap_or(target);
    Some((id.to_owned(), target.to_owned()))
}

fn inline_links(line: &str, links: &mut Vec<(String, String)>) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '[' {
            i += 1;
            continue;
        }
        // Passende schließende Klammer (verschachtelt, z. B. Badges).
        let mut depth = 0usize;
        let mut close = None;
        for (j, &c) in chars.iter().enumerate().skip(i) {
            match c {
                '[' => depth += 1,
                ']' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        close = Some(j);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else { break };
        if chars.get(close + 1) == Some(&'(') {
            if let Some(target) = link_destination(&chars, close + 2) {
                let text: String = chars[i + 1..close].iter().collect();
                links.push((text, target));
            }
        }
        // Weiter im Linktext, damit verschachtelte Links/Bilder gefunden werden.
        i += 1;
    }
}

/// Liest das Ziel eines Inline-Links ab `start` (hinter `(`), bis zur
/// schließenden Klammer; ein optionaler Titel wird verworfen.
fn link_destination(chars: &[char], start: usize) -> Option<String> {
    let mut i = start;
    while chars.get(i).is_some_and(|c| c.is_whitespace()) {
        i += 1;
    }
    let mut target = String::new();
    if chars.get(i) == Some(&'<') {
        i += 1;
        loop {
            match chars.get(i) {
                Some('>') => break,
                Some(&c) => target.push(c),
                None => return None,
            }
            i += 1;
        }
    } else {
        let mut depth = 0usize;
        loop {
            match chars.get(i) {
                Some('(') => {
                    depth += 1;
                    target.push('(');
                }
                Some(')') if depth == 0 => break,
                Some(')') => {
                    depth -= 1;
                    target.push(')');
                }
                Some(c) if c.is_whitespace() => break,
                Some(&c) => target.push(c),
                None => return None,
            }
            i += 1;
        }
        // Rest bis `)` (Titel) muss geschlossen sein.
        if !chars[i..].contains(&')') {
            return None;
        }
    }
    Some(target)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use std::fs;

    type TestResult = Result<(), Box<dyn Error>>;

    fn write(root: &Path, rel: &str, content: &str) -> TestResult {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)?;
        Ok(())
    }

    /// Baut die Node-Liste per Hand (rekursiv, sortiert).
    fn nodes_of(root: &Path) -> Result<Vec<Node>, Box<dyn Error>> {
        fn visit(root: &Path, dir: &Path, out: &mut Vec<Node>) -> Result<(), Box<dyn Error>> {
            for entry in fs::read_dir(dir)? {
                let path = entry?.path();
                let rel = path.strip_prefix(root)?.to_path_buf();
                let depth = u16::try_from(rel.components().count())?;
                if path.is_dir() {
                    out.push(Node {
                        path: rel,
                        kind: FileKind::Dir,
                        size: 0,
                        depth,
                        ignored: false,
                    });
                    visit(root, &path, out)?;
                } else {
                    let kind = if path.extension().is_some_and(|e| e == "md") {
                        FileKind::Markdown
                    } else {
                        FileKind::Config
                    };
                    let size = fs::metadata(&path)?.len();
                    out.push(Node {
                        path: rel,
                        kind,
                        size,
                        depth,
                        ignored: false,
                    });
                }
            }
            Ok(())
        }
        let mut out = Vec::new();
        visit(root, root, &mut out)?;
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn project(
        root: &str,
        kind: ProjectKind,
        name: &str,
        members: &[&str],
        manifest: Option<&str>,
    ) -> Project {
        Project {
            root: PathBuf::from(root),
            kind,
            name: name.to_owned(),
            members: members.iter().map(PathBuf::from).collect(),
            manifest: manifest.map(PathBuf::from),
        }
    }

    fn has(
        relations: &[Relation],
        from: &str,
        to: &str,
        kind: RelationKind,
        label: Option<&str>,
    ) -> bool {
        relations.iter().any(|r| {
            r.from == Path::new(from)
                && r.to == Path::new(to)
                && r.kind == kind
                && r.label.as_deref() == label
        })
    }

    #[test]
    fn normalizes_lexically() {
        assert_eq!(
            normalize_relative(Path::new("a/./b/../c")),
            Some(PathBuf::from("a/c"))
        );
        assert_eq!(normalize_relative(Path::new("a/../..")), None);
        assert_eq!(normalize_relative(Path::new("a/..")), Some(PathBuf::new()));
        assert_eq!(normalize_relative(Path::new("/abs")), None);
        assert_eq!(
            resolve(Path::new("/r"), Path::new("x/y"), "../../z"),
            Some(PathBuf::from("z"))
        );
        assert_eq!(resolve(Path::new("/r"), Path::new("x"), "../../z"), None);
        assert_eq!(
            resolve(Path::new("/r"), Path::new("x"), "/r/q/../w"),
            Some(PathBuf::from("w"))
        );
        assert_eq!(resolve(Path::new("/r"), Path::new("x"), "/other"), None);
    }

    #[test]
    fn cargo_workspace_members_deps_and_nesting() -> TestResult {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        write(
            root,
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\n[workspace.dependencies]\nbeta = { path = \"crates/beta\" }\n",
        )?;
        write(
            root,
            "crates/alpha/Cargo.toml",
            "[package]\nname = \"alpha\"\n[dependencies]\nbeta = { workspace = true }\nserde = \"1\"\n\
             [dev-dependencies]\ngamma-renamed = { path = \"../../tools/gamma\", package = \"gamma\" }\n\
             [target.'cfg(unix)'.build-dependencies]\nout = { path = \"../../../outside\" }\n",
        )?;
        write(
            root,
            "crates/beta/Cargo.toml",
            "[package]\nname = \"beta\"\n[dependencies]\nalpha = \"0.1\"\n",
        )?;
        write(
            root,
            "tools/gamma/Cargo.toml",
            "[package]\nname = \"gamma\"\n",
        )?;
        write(root, "tools/gamma/sub/Cargo.toml", "this is = = not toml")?;

        let projects = vec![
            project(
                "",
                ProjectKind::CargoWorkspace,
                "root",
                &["crates/alpha", "crates/beta"],
                Some("Cargo.toml"),
            ),
            project("", ProjectKind::Git, "repo", &[], None),
            project(
                "crates/alpha",
                ProjectKind::CargoCrate,
                "alpha",
                &[],
                Some("crates/alpha/Cargo.toml"),
            ),
            project(
                "crates/beta",
                ProjectKind::CargoCrate,
                "beta",
                &[],
                Some("crates/beta/Cargo.toml"),
            ),
            project(
                "tools/gamma",
                ProjectKind::CargoCrate,
                "gamma",
                &[],
                Some("tools/gamma/Cargo.toml"),
            ),
            project("tools/gamma/sub", ProjectKind::CargoCrate, "sub", &[], None),
        ];
        let nodes = nodes_of(root)?;
        let rel = find_relations(root, &projects, &nodes);

        assert!(has(
            &rel,
            "",
            "crates/alpha",
            RelationKind::WorkspaceMember,
            Some("alpha")
        ));
        assert!(has(
            &rel,
            "",
            "crates/beta",
            RelationKind::WorkspaceMember,
            Some("beta")
        ));
        assert!(has(
            &rel,
            "",
            "crates/beta",
            RelationKind::PathDependency,
            Some("beta")
        ));
        assert!(has(
            &rel,
            "crates/alpha",
            "crates/beta",
            RelationKind::CrateDependency,
            Some("beta")
        ));
        assert!(has(
            &rel,
            "crates/beta",
            "crates/alpha",
            RelationKind::CrateDependency,
            Some("alpha")
        ));
        assert!(has(
            &rel,
            "crates/alpha",
            "tools/gamma",
            RelationKind::PathDependency,
            Some("gamma-renamed")
        ));
        assert!(has(
            &rel,
            "",
            "tools/gamma",
            RelationKind::NestedProject,
            Some("gamma")
        ));
        assert!(has(
            &rel,
            "tools/gamma",
            "tools/gamma/sub",
            RelationKind::NestedProject,
            Some("sub")
        ));
        // Mitglieder werden nicht zusätzlich als verschachtelt gemeldet.
        assert!(!rel.iter().any(|r| r.kind == RelationKind::NestedProject && r.to == Path::new("crates/alpha")));
        // Externe/fremde Abhängigkeiten und Pfade außerhalb der Wurzel fehlen.
        assert!(
            !rel.iter()
                .any(|r| r.label.as_deref() == Some("serde") || r.label.as_deref() == Some("out"))
        );

        let mut sorted = rel.clone();
        sorted.sort_by(|a, b| {
            (&a.from, &a.to, kind_rank(a.kind)).cmp(&(&b.from, &b.to, kind_rank(b.kind)))
        });
        assert_eq!(rel, sorted);
        Ok(())
    }

    #[test]
    fn npm_and_python_dependencies() -> TestResult {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        write(
            root,
            "web/package.json",
            r#"{"name":"web","dependencies":{"ui":"file:../libs/ui","shared":"workspace:*","left-pad":"^1"},
                "devDependencies":{"tools":"link:./tools","wp":"workspace:../libs/ui"}}"#,
        )?;
        write(root, "web/tools/package.json", r#"{"name":"tools"}"#)?;
        write(root, "libs/ui/package.json", r#"{"name":"ui"}"#)?;
        write(root, "libs/shared/package.json", r#"{"name":"shared"}"#)?;
        write(root, "broken/package.json", "{not json")?;
        write(
            root,
            "py/app/pyproject.toml",
            "[tool.poetry.dependencies]\ncore = { path = \"../core\", develop = true }\n\
             [tool.uv.sources]\nlib = { path = \"../lib\" }\n",
        )?;
        fs::create_dir_all(root.join("py/core"))?;
        fs::create_dir_all(root.join("py/lib"))?;

        let projects = vec![
            project(
                "web",
                ProjectKind::Node,
                "web",
                &[],
                Some("web/package.json"),
            ),
            project(
                "web/tools",
                ProjectKind::Node,
                "tools",
                &[],
                Some("web/tools/package.json"),
            ),
            project(
                "libs/ui",
                ProjectKind::Node,
                "ui",
                &[],
                Some("libs/ui/package.json"),
            ),
            project(
                "libs/shared",
                ProjectKind::Node,
                "shared",
                &[],
                Some("libs/shared/package.json"),
            ),
            project(
                "broken",
                ProjectKind::Node,
                "broken",
                &[],
                Some("broken/package.json"),
            ),
            project(
                "py/app",
                ProjectKind::Python,
                "app",
                &[],
                Some("py/app/pyproject.toml"),
            ),
        ];
        let nodes = nodes_of(root)?;
        let rel = find_relations(root, &projects, &nodes);

        assert!(has(
            &rel,
            "web",
            "libs/ui",
            RelationKind::PathDependency,
            Some("ui")
        ));
        assert!(has(
            &rel,
            "web",
            "web/tools",
            RelationKind::PathDependency,
            Some("tools")
        ));
        assert!(has(
            &rel,
            "web",
            "libs/shared",
            RelationKind::CrateDependency,
            Some("shared")
        ));
        assert!(has(
            &rel,
            "web",
            "web/tools",
            RelationKind::NestedProject,
            Some("tools")
        ));
        assert!(has(
            &rel,
            "py/app",
            "py/core",
            RelationKind::PathDependency,
            Some("core")
        ));
        assert!(has(
            &rel,
            "py/app",
            "py/lib",
            RelationKind::PathDependency,
            Some("lib")
        ));
        assert!(!rel.iter().any(|r| r.label.as_deref() == Some("left-pad")));
        Ok(())
    }

    #[test]
    fn markdown_doc_links() -> TestResult {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        let long = "x".repeat(120);
        write(
            root,
            "docs/guide.md",
            &format!(
                "# Guide\nSee [the API](api.md#section) and [Readme](../README.md?plain=1).\n\
                 ![logo](img/logo%20big.png \"Logo\") [{long}](./api.md)\n\
                 [web](https://example.com) [mail](mailto:a@b.c) [anchor](#top) [missing](nope.md)\n\
                 [escape](../../etc/passwd) [root](/src) `[code](api.md)`\n\
                 ```\n[fenced](other.md)\n```\n[ref]: <other.md>\n"
            ),
        )?;
        write(root, "docs/api.md", "nothing")?;
        write(root, "docs/other.md", "nothing")?;
        write(root, "docs/img/logo big.png", "png")?;
        write(root, "README.md", "[docs](docs/)")?;
        fs::create_dir_all(root.join("src"))?;

        let nodes = nodes_of(root)?;
        let rel = find_relations(root, &[], &nodes);
        let links: Vec<&Relation> = rel
            .iter()
            .filter(|r| r.kind == RelationKind::DocLink)
            .collect();

        assert!(has(
            &rel,
            "docs/guide.md",
            "docs/api.md",
            RelationKind::DocLink,
            Some("the API")
        ));
        assert!(has(
            &rel,
            "docs/guide.md",
            "README.md",
            RelationKind::DocLink,
            Some("Readme")
        ));
        assert!(has(
            &rel,
            "docs/guide.md",
            "docs/img/logo big.png",
            RelationKind::DocLink,
            Some("logo")
        ));
        assert!(has(
            &rel,
            "docs/guide.md",
            "src",
            RelationKind::DocLink,
            Some("root")
        ));
        assert!(has(
            &rel,
            "docs/guide.md",
            "docs/other.md",
            RelationKind::DocLink,
            Some("ref")
        ));
        assert!(has(
            &rel,
            "README.md",
            "docs",
            RelationKind::DocLink,
            Some("docs")
        ));
        assert_eq!(links.len(), 6, "{links:#?}");
        assert!(links.iter().all(|r| {
            r.label
                .as_ref()
                .is_none_or(|l| l.chars().count() <= MAX_LABEL_CHARS)
        }));
        Ok(())
    }

    #[test]
    fn doc_links_are_capped() -> TestResult {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        let mut body = String::new();
        for i in 0..(MAX_DOC_LINKS + 50) {
            write(root, &format!("t/{i}.txt"), "")?;
            body.push_str(&format!("[l{i}](t/{i}.txt)\n"));
        }
        write(root, "index.md", &body)?;
        let nodes = nodes_of(root)?;
        let rel = find_relations(root, &[], &nodes);
        assert_eq!(
            rel.iter()
                .filter(|r| r.kind == RelationKind::DocLink)
                .count(),
            MAX_DOC_LINKS
        );
        Ok(())
    }

    #[test]
    fn extracts_nested_badge_links() {
        let links = extract_links("[![badge](img.svg)](target.md)");
        let targets: Vec<&str> = links.iter().map(|(_, t)| t.as_str()).collect();
        assert!(targets.contains(&"target.md"));
        assert!(targets.contains(&"img.svg"));
    }
}
