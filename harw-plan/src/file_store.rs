//! Persistente Datei-Implementierung des `PlanStore`-Traits.
//!
//! Verantwortungsbereich: `FilePlanStore` — schreibt Pläne atomar als
//! `rev-<n>.json` und pflegt eine append-only `history.jsonl` unter
//! `<root>/plans/<plan_id>/`.
//!
//! Atomares Schreiben erfolgt via Tmp-Datei + `std::fs::rename` (Design-Doc §4).
//!
//! Die Mutationslogik selbst liegt **nicht** hier, sondern in
//! `crate::mutation` — dieselbe Funktion, die auch `InMemoryPlanStore`
//! aufruft. Dieses Modul verantwortet ausschließlich Persistenz, Reload,
//! Locking, Revisionsvergabe und Konfigurationsdurchsetzung.
//!
//! Exportierte Typen: [`FilePlanStore`].

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use time::OffsetDateTime;
use tracing::{debug, info};

use crate::actions::{PlanAction, PlanEvent};
use crate::config::PlanToolConfig;
use crate::error::{PlanError, PlanResult};
use crate::ids::{PlanId, RevisionId};
use crate::mutation::apply_mutation;
use crate::store::PlanStore;
use crate::types::Plan;
use crate::validate::validate_with;

/// Interner Zustand des File-Stores.
struct Inner {
    /// Aktueller Planzustand (gecacht im RAM).
    plan: Option<Plan>,
    /// Event-History (gecacht).
    history: Vec<PlanEvent>,
    /// Plan-ID (für Verzeichnis-Aufbau).
    plan_id: Option<PlanId>,
    /// Nächste Revisionsnummer.
    next_revision: RevisionId,
    /// Wurzelverzeichnis.
    root: PathBuf,
}

/// Vollständig synchronisierter, aber noch nicht sichtbarer Snapshot-Write.
struct StagedWrite {
    target: PathBuf,
    temporary: PathBuf,
}

impl StagedWrite {
    /// Veröffentlicht die synchronisierte Temp-Datei erst nach dem dauerhaften
    /// History-Append und synchronisiert danach den Verzeichniseintrag.
    fn commit(self) -> PlanResult<()> {
        if let Err(error) = std::fs::rename(&self.temporary, &self.target) {
            let _ = std::fs::remove_file(&self.temporary);
            return Err(error.into());
        }
        FilePlanStore::sync_parent_directory(&self.target)
    }

    /// Verwirft den noch nicht sichtbaren Snapshot nach einem fehlgeschlagenen
    /// History-Append.
    fn discard(self) {
        let _ = std::fs::remove_file(self.temporary);
    }
}

/// Persistenter `PlanStore`, der Pläne und History im Dateisystem ablegt.
///
/// # Description
/// Schreibt jeden Plan-Zustand als `<root>/plans/<plan_id>/rev-<n>.json`.
/// Events werden append-only in `<root>/plans/<plan_id>/history.jsonl` geschrieben.
/// Atomares Schreiben via Tmp-Datei + `std::fs::rename` verhindert partielle Writes.
///
/// # Concurrency
/// `Send + Sync` durch `RwLock<Inner>`. Lese-Operationen halten Lese-Lock;
/// `apply` hält Schreib-Lock.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::file_store::FilePlanStore;
/// use harw_plan::store::PlanStore;
/// use harw_plan::actions::PlanAction;
/// use harw_plan::ids::PlanId;
///
/// let store = FilePlanStore::new("/tmp/myplans").unwrap();
/// store.apply(PlanAction::Create {
///     plan_id: PlanId::new("p-1"),
///     goal: "Ziel".to_owned(),
/// }, "orchestrator").unwrap();
/// ```
pub struct FilePlanStore {
    inner: RwLock<Inner>,
    config: PlanToolConfig,
}

