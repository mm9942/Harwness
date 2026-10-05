//! Arbeitsbereichs-Kontext für die Wurzelsitzung: kompakter Repository-Überblick.
//!
//! # Beschreibung
//! [`RepoTreeContextProvider`] liefert genau ein Fragment mit dem Label
//! [`REPO_TREE_LABEL`] (`"repo.tree"`). Grundlage ist ein
//! [`harw_explorer::ExplorerIndex`] der Arbeitsbereichs-Wurzel:
//!
//! 1. ein kompakter Baum (Tiefe ≤ [`TREE_MAX_DEPTH`], höchstens
//!    [`TREE_MAX_LINES`] Zeilen, Verzeichnisse vor Dateien, von
//!    `.gitignore`/`.ignore`/versteckt ausgeschlossene Pfade übersprungen —
//!    das ist die Vorgabe von [`harw_explorer::ExplorerOptions`] mit
//!    `include_ignored = false`),
//! 2. die erkannten Projekte (Art, Name, Wurzel),
//! 3. die wichtigsten Relationen zwischen ihnen (Workspace-Mitgliedschaft und
//!    Pfad-Abhängigkeiten zuerst, Dokument-Links zuletzt).
//!
//! # Zwischenspeicher
//! Der gerenderte Überblick wird je Provider (= je Wurzel) zwischengespeichert.
//! Frühestens nach [`REPO_TREE_REFRESH_INTERVAL`] prüft der Provider einen
//! billigen Fingerabdruck der Wurzel (Anzahl und jüngste Änderungszeit der
//! Wurzel selbst und ihrer direkten Einträge); nur wenn sich dieser geändert
//! hat, wird der Index neu gebaut. Neue oder entfernte Dateien direkt in einem
//! Verzeichnis der ersten Ebene (z. B. ein neues Crate unter `crates/`) ändern
//! dessen `mtime` und werden damit erkannt; reine Inhaltsänderungen tief im
//! Baum nicht — für einen Strukturüberblick ist das gewollt.
//!
//! # Vertrauensklasse
//! Wie alle anderen aus dem Arbeitsbereich abgeleiteten Fragmente (Handoff,
//! Gedächtnis-Fakten) behauptet dieser Provider nichts Besonderes:
//! [`ContextProvider::max_trust`] bleibt bei der Vorgabe
//! [`harw_context::TrustClass::Data`], und `contribute_v2` fällt auf die
//! v1-Brücke zurück. Datei- und Ordnernamen stammen aus dem Arbeitsbereich und
//! sind damit Daten, keine Anweisungen.
//!
//! # Aufgaben-Kontext (`task.objective`/`task.read_scope`)
//! Ein zweiter Provider für das Ziel der anstehenden Aufgabe ist hier bewusst
//! **nicht** implementiert: [`TurnInputContext`] trägt nur `session_id`,
//! `turn_id` und `metadata`, und `metadata` ist an allen produktiven
//! Aufrufstellen (`harw-core/src/turn_loop.rs`) heute `Null`. Die anstehende
//! Aufgabe ist über die Provider-Schnittstelle damit nicht erreichbar.

use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use harw_explorer::{ExplorerIndex, ExplorerOptions, Project, Relation, RelationKind};
use harw_extension_api::{ContextFragment, ContextProvider, ExtFuture, TurnInputContext};

/// Label des Repository-Überblick-Fragments.
pub const REPO_TREE_LABEL: &str = "repo.tree";

/// Namensraum, unter dem [`RepoTreeContextProvider`] registriert wird.
pub const REPO_TREE_NAMESPACE: &str = harw_context::sources::repo_tree.namespace;

/// Mindestabstand zwischen zwei Fingerabdruck-Prüfungen der Wurzel.
pub const REPO_TREE_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Maximale Baumtiefe im gerenderten Überblick (direkte Kinder = 1).
pub const TREE_MAX_DEPTH: usize = 3;

/// Maximale Zeilenzahl des gerenderten Baums (inklusive der
/// `… (N weitere)`-Schlusszeile).
pub const TREE_MAX_LINES: usize = 200;

/// Maximale Tiefe des zugrunde liegenden Index. Tiefer als der gerenderte
/// Baum, damit verschachtelte Projekte (z. B. `apps/web/package.json`) und
/// ihre Relationen trotzdem erkannt werden.
const INDEX_MAX_DEPTH: usize = 8;