/// Ergebnis von [`FilePlanStore::load`]: der aus `<root>/plans/` rekonstruierte
/// Store-Zustand als `(Plan, History, Plan-ID, nächste Revision)`.
///
/// - `Option<Plan>`: der neueste vollständige Planstand, falls einer existiert.
/// - `Vec<PlanEvent>`: die zugehörige, gecachte Event-History.
/// - `Option<PlanId>`: die ID des geladenen Plans, falls einer existiert.
/// - `RevisionId`: die nächste zu vergebende Revision.
type LoadedPlanState = (Option<Plan>, Vec<PlanEvent>, Option<PlanId>, RevisionId);

impl FilePlanStore {
    /// Erstellt einen neuen `FilePlanStore` mit dem angegebenen Wurzelverzeichnis.
    ///
    /// # Arguments
    /// - `root` (`impl AsRef<Path>`): Wurzelverzeichnis für alle Plandateien.
    ///
    /// # Errors
    /// - [`PlanError::Io`] wenn das Verzeichnis nicht erstellt werden kann.
    ///
    /// Dieser Kompatibilitätspfad verwendet explizit die aktivierten
    /// Standardwerte. Neue Aufrufer sollen [`Self::with_config`] verwenden,
    /// damit ihre Tool-Konfiguration an der Persistenzgrenze erzwungen wird.
    pub fn new(root: impl AsRef<Path>) -> PlanResult<Self> {
        Self::with_config(root, PlanToolConfig::enabled_defaults())
    }

    /// Erstellt einen neuen `FilePlanStore` und erzwingt die Plan-Tool-Konfiguration.
    ///
    /// Die Konfiguration wird vor dem Anlegen des Wurzelverzeichnisses geprüft.
    /// Ein bereits vorhandener Plan wird ebenfalls vor dem Exponieren des Stores
    /// gegen das Knotenlimit validiert.
    ///
    /// # Errors
    /// - [`PlanError::Config`] wenn die Konfiguration deaktiviert oder ungültig ist.
    /// - [`PlanError::Io`] wenn das Verzeichnis nicht erstellt werden kann oder
    ///   Plandateien nicht lesbar sind.
    pub fn with_config(root: impl AsRef<Path>, config: PlanToolConfig) -> PlanResult<Self> {
        config.require_enabled().map_err(PlanError::Config)?;
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        let (plan, history, plan_id, next_revision) = Self::load(&root)?;
        if let Some(plan) = &plan {
            config.validate_plan(plan).map_err(PlanError::Config)?;
        }
        Ok(Self {
            inner: RwLock::new(Inner {
                plan,
                history,
                plan_id,
                next_revision,
                root,
            }),
            config,
        })
    }

    /// Lädt beim Start den neuesten vollständigen Planstand aus dem Dateibaum.
    ///
    /// Die Dateinamen sind die durable Sequenzquelle. Der Snapshot wird erst
    /// nach dem dauerhaft angehängten History-Event sichtbar; falls mehrere
    /// Plan-IDs vorhanden sind, wird deterministisch der höchste
    /// Revisionsstand gewählt.
    fn load(root: &Path) -> PlanResult<LoadedPlanState> {
        let plans_root = root.join("plans");
        if !plans_root.exists() {
            return Ok((None, Vec::new(), None, RevisionId::new(1)));
        }

        let mut newest: Option<(RevisionId, Plan)> = None;
        for entry in std::fs::read_dir(&plans_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            for file in std::fs::read_dir(entry.path())? {
                let file = file?;
                if !file.file_type()?.is_file() {
                    continue;
                }
                let Some(name) = file.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let Some(number) = name
                    .strip_prefix("rev-")
                    .and_then(|n| n.strip_suffix(".json"))
                    .and_then(|n| n.parse::<u64>().ok())
                else {
                    continue;
                };
                let revision = RevisionId::new(number);
                let plan: Plan = serde_json::from_reader(std::fs::File::open(file.path())?)?;
                if newest.as_ref().is_none_or(|(current, current_plan)| {
                    revision > *current
                        || (revision == *current && plan.id.as_str() > current_plan.id.as_str())
                }) {
                    newest = Some((revision, plan));
                }
            }
        }

        let Some((file_revision, plan)) = newest else {
            return Ok((None, Vec::new(), None, RevisionId::new(1)));
        };
        let plan_id = plan.id.clone();
        let history_path = Self::history_path(root, &plan_id);
        let history = if history_path.exists() {
            let reader = BufReader::new(std::fs::File::open(history_path)?);
            reader
                .lines()
                .filter_map(|line| match line {
                    Ok(line) if line.trim().is_empty() => None,
                    Ok(line) => {
                        Some(serde_json::from_str::<PlanEvent>(&line).map_err(PlanError::Serde))
                    }
                    Err(error) => Some(Err(PlanError::Io(error))),
                })
                .collect::<PlanResult<Vec<_>>>()?
        } else {
            Vec::new()
        };
        let history_revision = history
            .iter()
            .map(|event| event.revision)
            .max()
            .unwrap_or(RevisionId::new(0));
        let next = file_revision
            .max(plan.revision)
            .max(history_revision)
            .next();
        Ok((Some(plan), history, Some(plan_id), next))
    }