/// Harte Obergrenze an Einträgen des zugrunde liegenden Index.
const INDEX_MAX_NODES: usize = 20_000;

/// Höchstzahl gelisteter Projekte.
const PROJECTS_MAX: usize = 40;

/// Höchstzahl gelisteter Relationen.
const RELATIONS_MAX: usize = 30;

/// Billiger Fingerabdruck einer Wurzel: Anzahl direkter Einträge und jüngste
/// Änderungszeit der Wurzel selbst und ihrer direkten Einträge.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RootFingerprint {
    entries: usize,
    newest: Option<SystemTime>,
}

/// Zwischengespeicherter Überblick samt Prüfzeitpunkt.
#[derive(Debug)]
struct CachedTree {
    /// Gerenderter Fragmentinhalt; `None`, wenn der Index nicht gebaut werden
    /// konnte (dann wird bis zur nächsten Prüfung nichts geliefert).
    content: Option<String>,
    /// Fingerabdruck zum Zeitpunkt des letzten Baus; `None`, wenn die Wurzel
    /// nicht lesbar war.
    fingerprint: Option<RootFingerprint>,
    /// Zeitpunkt der letzten Fingerabdruck-Prüfung (oder des letzten Baus).
    checked_at: Instant,
}

/// Kontext-Anbieter für einen kompakten Überblick des Arbeitsbereichs
/// (Label [`REPO_TREE_LABEL`]).
///
/// # Nebenläufigkeit
/// `Send + Sync`: der Zwischenspeicher liegt hinter einem [`Mutex`]; ein
/// vergifteter Mutex wird übernommen statt zu paniken (der Inhalt ist nur ein
/// Zwischenspeicher und wird bei Bedarf neu gebaut). Der Index wird synchron
/// innerhalb des Futures gebaut — wie bei den übrigen dateibasierten
/// Providern dieses Crates; der Zwischenspeicher begrenzt das auf höchstens
/// einen Durchlauf je [`REPO_TREE_REFRESH_INTERVAL`] und Strukturänderung.
///
/// # Fehler
/// Kein eigener Fehlertyp: eine ungültige Wurzel oder ein E/A-Fehler wird nur
/// `tracing::warn!`, der Provider liefert dann kein Fragment.
#[derive(Debug)]
pub struct RepoTreeContextProvider {
    root: PathBuf,
    refresh_interval: Duration,
    cache: Mutex<Option<CachedTree>>,
}

impl RepoTreeContextProvider {
    /// Baut einen Provider für die Arbeitsbereichs-Wurzel `root` mit dem
    /// Standard-Prüfabstand [`REPO_TREE_REFRESH_INTERVAL`].
    ///
    /// Der Index wird erst beim ersten [`ContextProvider::contribute`]
    /// gebaut, nicht hier.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self::with_refresh_interval(root, REPO_TREE_REFRESH_INTERVAL)
    }

    /// Wie [`Self::new`], aber mit frei wählbarem Prüfabstand
    /// (`Duration::ZERO` = bei jedem Aufruf Fingerabdruck prüfen).
    #[must_use]
    pub fn with_refresh_interval(root: PathBuf, refresh_interval: Duration) -> Self {
        Self {
            root,
            refresh_interval,
            cache: Mutex::new(None),
        }
    }

    /// Die Wurzel, die dieser Provider beschreibt.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Liefert den (ggf. aus dem Zwischenspeicher stammenden) Überblick.
    ///
    /// # Beschreibung
    /// Ist der Zwischenspeicher jünger als der Prüfabstand, wird er
    /// unverändert geliefert. Sonst wird der Fingerabdruck der Wurzel
    /// geprüft; bei Gleichheit wird nur der Prüfzeitpunkt erneuert, bei
    /// Abweichung der Index neu gebaut.
    fn overview(&self) -> Option<String> {
        let mut guard = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();

        if let Some(cached) = guard.as_mut() {
            if now.duration_since(cached.checked_at) < self.refresh_interval {
                return cached.content.clone();
            }
            let fingerprint = root_fingerprint(&self.root);
            if fingerprint.is_some() && fingerprint == cached.fingerprint {
                cached.checked_at = now;
                return cached.content.clone();
            }
        }

        let fingerprint = root_fingerprint(&self.root);
        let content = build_overview(&self.root);
        *guard = Some(CachedTree {
            content: content.clone(),
            fingerprint,
            checked_at: now,
        });
        content
    }
}