    /// Gibt den Planspeicherpfad für eine gegebene Revision zurück.
    fn plan_path(root: &Path, plan_id: &PlanId, revision: RevisionId) -> PathBuf {
        root.join("plans")
            .join(plan_id.as_str())
            .join(format!("rev-{}.json", revision.value()))
    }

    /// Gibt den History-Pfad zurück.
    fn history_path(root: &Path, plan_id: &PlanId) -> PathBuf {
        root.join("plans")
            .join(plan_id.as_str())
            .join("history.jsonl")
    }

    /// Bereitet `content` als synchronisierte Temp-Datei vor, ohne den
    /// Zielpfad schon sichtbar zu verändern.
    ///
    /// # Errors
    /// - [`PlanError::Io`] bei Schreibfehler.
    fn stage_atomic_write(path: &Path, content: &[u8]) -> PlanResult<StagedWrite> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Tmp-Datei im gleichen Verzeichnis (rename atomar nur innerhalb desselben FS).
        // create_new verhindert, dass ein stale Tempfile eines anderen Prozesses
        // stillschweigend überschrieben wird.
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let tmp_path = path.with_extension(format!("tmp-{}-{}", std::process::id(), nonce));
        let write_result = (|| -> PlanResult<()> {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)?;
            f.write_all(content)?;
            f.flush()?;
            f.sync_all()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(error);
        }
        Ok(StagedWrite {
            target: path.to_path_buf(),
            temporary: tmp_path,
        })
    }

    /// Synchronisiert das Verzeichnis eines Dateieintrags, damit ein zuvor
    /// synchronisiertes Rename bzw. Create nach einem Systemabsturz erhalten
    /// bleibt, soweit die Plattform Directory-Fsync unterstützt.
    fn sync_parent_directory(path: &Path) -> PlanResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }

    /// Hängt ein Event an die `history.jsonl` an.
    ///
    /// # Errors
    /// - [`PlanError::Io`] / [`PlanError::Serde`] bei Schreibfehler.
    fn append_event(root: &Path, plan_id: &PlanId, event: &PlanEvent) -> PlanResult<()> {
        let path = Self::history_path(root, plan_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let line = serde_json::to_string(event)?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let original_len = f.metadata()?.len();
        let append_result = (|| -> PlanResult<()> {
            writeln!(f, "{}", line)?;
            f.flush()?;
            f.sync_all()?;
            Self::sync_parent_directory(&path)?;
            Ok(())
        })();
        if let Err(error) = append_result {
            // A failed write may have appended a partial line. Restore the
            // pre-append length before exposing the error to the caller.
            if f.set_len(original_len).is_ok() {
                let _ = f.sync_all();
                let _ = Self::sync_parent_directory(&path);
            }
            return Err(error);
        }
        Ok(())
    }
}