impl ContextProvider for RepoTreeContextProvider {
    /// Liefert genau ein Fragment [`REPO_TREE_LABEL`], oder keines, wenn die
    /// Wurzel nicht indexiert werden konnte.
    fn contribute<'a>(&'a self, _ctx: &'a TurnInputContext) -> ExtFuture<'a, Vec<ContextFragment>> {
        Box::pin(async move {
            match self.overview() {
                Some(content) => vec![ContextFragment {
                    label: REPO_TREE_LABEL.to_owned(),
                    content,
                }],
                None => Vec::new(),
            }
        })
    }

    fn namespace(&self) -> &'static str {
        REPO_TREE_NAMESPACE
    }
}

/// Fingerabdruck der Wurzel, oder `None`, wenn sie nicht lesbar ist.
fn root_fingerprint(root: &Path) -> Option<RootFingerprint> {
    let mut newest = std::fs::metadata(root).and_then(|m| m.modified()).ok();
    let mut entries = 0usize;
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        entries += 1;
        if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
            newest = Some(newest.map_or(modified, |current| current.max(modified)));
        }
    }
    Some(RootFingerprint { entries, newest })
}

/// Baut den Index für `root` und rendert den Überblick.
fn build_overview(root: &Path) -> Option<String> {
    let options = ExplorerOptions {
        include_ignored: false,
        max_nodes: INDEX_MAX_NODES,
        max_depth: INDEX_MAX_DEPTH,
    };
    match ExplorerIndex::build(root, &options) {
        Ok(index) => Some(render_overview(&index)),
        Err(error) => {
            tracing::warn!(
                root = %root.display(),
                error = %error,
                "runtime.repo_tree.index_failed"
            );
            None
        }
    }
}

/// Rendert Baum, Projekte und Relationen eines Index.
fn render_overview(index: &ExplorerIndex) -> String {
    let mut out = format!(
        "Repository-Überblick (automatisch, Tiefe ≤ {TREE_MAX_DEPTH}): {}\n",
        index.summary()
    );

    // `tree` hängt nach `max_entries` Zeilen eine `… (N weitere)`-Zeile an;
    // eine Zeile Reserve hält das Ergebnis bei höchstens `TREE_MAX_LINES`.
    out.push_str(&index.tree(
        Path::new(""),
        TREE_MAX_DEPTH,
        TREE_MAX_LINES.saturating_sub(1),
    ));
    out.push('\n');
    if index.truncated {
        out.push_str("(Index gekürzt — sehr großer Arbeitsbereich)\n");
    }

    if !index.projects.is_empty() {
        out.push_str("\nProjekte:\n");
        for project in index.projects.iter().take(PROJECTS_MAX) {
            out.push_str(&project_line(project));
        }
        push_rest(&mut out, index.projects.len(), PROJECTS_MAX);
    }

    if !index.relations.is_empty() {
        let mut relations: Vec<&Relation> = index.relations.iter().collect();
        // Stabil: innerhalb einer Art bleibt die deterministische
        // Index-Reihenfolge erhalten.
        relations.sort_by_key(|relation| relation_rank(relation.kind));
        out.push_str("\nRelationen:\n");
        for relation in relations.iter().take(RELATIONS_MAX) {
            out.push_str(&relation_line(relation));
        }
        push_rest(&mut out, relations.len(), RELATIONS_MAX);
    }

    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Eine Projektzeile: `- <art> <name> @ <wurzel>`.
fn project_line(project: &Project) -> String {
    format!(
        "- {} {} @ {}\n",
        project.kind.label(),
        project.name,
        display_rel(&project.root)
    )
}

/// Eine Relationszeile: `- <von> → <nach> (<art>[: <label>])`.
fn relation_line(relation: &Relation) -> String {
    match relation.label.as_deref() {
        Some(label) if !label.trim().is_empty() => format!(
            "- {} → {} ({}: {})\n",
            display_rel(&relation.from),
            display_rel(&relation.to),
            relation.kind.label(),
            label.trim()
        ),
        _ => format!(
            "- {} → {} ({})\n",
            display_rel(&relation.from),
            display_rel(&relation.to),
            relation.kind.label()
        ),
    }
}

/// Hängt `… (N weitere)` an, falls `total` die Grenze `shown` übersteigt.
fn push_rest(out: &mut String, total: usize, shown: usize) {
    if total > shown {
        out.push_str(&format!("… ({} weitere)\n", total - shown));
    }
}

/// Reihenfolge der Relationsarten im Überblick (klein = wichtiger).
fn relation_rank(kind: RelationKind) -> u8 {
    match kind {
        RelationKind::WorkspaceMember => 0,
        RelationKind::PathDependency => 1,
        RelationKind::CrateDependency => 2,
        RelationKind::NestedProject => 3,
        RelationKind::DocLink => 4,
    }
}

/// Relativer Pfad mit `/` als Trenner; die Wurzel selbst erscheint als `.`.
fn display_rel(path: &Path) -> String {
    let parts: Vec<String> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    /// Treibt eine `ExtFuture` synchron zu Ende (wie in `handoff.rs`); die
    /// Zukunft dieses Providers hat kein inneres `.await` und ist beim ersten
    /// Poll fertig.
    fn block_on<T>(mut fut: ExtFuture<'_, T>) -> T {
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        loop {
            if let std::task::Poll::Ready(value) = fut.as_mut().poll(&mut cx) {
                return value;
            }
        }
    }

    /// Temp-Arbeitsbereich: Cargo-Workspace mit einem Mitglied, ein
    /// npm-Paket, eine PDF-Datei und ein per `.gitignore` ausgeschlossener
    /// Ordner.
    fn fixture() -> TestResult<tempfile::TempDir> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/core\"]\nresolver = \"2\"\n",
        )?;
        std::fs::create_dir_all(root.join("crates/core/src"))?;
        std::fs::write(
            root.join("crates/core/Cargo.toml"),
            "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )?;
        std::fs::write(root.join("crates/core/src/lib.rs"), "pub fn f() {}\n")?;
        std::fs::create_dir_all(root.join("web"))?;
        std::fs::write(
            root.join("web/package.json"),
            "{\"name\": \"web-app\", \"version\": \"1.0.0\"}\n",
        )?;
        std::fs::write(root.join("web/index.js"), "console.log('hi');\n")?;
        std::fs::create_dir_all(root.join("docs"))?;
        std::fs::write(root.join("docs/manual.pdf"), b"%PDF-1.4\n%fixture\n")?;
        std::fs::write(root.join(".gitignore"), "secret_out/\n")?;
        std::fs::create_dir_all(root.join("secret_out"))?;
        std::fs::write(root.join("secret_out/leak.txt"), "do not show\n")?;
        Ok(dir)
    }

    fn content_of(fragments: &[ContextFragment]) -> TestResult<&str> {
        let [fragment] = fragments else {
            return Err(TestError::Unexpected(format!(
                "genau ein Fragment erwartet, erhalten: {}",
                fragments.len()
            )));
        };
        if fragment.label != REPO_TREE_LABEL {
            return Err(TestError::Unexpected(format!(
                "falsches Label: {}",
                fragment.label
            )));
        }
        Ok(fragment.content.as_str())
    }

    #[test]
    fn repo_tree_lists_tree_projects_and_relations() -> TestResult {
        let dir = fixture()?;
        let provider = RepoTreeContextProvider::new(dir.path().to_path_buf());

        let fragments = block_on(provider.contribute(&TurnInputContext::default()));
        let content = content_of(&fragments)?;

        assert!(content.starts_with("Repository-Überblick"), "{content}");
        // Baum: Verzeichnisse und Dateien, PDF als solche klassifiziert.
        assert!(content.contains("crates/"), "{content}");
        assert!(content.contains("web/"), "{content}");
        assert!(content.contains("manual.pdf  (pdf"), "{content}");
        assert!(content.contains("package.json"), "{content}");
        // Ignorierte Pfade tauchen nicht auf.
        assert!(!content.contains("secret_out"), "{content}");
        assert!(!content.contains("leak.txt"), "{content}");
        // Projekte.
        assert!(content.contains("\nProjekte:\n"), "{content}");
        assert!(content.contains("- cargo-workspace "), "{content}");
        assert!(
            content.contains("- cargo-crate core-lib @ crates/core"),
            "{content}"
        );
        assert!(content.contains("- node web-app @ web"), "{content}");
        // Relationen: Workspace-Mitgliedschaft steht vorn.
        assert!(content.contains("\nRelationen:\n"), "{content}");
        assert!(content.contains("- . → crates/core (member"), "{content}");
        Ok(())
    }

    #[test]
    fn repo_tree_directories_come_before_files() -> TestResult {
        let dir = fixture()?;
        let provider = RepoTreeContextProvider::new(dir.path().to_path_buf());

        let fragments = block_on(provider.contribute(&TurnInputContext::default()));
        let content = content_of(&fragments)?;

        let web_dir = content
            .find("\nweb/")
            .ok_or(TestError::Missing("web/ auf oberster Ebene"))?;
        let cargo_file = content
            .find("\nCargo.toml")
            .ok_or(TestError::Missing("Cargo.toml auf oberster Ebene"))?;
        assert!(web_dir < cargo_file, "{content}");
        Ok(())
    }

    #[test]
    fn repo_tree_respects_depth_and_line_limits() -> TestResult {
        let dir = tempfile::tempdir()?;
        let deep = dir.path().join("a/b/c/d/e");
        std::fs::create_dir_all(&deep)?;
        std::fs::write(deep.join("too_deep.txt"), "x")?;
        let many = dir.path().join("many");
        std::fs::create_dir_all(&many)?;
        for i in 0..(TREE_MAX_LINES + 50) {
            std::fs::write(many.join(format!("f{i:04}.txt")), "x")?;
        }
        let provider = RepoTreeContextProvider::new(dir.path().to_path_buf());

        let fragments = block_on(provider.contribute(&TurnInputContext::default()));
        let content = content_of(&fragments)?;

        assert!(!content.contains("too_deep.txt"), "Tiefe > 3 sichtbar");
        assert!(!content.contains("d/"), "Tiefe > 3 sichtbar");
        assert!(content.contains("weitere)"), "Kürzungshinweis fehlt");
        // Überschrift + Baumzeilen; ohne Projekte/Relationen folgt nichts.
        let tree_lines = content.lines().count().saturating_sub(1);
        assert!(tree_lines <= TREE_MAX_LINES, "{tree_lines} Baumzeilen");
        Ok(())
    }

    #[test]
    fn repo_tree_is_cached_within_the_refresh_interval() -> TestResult {
        let dir = fixture()?;
        let provider = RepoTreeContextProvider::with_refresh_interval(
            dir.path().to_path_buf(),
            Duration::from_secs(3600),
        );
        let first = block_on(provider.contribute(&TurnInputContext::default()));
        let first = content_of(&first)?.to_owned();

        std::fs::create_dir_all(dir.path().join("added_later"))?;
        let second = block_on(provider.contribute(&TurnInputContext::default()));
        let second = content_of(&second)?;

        assert_eq!(first, second);
        assert!(!second.contains("added_later"), "{second}");
        Ok(())
    }

    #[test]
    fn repo_tree_refreshes_after_a_structural_change() -> TestResult {
        let dir = fixture()?;
        let provider = RepoTreeContextProvider::with_refresh_interval(
            dir.path().to_path_buf(),
            Duration::ZERO,
        );
        let first = block_on(provider.contribute(&TurnInputContext::default()));
        assert!(!content_of(&first)?.contains("added_later"));

        std::fs::create_dir_all(dir.path().join("added_later"))?;
        let second = block_on(provider.contribute(&TurnInputContext::default()));
        let second = content_of(&second)?;

        assert!(second.contains("added_later/"), "{second}");
        Ok(())
    }

    #[test]
    fn repo_tree_contributes_nothing_for_a_missing_root() -> TestResult {
        let dir = tempfile::tempdir()?;
        let provider = RepoTreeContextProvider::new(dir.path().join("does-not-exist"));

        let fragments = block_on(provider.contribute(&TurnInputContext::default()));

        assert!(fragments.is_empty());
        Ok(())
    }

    #[test]
    fn repo_tree_declares_namespace_and_default_trust() {
        let provider = RepoTreeContextProvider::new(PathBuf::from("."));
        assert_eq!(provider.namespace(), REPO_TREE_NAMESPACE);
        assert_eq!(provider.max_trust(), harw_context::TrustClass::Data);
    }

    #[test]
    fn display_rel_renders_root_as_dot_and_uses_slashes() {
        assert_eq!(display_rel(Path::new("")), ".");
        assert_eq!(display_rel(Path::new("crates/core")), "crates/core");
    }
}