impl PlanStore for FilePlanStore {
    fn current(&self) -> PlanResult<Plan> {
        let inner = self
            .inner
            .read()
            .map_err(|_| PlanError::Io(std::io::Error::other("RwLock vergiftet")))?;
        inner.plan.clone().ok_or(PlanError::PlanNotFound)
    }

    fn revision(&self) -> RevisionId {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        match &inner.plan {
            Some(p) => p.revision,
            None => RevisionId::new(0),
        }
    }

    fn apply(&self, action: PlanAction, actor: &str) -> PlanResult<PlanEvent> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| PlanError::Io(std::io::Error::other("RwLock vergiftet")))?;

        let current_node_count = inner.plan.as_ref().map_or(0, |plan| plan.nodes.len());
        self.config
            .validate_action(&action, current_node_count)
            .map_err(PlanError::Config)?;

        let now = OffsetDateTime::now_utc();

        // `Create` legt an, es überschreibt nicht — auch nicht den beim Start
        // aus `<root>/plans/` geladenen Plan.
        if let PlanAction::Create { plan_id, goal } = &action {
            if let Some(existing) = inner.plan.as_ref() {
                return Err(PlanError::PlanExists {
                    id: existing.id.clone(),
                });
            }

            let plan_id = plan_id.clone();
            let goal = goal.clone();
            info!(plan_id = %plan_id, "Persistenten Plan erstellen");
            let revision = inner.next_revision;
            let plan = Plan {
                id: plan_id.clone(),
                revision,
                parent_revision: None,
                goal_statement: goal,
                goal_id: None,
                nodes: Vec::new(),
                created_at: now,
                updated_at: now,
            };
            let bytes = serde_json::to_vec_pretty(&plan)?;
            let event = PlanEvent {
                revision,
                action,
                actor: actor.to_owned(),
                applied_at: now,
            };
            let plan_path = Self::plan_path(&inner.root, &plan_id, revision);
            let staged_snapshot = Self::stage_atomic_write(&plan_path, &bytes)?;
            if let Err(error) = Self::append_event(&inner.root, &plan_id, &event) {
                staged_snapshot.discard();
                return Err(error);
            }
            staged_snapshot.commit()?;
            inner.next_revision = revision.next();
            inner.plan = Some(plan);
            inner.plan_id = Some(plan_id);
            inner.history.push(event.clone());
            return Ok(event);
        }

        let plan = inner.plan.as_ref().ok_or(PlanError::PlanNotFound)?;

        // Validierung
        validate_with(plan, &action, &self.config, now)?;

        let revision = inner.next_revision;
        // Mutation auf einem Kandidaten — einzige Mutationsstelle, geteilt mit
        // `InMemoryPlanStore`. Der Kandidat wird erst nach dem dauerhaften
        // History-Append sichtbar.
        let mut candidate = plan.clone();
        apply_mutation(&mut candidate, &action, actor, now);
        candidate.updated_at = now;
        candidate.revision = revision;

        // Atomar schreiben
        let plan_id = inner
            .plan_id
            .as_ref()
            .ok_or(PlanError::PlanNotFound)?
            .clone();
        let bytes = serde_json::to_vec_pretty(&candidate)?;
        let plan_path = Self::plan_path(&inner.root, &plan_id, revision);
        let staged_snapshot = Self::stage_atomic_write(&plan_path, &bytes)?;

        let event = PlanEvent {
            revision,
            action,
            actor: actor.to_owned(),
            applied_at: now,
        };
        if let Err(error) = Self::append_event(&inner.root, &plan_id, &event) {
            staged_snapshot.discard();
            return Err(error);
        }
        staged_snapshot.commit()?;
        debug!(revision = %revision, actor = actor, "Persistente Aktion angewendet");
        inner.next_revision = revision.next();
        inner.plan = Some(candidate);
        inner.history.push(event.clone());
        Ok(event)
    }

    fn history(&self, since: Option<RevisionId>) -> PlanResult<Vec<PlanEvent>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| PlanError::Io(std::io::Error::other("RwLock vergiftet")))?;

        // Lese von Disk (history.jsonl) falls Plan-ID bekannt
        if let Some(plan_id) = &inner.plan_id {
            let path = Self::history_path(&inner.root, plan_id);
            if !path.exists() {
                return Ok(vec![]);
            }
            let f = std::fs::File::open(&path)?;
            let reader = BufReader::new(f);
            let mut events = Vec::new();
            for line in reader.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let ev: PlanEvent = serde_json::from_str(&line)?;
                if let Some(since_rev) = since
                    && ev.revision < since_rev
                {
                    continue;
                }
                events.push(ev);
            }
            return Ok(events);
        }

        // Fallback: RAM-Cache
        Ok(match since {
            None => inner.history.clone(),
            Some(rev) => inner
                .history
                .iter()
                .filter(|e| e.revision >= rev)
                .cloned()
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::PlanAction;
    use crate::config::{PlanToolConfig, PlanToolConfigError};
    use crate::ids::{PathOrSymbol, PlanId, TaskId};
    use crate::store::PlanStore;
    use crate::types::{PlanNode, PlanNodeKind, PlanNodeStatus};
    use tempfile::TempDir;
    use time::OffsetDateTime;

    fn make_store(dir: &TempDir) -> FilePlanStore {
        FilePlanStore::new(dir.path()).unwrap()
    }

    fn config_with_max_nodes(max_nodes: usize) -> PlanToolConfig {
        PlanToolConfig {
            max_nodes,
            ..PlanToolConfig::enabled_defaults()
        }
    }

    fn make_node(id: &str) -> PlanNode {
        PlanNode {
            id: TaskId::new(id),
            objective: "obj".to_owned(),
            dependencies: vec![],
            input_contracts: vec![],
            output_contracts: vec![],
            read_scope: vec![],
            write_scope: vec![PathOrSymbol::new(format!("src/{}.rs", id))],
            forbidden_scope: vec![],
            acceptance_criteria: vec![],
            invalidation_conditions: vec![],
            status: PlanNodeStatus::Draft,
            evidence: vec![],
            kind: PlanNodeKind::Coding,
            wave: None,
            assignment: None,
            parent: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn test_persist_and_reload_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);

        store
            .apply(
                PlanAction::Create {
                    plan_id: PlanId::new("p-1"),
                    goal: "Persistenz-Ziel".to_owned(),
                },
                "orchestrator",
            )
            .unwrap();
        store
            .apply(
                PlanAction::AddNode {
                    node: make_node("t1"),
                },
                "a",
            )
            .unwrap();

        let plan = store.current().unwrap();
        assert_eq!(plan.nodes.len(), 1);
        assert_eq!(plan.goal_statement, "Persistenz-Ziel");

        // Plan-Datei muss existieren
        let rev_path = dir
            .path()
            .join("plans")
            .join("p-1")
            .join(format!("rev-{}.json", plan.revision.value()));
        assert!(
            rev_path.exists(),
            "rev-Datei existiert nicht: {:?}",
            rev_path
        );

        // Reload via JSON
        let bytes = std::fs::read(&rev_path).unwrap();
        let reloaded: Plan = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(reloaded.id, plan.id);
        assert_eq!(reloaded.nodes.len(), 1);
    }

    #[test]
    fn test_history_append_only() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);

        store
            .apply(
                PlanAction::Create {
                    plan_id: PlanId::new("p-2"),
                    goal: "History".to_owned(),
                },
                "o",
            )
            .unwrap();
        store
            .apply(
                PlanAction::AddNode {
                    node: make_node("t1"),
                },
                "a",
            )
            .unwrap();
        store
            .apply(
                PlanAction::SetStatus {
                    id: TaskId::new("t1"),
                    status: PlanNodeStatus::Ready,
                    reason: None,
                },
                "a",
            )
            .unwrap();

        let hist = store.history(None).unwrap();
        assert!(hist.len() >= 3, "History muss mindestens 3 Einträge haben");

        // History-Datei muss existieren und Zeilen enthalten
        let hist_path = dir.path().join("plans").join("p-2").join("history.jsonl");
        assert!(hist_path.exists(), "history.jsonl fehlt");
        let content = std::fs::read_to_string(&hist_path).unwrap();
        let line_count = content.lines().filter(|l| !l.trim().is_empty()).count();
        assert!(
            line_count >= 3,
            "history.jsonl hat zu wenig Zeilen: {}",
            line_count
        );
    }

    #[test]
    fn test_staged_write_via_tmp_rename() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("test.json");

        FilePlanStore::stage_atomic_write(&target, b"{\"ok\":true}")
            .unwrap()
            .commit()
            .unwrap();
        assert!(target.exists(), "Zieldatei fehlt nach staged write");

        // Keine staging-Datei darf nach dem Commit zurückbleiben.
        let leftovers = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(leftovers, 0, "Tmp-Datei wurde nicht entfernt");

        // Inhalt korrekt
        let content = std::fs::read(&target).unwrap();
        assert_eq!(content, b"{\"ok\":true}");
    }

    #[test]
    fn test_restart_loads_plan_history_and_next_revision() {
        let dir = TempDir::new().unwrap();
        {
            let store = make_store(&dir);
            store
                .apply(
                    PlanAction::Create {
                        plan_id: PlanId::new("restart"),
                        goal: "restart-safe".to_owned(),
                    },
                    "orchestrator",
                )
                .unwrap();
            store
                .apply(
                    PlanAction::AddNode {
                        node: make_node("t1"),
                    },
                    "worker",
                )
                .unwrap();
        }

        let reloaded = make_store(&dir);
        assert_eq!(reloaded.current().unwrap().nodes.len(), 1);
        assert_eq!(reloaded.history(None).unwrap().len(), 2);
        assert_eq!(
            reloaded
                .apply(PlanAction::Inspect, "worker")
                .unwrap()
                .revision,
            RevisionId::new(3)
        );
    }

    #[test]
    fn test_second_create_is_rejected_with_plan_exists() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        store
            .apply(
                PlanAction::Create {
                    plan_id: PlanId::new("p-erst"),
                    goal: "erster Plan".to_owned(),
                },
                "orchestrator",
            )
            .unwrap();

        let result = store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-zweit"),
                goal: "darf nicht überschreiben".to_owned(),
            },
            "orchestrator",
        );

        assert!(
            matches!(&result, Err(PlanError::PlanExists { id }) if id == &PlanId::new("p-erst")),
            "ein zweites Create muss fail-closed abgelehnt werden, Ergebnis: {result:?}"
        );
        assert_eq!(store.current().unwrap().id, PlanId::new("p-erst"));
        assert!(
            !dir.path().join("plans").join("p-zweit").exists(),
            "das abgelehnte Create darf kein Plan-Verzeichnis anlegen"
        );
    }

    #[test]
    fn test_create_after_reload_is_rejected_with_plan_exists() {
        let dir = TempDir::new().unwrap();
        {
            let store = make_store(&dir);
            store
                .apply(
                    PlanAction::Create {
                        plan_id: PlanId::new("p-durable"),
                        goal: "überlebt den Neustart".to_owned(),
                    },
                    "orchestrator",
                )
                .unwrap();
        }

        // Nach dem Neustart ist der Plan geladen — ein Create würde ihn sonst
        // still überschreiben.
        let reloaded = make_store(&dir);
        let result = reloaded.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-durable"),
                goal: "anderes Ziel".to_owned(),
            },
            "orchestrator",
        );

        assert!(
            matches!(&result, Err(PlanError::PlanExists { id }) if id == &PlanId::new("p-durable")),
            "Create nach Reload muss abgelehnt werden, Ergebnis: {result:?}"
        );
        assert_eq!(
            reloaded.current().unwrap().goal_statement,
            "überlebt den Neustart"
        );
        assert_eq!(reloaded.history(None).unwrap().len(), 1);
    }

    #[test]
    fn test_failed_history_write_does_not_commit_memory_or_revision_file() {
        let dir = TempDir::new().unwrap();
        let history_path = dir
            .path()
            .join("plans")
            .join("p-rollback")
            .join("history.jsonl");
        std::fs::create_dir_all(history_path.parent().unwrap()).unwrap();
        std::fs::create_dir(&history_path).unwrap();
        let store = make_store(&dir);

        let result = store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-rollback"),
                goal: "must not commit".to_owned(),
            },
            "orchestrator",
        );
        assert!(result.is_err());
        assert_eq!(store.revision(), RevisionId::new(0));
        assert!(matches!(store.current(), Err(PlanError::PlanNotFound)));
        assert!(
            !dir.path()
                .join("plans")
                .join("p-rollback")
                .join("rev-1.json")
                .exists()
        );
    }

    #[test]
    fn test_failed_history_append_does_not_publish_update_snapshot() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        store
            .apply(
                PlanAction::Create {
                    plan_id: PlanId::new("update-rollback"),
                    goal: "must keep revision one".to_owned(),
                },
                "orchestrator",
            )
            .unwrap();

        let history_path = dir
            .path()
            .join("plans")
            .join("update-rollback")
            .join("history.jsonl");
        std::fs::remove_file(&history_path).unwrap();
        std::fs::create_dir(&history_path).unwrap();

        let result = store.apply(
            PlanAction::AddNode {
                node: make_node("t1"),
            },
            "worker",
        );

        assert!(result.is_err());
        assert_eq!(store.revision(), RevisionId::new(1));
        assert!(store.current().unwrap().nodes.is_empty());
        assert!(
            !dir.path()
                .join("plans")
                .join("update-rollback")
                .join("rev-2.json")
                .exists(),
            "ein fehlgeschlagener History-Append darf keinen Update-Snapshot veröffentlichen"
        );
    }

    #[test]
    fn test_with_config_rejects_disabled_tool_before_creating_store_root() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("disabled-store");

        let result = FilePlanStore::with_config(&root, PlanToolConfig::default());

        assert!(
            matches!(
                result,
                Err(PlanError::Config(PlanToolConfigError::Disabled))
            ),
            "deaktiviertes Tool muss einen typisierten Konfigurationsfehler liefern"
        );
        assert!(
            !root.exists(),
            "deaktivierte Konfiguration darf kein Wurzelverzeichnis anlegen"
        );
    }

    #[test]
    fn test_configured_store_rejects_node_limit_before_durable_write() {
        let dir = TempDir::new().unwrap();
        let store = FilePlanStore::with_config(dir.path(), config_with_max_nodes(1)).unwrap();
        store
            .apply(
                PlanAction::Create {
                    plan_id: PlanId::new("node-limit"),
                    goal: "enforce node limit".to_owned(),
                },
                "orchestrator",
            )
            .unwrap();
        store
            .apply(
                PlanAction::AddNode {
                    node: make_node("t1"),
                },
                "worker",
            )
            .unwrap();

        let result = store.apply(
            PlanAction::AddNode {
                node: make_node("t2"),
            },
            "worker",
        );

        assert!(
            matches!(
                result,
                Err(PlanError::Config(PlanToolConfigError::NodeLimitExceeded {
                    max_nodes: 1,
                    attempted_nodes: 2,
                }))
            ),
            "Knotenlimit muss AddNode mit einem typisierten Konfigurationsfehler ablehnen"
        );
        assert_eq!(store.current().unwrap().nodes.len(), 1);
        assert_eq!(store.revision(), RevisionId::new(2));
        assert!(
            !dir.path()
                .join("plans")
                .join("node-limit")
                .join("rev-3.json")
                .exists(),
            "abgelehnte Aktion darf keinen Snapshot schreiben"
        );
        assert_eq!(store.history(None).unwrap().len(), 2);
    }
}
