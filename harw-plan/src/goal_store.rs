//! Store-Implementierungen des [`GoalStore`]-Traits — flüchtig und persistent.
//!
//! # Verantwortungsbereich
//! Dieses Modul besitzt ausschließlich Locking, Revisionsvergabe, Zeitstempel-
//! hoheit, Persistenz und die Event-History eines Ziels. Die Regeln selbst —
//! Status-Matrix, Kriterien-Voraussetzung und die Autoritätsregel „ein
//! Modell-Akteur darf `Achieved`/`Abandoned` nicht setzen“ — liegen **nicht**
//! hier, sondern in [`validate_goal_action`]; die Mutation liegt in
//! [`apply_goal_action`]. Beide Stores rufen dieselben reinen Funktionen auf,
//! es gibt keine zweite Kopie der Goal-Logik.
//!
//! Der Aufruf von [`validate_goal_action`] in `apply` ist die **zweite von zwei
//! unabhängigen Grenzen** für die Autoritätsregel: die erste liegt in der
//! aufrufenden Op (z. B. `/goal set`), die zweite hier an der Persistenzgrenze.
//! Ein Modell-Akteur, der die Op umgeht, wird deshalb auch dann abgewiesen,
//! wenn er direkt gegen den Store spricht — und ein abgewiesener Versuch
//! hinterlässt weder einen History-Eintrag noch einen Snapshot auf Platte.
//!
//! # Exportierte Typen
//! - [`InMemoryGoalStore`] — flüchtig, `RwLock<Inner>`, für Tests und
//!   Sitzungen ohne Persistenz.
//! - [`FileGoalStore`] — persistent, gleiche Persistenzform wie
//!   [`crate::file_store::FilePlanStore`]: atomarer Snapshot über
//!   Temp-Datei → `rename` → Parent-fsync, `history.jsonl` als Append-Log,
//!   `load()` beim Öffnen.
//!
//! # Concurrency
//! Beide Typen sind `Send + Sync` durch `std::sync::RwLock<Inner>`. `current`,
//! `revision` und `history` halten einen Lese-Lock, `apply` einen exklusiven
//! Schreib-Lock. Ein vergifteter Lock wird in
//! [`PlanError::Io`] überführt (kein `unwrap()`); `revision` kann keinen Fehler
//! melden und liest deshalb den inneren Wert des vergifteten Locks aus — genau
//! wie [`crate::memory_store::InMemoryPlanStore::revision`].
//!
//! # Fehler
//! Alle Operationen geben [`PlanError`] zurück: [`PlanError::GoalNotFound`],
//! [`PlanError::GoalTransitionReserved`], [`PlanError::ActorNotAuthorized`] und
//! [`PlanError::InvalidId`] aus [`validate_goal_action`], dazu
//! [`PlanError::Io`] und [`PlanError::Serde`] aus der Persistenz.
//!
//! # Abweichungen von `file_store.rs` (und ihr Grund)
//! 1. **Ein Verzeichnis je Ziel.** [`FileGoalStore::new`] bekommt das
//!    Wurzelverzeichnis *dieses einen* Ziels (`<HARW_HOME>/goals/<goal_id>`),
//!    nicht ein Sammelverzeichnis. Der Snapshot heißt daher `<root>/rev-<n>.json`
//!    statt `<root>/plans/<plan_id>/rev-<n>.json`. Damit entfällt auch der
//!    Tie-Break über die ID, den `FilePlanStore::load` beim Scannen mehrerer
//!    Plan-Verzeichnisse braucht — pro Verzeichnis existiert genau ein Ziel und
//!    je Revision genau eine Datei.
//! 2. **`Set` ersetzt, `Create` nicht.** `FilePlanStore` weist ein zweites
//!    `Create` mit [`PlanError::PlanExists`] ab. [`GoalAction::Set`] ist laut
//!    [`crate::goal`] ausdrücklich „legt an **bzw. ersetzt vollständig**“, und
//!    [`validate_goal_action`] lässt es zu; ein `GoalExists`-Gegenstück
//!    existiert in [`PlanError`] nicht. Ein erneutes `Set` wird deshalb
//!    angenommen — `created_at` des ursprünglichen Ziels bleibt dabei erhalten
//!    (siehe [`FileGoalStore::apply`]).
//! 3. **Keine `with_config`-Variante.** [`crate::config::PlanToolConfig`]
//!    beschreibt Plan-Knoten (`max_nodes`, `require_exploration_for`) und hat
//!    auf ein Ziel-Aggregat keine anwendbare Regel; `validate_action` nimmt
//!    ausschließlich eine [`crate::actions::PlanAction`] entgegen. Eine
//!    Konfigurationsgrenze, die nichts prüfen könnte, wäre reine Attrappe.
//! 4. **`StagedWrite` und `sync_parent_directory` sind hier erneut
//!    definiert.** Beide sind in `file_store.rs` modul-privat und dieses AP
//!    besitzt `file_store.rs` nicht. Die Struktur ist bewusst identisch, damit
//!    ein späteres Herausziehen in ein gemeinsames, crate-privates Modul eine
//!    reine Verschiebung bleibt.
//! 5. **Revision ist `u64`, nicht [`crate::ids::RevisionId`].** Das gibt
//!    [`crate::goal::Goal`] so vor (`revision: u64`,
//!    [`GoalStore::revision`] → `u64`); der Newtype gehört dem Plan-Aggregat.
//!
//! # Integritätssiegel (W3/C-PLAN)
//! [`FileGoalStore`] versiegelt jeden Snapshot wie `FilePlanStore`
//! (`rev-<n>.seal`, gemeinsamer crate-privater Code in
//! `crate::file_store::seal`) und prüft das Siegel des neuesten Snapshots
//! beim Öffnen ([`PlanError::SealMismatch`]). Ein Verzeichnis ohne jedes
//! Siegel gilt als Legacy-Stand (einmal `warn!`, der nächste Write
//! versiegelt). Ungeschlüsselt: erkennt Beschädigung und naive Manipulation,
//! nicht einen Angreifer mit Schreibrecht auf `HARW_HOME`.
//!
//! # Examples
//! ```rust,no_run
//! use harw_plan::error::PlanError;
//! use harw_plan::goal::GoalStore;
//! use harw_plan::goal_store::{FileGoalStore, InMemoryGoalStore};
//!
//! // Vor dem ersten `Set` hält der Store kein Ziel.
//! let volatile = InMemoryGoalStore::new();
//! assert!(matches!(volatile.current(), Err(PlanError::GoalNotFound)));
//! assert_eq!(volatile.revision(), 0);
//!
//! // Der Datei-Store lädt ein bereits persistiertes Ziel beim Öffnen.
//! let durable = FileGoalStore::new("/srv/harw/goals/g-1");
//! assert!(durable.is_ok());
//! ```

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use time::OffsetDateTime;
use tracing::{debug, info};

use crate::error::{PlanError, PlanResult};
use crate::file_store::seal;
use crate::goal::{
    Goal, GoalAction, GoalEvent, GoalStore, apply_goal_action, validate_goal_action,
};

/// Meldung eines vergifteten `RwLock` — identisch zu `file_store`/`memory_store`,
/// damit dieselbe Ursache überall denselben Text liefert.
const POISONED_LOCK: &str = "RwLock vergiftet";

/// Übersetzt einen vergifteten Lock in einen typisierten Fehler.
///
/// `PlanError` hat keine eigene Lock-Variante; `Io` ist die semantisch nächste
/// vorhandene (so löst es auch `FilePlanStore`).
fn poisoned_lock() -> PlanError {
    PlanError::Io(std::io::Error::other(POISONED_LOCK))
}

/// Interner Zustand des In-Memory-Goal-Stores.
#[derive(Debug)]
struct MemoryInner {
    /// Aktuelles Ziel (`None`, solange kein `Set` angewendet wurde).
    goal: Option<Goal>,
    /// Event-History (append-only).
    history: Vec<GoalEvent>,
    /// Nächste zu vergebende Revisionsnummer.
    next_revision: u64,
}

/// Interner Zustand des Datei-Goal-Stores.
#[derive(Debug)]
struct FileInner {
    /// Aktuelles Ziel (im RAM gecacht).
    goal: Option<Goal>,
    /// Event-History (im RAM gecacht; die Wahrheit steht in `history.jsonl`).
    history: Vec<GoalEvent>,
    /// Nächste zu vergebende Revisionsnummer.
    next_revision: u64,
    /// Letztes Glied der Siegelkette (`None` vor dem ersten Siegel).
    seal: Option<seal::SealLink>,
    /// Wurzelverzeichnis dieses einen Ziels.
    root: PathBuf,
}

/// Ergebnis von `FileGoalStore::load`: Ziel, History, nächste Revision und
/// letztes Siegel-Kettenglied.
type LoadedGoalState = (Option<Goal>, Vec<GoalEvent>, u64, Option<seal::SealLink>);

/// Vollständig synchronisierter, aber noch nicht sichtbarer Snapshot-Write.
///
/// Strukturgleich zu `file_store::StagedWrite` (dort modul-privat, siehe
/// Modulkopf Abweichung 4).
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
        FileGoalStore::sync_parent_directory(&self.target)
    }

    /// Verwirft den noch nicht sichtbaren Snapshot nach einem fehlgeschlagenen
    /// History-Append.
    fn discard(self) {
        let _ = std::fs::remove_file(self.temporary);
    }
}

/// Thread-sicherer, flüchtiger [`GoalStore`].
///
/// # Description
/// Hält Ziel, History und Revisionszähler in einem `std::sync::RwLock`. Jede
/// Mutation läuft über [`validate_goal_action`] und [`apply_goal_action`];
/// `revision`, `created_at` und `updated_at` gehören dem Store, nicht dem
/// Aufrufer. Pendant zu [`crate::memory_store::InMemoryPlanStore`] — ideal für
/// Unit-Tests und Sitzungen, die kein Ziel überdauern müssen.
///
/// # Concurrency
/// `Send + Sync`. Lesezugriffe halten einen Lese-Lock, `apply` einen exklusiven
/// Schreib-Lock über die gesamte Validierung und Mutation.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::error::PlanError;
/// use harw_plan::goal::GoalStore;
/// use harw_plan::goal_store::InMemoryGoalStore;
///
/// let store = InMemoryGoalStore::new();
/// assert!(matches!(store.current(), Err(PlanError::GoalNotFound)));
/// assert!(store.history(None).unwrap_or_default().is_empty());
/// ```
#[derive(Debug)]
pub struct InMemoryGoalStore {
    inner: RwLock<MemoryInner>,
}

impl InMemoryGoalStore {
    /// Erstellt einen neuen, leeren `InMemoryGoalStore`.
    ///
    /// # Returns
    /// Ein Store ohne Ziel; [`GoalStore::current`] meldet bis zum ersten
    /// `Set` [`PlanError::GoalNotFound`], [`GoalStore::revision`] liefert `0`.
    ///
    /// # Concurrency
    /// Der zurückgegebene Store ist `Send + Sync` und kann per `Arc` geteilt
    /// werden.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_plan::goal::GoalStore;
    /// use harw_plan::goal_store::InMemoryGoalStore;
    ///
    /// let store = InMemoryGoalStore::new();
    /// assert_eq!(store.revision(), 0);
    /// ```
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(MemoryInner {
                goal: None,
                history: Vec::new(),
                next_revision: 1,
            }),
        }
    }
}

impl Default for InMemoryGoalStore {
    /// Entspricht [`InMemoryGoalStore::new`] — ein leerer Store ohne Ziel.
    fn default() -> Self {
        Self::new()
    }
}

/// [`GoalStore`] auf flüchtigem Speicher.
///
/// Alle Regeln stammen aus [`validate_goal_action`], alle Mutationen aus
/// [`apply_goal_action`]; dieser Block verantwortet nur Lock, Revision,
/// Zeitstempel und History.
impl GoalStore for InMemoryGoalStore {
    /// Gibt eine Kopie des aktuellen Ziels zurück.
    ///
    /// # Returns
    /// Das aktuelle [`Goal`] inklusive der vom Store gesetzten Felder
    /// `revision`, `created_at` und `updated_at`.
    ///
    /// # Errors
    /// - [`PlanError::GoalNotFound`] solange kein `Set` angewendet wurde.
    /// - [`PlanError::Io`] wenn der Lock vergiftet ist.
    ///
    /// # Concurrency
    /// Hält nur einen Lese-Lock; das Ziel wird als Snapshot herausgegeben.
    fn current(&self) -> PlanResult<Goal> {
        let inner = self.inner.read().map_err(|_| poisoned_lock())?;
        inner.goal.clone().ok_or(PlanError::GoalNotFound)
    }

    /// Gibt die Revision des aktuellen Ziels zurück (`0`, wenn keins existiert).
    ///
    /// # Concurrency
    /// Hält einen Lese-Lock. Ein vergifteter Lock wird nicht zum Fehler — die
    /// Trait-Signatur lässt keinen zu —, sondern über `into_inner()` gelesen.
    fn revision(&self) -> u64 {
        let inner = self.inner.read().unwrap_or_else(|error| error.into_inner());
        match &inner.goal {
            Some(goal) => goal.revision,
            None => 0,
        }
    }

    /// Validiert die Aktion, wendet sie an und schreibt ein [`GoalEvent`].
    ///
    /// # Description
    /// Reihenfolge: Lock → [`validate_goal_action`] → Mutation → Revisions- und
    /// Zeitstempelvergabe → History-Append. Eine abgewiesene Aktion verlässt
    /// die Funktion **vor** jeder Mutation: weder Ziel noch Revision noch
    /// History ändern sich. Bei `Set` bleibt ein bereits vorhandenes
    /// `created_at` erhalten, weil es den Beginn der Zielverfolgung markiert und
    /// nicht den der letzten Fassung; alle anderen Felder ersetzt `Set`
    /// vollständig.
    ///
    /// # Arguments
    /// - `action` (`GoalAction`): anzuwendende Aktion (übernimmt Eigentum, weil
    ///   sie unverändert in das Event wandert).
    /// - `actor` (`&str`): Akteur, z. B. `"human:alice"` oder `"model:gpt"`.
    ///
    /// # Returns
    /// Das persistierte [`GoalEvent`] mit der neu vergebenen Revision.
    ///
    /// # Errors
    /// - [`PlanError::GoalNotFound`], [`PlanError::GoalTransitionReserved`],
    ///   [`PlanError::InvalidId`] aus [`validate_goal_action`].
    /// - [`PlanError::ActorNotAuthorized`] wenn ein `model:`-Akteur
    ///   `Achieved`/`Abandoned` erklären will.
    /// - [`PlanError::Io`] wenn der Lock vergiftet ist.
    ///
    /// # Concurrency
    /// Hält über die gesamte Operation einen exklusiven Schreib-Lock; Validierung
    /// und Mutation sind damit atomar gegenüber anderen Threads.
    fn apply(&self, action: GoalAction, actor: &str) -> PlanResult<GoalEvent> {
        self.apply_guarded(action, actor, &|_| Ok(()))
    }

    /// Wie [`GoalStore::apply`]; `guard` sieht das gespeicherte Ziel unter
    /// demselben exklusiven Schreib-Lock wie Validierung und Mutation (atomar
    /// gegenüber anderen Threads). Ein abgewiesener `guard` verändert nichts.
    ///
    /// # Errors
    /// Der Fehler von `guard`, sonst wie [`GoalStore::apply`].
    fn apply_guarded(
        &self,
        action: GoalAction,
        actor: &str,
        guard: &dyn Fn(Option<&Goal>) -> PlanResult<()>,
    ) -> PlanResult<GoalEvent> {
        let mut inner = self.inner.write().map_err(|_| poisoned_lock())?;
        guard(inner.goal.as_ref())?;

        // Erste Amtshandlung: die Regeln. Alles danach ist bereits Mutation.
        validate_goal_action(inner.goal.as_ref(), &action, actor)?;

        let now = OffsetDateTime::now_utc();
        let revision = inner.next_revision;

        match &action {
            GoalAction::Set { goal } => {
                info!(goal_id = %goal.id, actor = actor, "Ziel gesetzt");
                let created_at = inner
                    .goal
                    .as_ref()
                    .map_or(now, |existing| existing.created_at);
                let mut stored = goal.clone();
                stored.created_at = created_at;
                inner.goal = Some(stored);
            }
            other => {
                // `validate_goal_action` hat das Ziel für jede Nicht-`Set`-Aktion
                // bereits erzwungen; der Zweig hält die Funktion frei von
                // `unwrap()`, statt auf die Vorbedingung zu vertrauen.
                let Some(goal) = inner.goal.as_mut() else {
                    return Err(PlanError::GoalNotFound);
                };
                apply_goal_action(goal, other, now);
            }
        }

        // Revisions- und Zeitstempelhoheit liegt beim Store (goal.rs §Runtime
        // besitzt Status): ein vom Aufrufer mitgegebener Wert wird überschrieben.
        if let Some(goal) = inner.goal.as_mut() {
            goal.revision = revision;
            goal.updated_at = now;
        }
        inner.next_revision = revision.saturating_add(1);

        debug!(revision = revision, actor = actor, "Ziel-Aktion angewendet");

        let event = GoalEvent {
            revision,
            action,
            actor: actor.to_owned(),
            applied_at: now,
        };
        inner.history.push(event.clone());
        Ok(event)
    }

    /// Gibt die Event-History in chronologischer Reihenfolge zurück.
    ///
    /// # Arguments
    /// - `since` (`Option<u64>`): `None` liefert alle Events, `Some(n)` alle mit
    ///   `revision >= n` (inklusiv).
    ///
    /// # Returns
    /// `Vec<GoalEvent>` — leer, wenn noch keine Aktion angewendet wurde.
    ///
    /// # Errors
    /// - [`PlanError::Io`] wenn der Lock vergiftet ist.
    ///
    /// # Concurrency
    /// Hält nur einen Lese-Lock.
    fn history(&self, since: Option<u64>) -> PlanResult<Vec<GoalEvent>> {
        let inner = self.inner.read().map_err(|_| poisoned_lock())?;
        Ok(match since {
            None => inner.history.clone(),
            Some(from) => inner
                .history
                .iter()
                .filter(|event| event.revision >= from)
                .cloned()
                .collect(),
        })
    }
}

/// Persistenter [`GoalStore`], der Ziel und History im Dateisystem ablegt.
///
/// # Description
/// Schreibt jeden Zielzustand als `<root>/rev-<n>.json` und hängt jedes Event an
/// `<root>/history.jsonl` an. `<root>` ist das Verzeichnis *dieses einen* Ziels
/// (siehe [`FileGoalStore::new`]). Der Snapshot wird über eine synchronisierte
/// Temp-Datei plus `std::fs::rename` veröffentlicht — und zwar erst, nachdem das
/// History-Event dauerhaft geschrieben ist. Ein abgebrochener Schreibvorgang
/// hinterlässt deshalb nie einen sichtbaren Halb-Zustand: entweder Event *und*
/// Snapshot existieren, oder keiner von beiden.
///
/// # Concurrency
/// `Send + Sync` durch `RwLock<FileInner>`. Lesezugriffe halten einen Lese-Lock,
/// `apply` einen exklusiven Schreib-Lock über Validierung, I/O und Cache-Update.
/// Der Store synchronisiert *Threads*, nicht *Prozesse*: zwei Prozesse auf
/// demselben Verzeichnis würden dieselbe Revisionsnummer vergeben.
///
/// # Errors
/// Zusätzlich zu den Validierungsfehlern [`PlanError::Io`] und
/// [`PlanError::Serde`] aus Persistenz und Reload.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::goal::GoalStore;
/// use harw_plan::goal_store::FileGoalStore;
///
/// // Öffnet (und erzeugt) das Zielverzeichnis und lädt einen vorhandenen Stand.
/// let store = FileGoalStore::new("/srv/harw/goals/g-1");
/// assert!(store.is_ok());
/// ```
#[derive(Debug)]
pub struct FileGoalStore {
    inner: RwLock<FileInner>,
}

impl FileGoalStore {
    /// Öffnet das Wurzelverzeichnis eines Ziels und lädt seinen letzten Stand.
    ///
    /// # Description
    /// `root` ist das Verzeichnis für **dieses eine** Ziel, z. B.
    /// `<HARW_HOME>/goals/<goal_id>`. Es wird angelegt, falls es fehlt; ein
    /// vorhandener Stand (`rev-<n>.json` plus `history.jsonl`) wird geladen, so
    /// dass Ziel, Revision und History einen Neustart überdauern.
    ///
    /// # Arguments
    /// - `root` (`impl AsRef<Path>`): Wurzelverzeichnis dieses Ziels; wird nur
    ///   geborgt und intern in einen `PathBuf` überführt.
    ///
    /// # Returns
    /// Einen einsatzbereiten `FileGoalStore` — mit geladenem Ziel, falls das
    /// Verzeichnis bereits einen Stand enthielt, sonst leer.
    ///
    /// # Errors
    /// - [`PlanError::Io`] wenn das Verzeichnis nicht angelegt oder nicht
    ///   gelesen werden kann.
    /// - [`PlanError::Serde`] wenn ein vorhandener Snapshot oder eine
    ///   History-Zeile kein gültiges JSON ist. Der Store öffnet dann **nicht**
    ///   fail-open mit leerem Ziel, sondern meldet den Fehler.
    ///
    /// # Concurrency
    /// Der zurückgegebene Store ist `Send + Sync` und kann per `Arc` geteilt
    /// werden.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_plan::goal_store::FileGoalStore;
    ///
    /// let store = FileGoalStore::new("/srv/harw/goals/g-1");
    /// assert!(store.is_ok());
    /// ```
    pub fn new(root: impl AsRef<Path>) -> PlanResult<Self> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        let (goal, history, next_revision, seal) = Self::load(&root)?;
        Ok(Self {
            inner: RwLock::new(FileInner {
                goal,
                history,
                next_revision,
                seal,
                root,
            }),
        })
    }

    /// Lädt beim Öffnen den neuesten vollständigen Zielstand aus `root`.
    ///
    /// Die Dateinamen sind die durable Sequenzquelle: gewählt wird die höchste
    /// `rev-<n>.json`. Anders als `FilePlanStore::load` braucht es keinen
    /// Tie-Break über die ID — ein Verzeichnis gehört genau einem Ziel, und je
    /// Revision existiert genau eine Datei. Die nächste Revision ist das Maximum
    /// aus Dateiname, `goal.revision` und der höchsten History-Revision plus
    /// eins, damit ein halb geschriebener Stand keine Nummer doppelt vergibt.
    ///
    /// Nur der neueste Snapshot wird gelesen; sein Siegel wird vor der
    /// Deserialisierung geprüft.
    fn load(root: &Path) -> PlanResult<LoadedGoalState> {
        if !root.exists() {
            return Ok((None, Vec::new(), 1, None));
        }

        let mut newest: Option<(u64, PathBuf)> = None;
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(number) = name
                .strip_prefix("rev-")
                .and_then(|rest| rest.strip_suffix(".json"))
                .and_then(|digits| digits.parse::<u64>().ok())
            else {
                continue;
            };
            if newest.as_ref().is_none_or(|(current, _)| number > *current) {
                newest = Some((number, entry.path()));
            }
        }

        let Some((file_revision, snapshot_path)) = newest else {
            return Ok((None, Vec::new(), 1, None));
        };
        let bytes = std::fs::read(&snapshot_path)?;
        let dir_sealed = seal::directory_is_sealed(root)?;
        let link = seal::verify(&snapshot_path, file_revision, &bytes, dir_sealed)?;
        let goal: Goal = serde_json::from_slice(&bytes)?;

        let history_path = Self::history_path(root);
        let history = if history_path.exists() {
            let reader = BufReader::new(std::fs::File::open(history_path)?);
            reader
                .lines()
                .filter_map(|line| match line {
                    Ok(line) if line.trim().is_empty() => None,
                    Ok(line) => {
                        Some(serde_json::from_str::<GoalEvent>(&line).map_err(PlanError::Serde))
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
            .unwrap_or(0);
        let next = file_revision
            .max(goal.revision)
            .max(history_revision)
            .saturating_add(1);
        Ok((Some(goal), history, next, link))
    }

    /// Gibt den Snapshot-Pfad einer Revision zurück.
    fn goal_path(root: &Path, revision: u64) -> PathBuf {
        root.join(format!("rev-{revision}.json"))
    }

    /// Gibt den Pfad des Append-Logs zurück.
    fn history_path(root: &Path) -> PathBuf {
        root.join("history.jsonl")
    }

    /// Bereitet `content` als synchronisierte Temp-Datei vor, ohne den Zielpfad
    /// schon sichtbar zu verändern.
    ///
    /// # Errors
    /// - [`PlanError::Io`] bei Schreibfehler.
    fn stage_atomic_write(path: &Path, content: &[u8]) -> PlanResult<StagedWrite> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Tmp-Datei im gleichen Verzeichnis (rename ist nur innerhalb desselben
        // FS atomar). `create_new` verhindert, dass ein stale Tempfile eines
        // anderen Prozesses stillschweigend überschrieben wird.
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let tmp_path = path.with_extension(format!("tmp-{}-{}", std::process::id(), nonce));
        let write_result = (|| -> PlanResult<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)?;
            file.write_all(content)?;
            file.flush()?;
            file.sync_all()?;
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
    fn append_event(root: &Path, event: &GoalEvent) -> PlanResult<()> {
        let path = Self::history_path(root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let line = serde_json::to_string(event)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let original_len = file.metadata()?.len();
        let append_result = (|| -> PlanResult<()> {
            writeln!(file, "{line}")?;
            file.flush()?;
            file.sync_all()?;
            Self::sync_parent_directory(&path)?;
            Ok(())
        })();
        if let Err(error) = append_result {
            // Ein fehlgeschlagener Write kann eine Teilzeile hinterlassen haben.
            // Vor dem Melden des Fehlers auf die Länge vor dem Append kürzen.
            if file.set_len(original_len).is_ok() {
                let _ = file.sync_all();
                let _ = Self::sync_parent_directory(&path);
            }
            return Err(error);
        }
        Ok(())
    }
}

/// [`GoalStore`] auf dem Dateisystem.
///
/// Regeln und Mutation stammen unverändert aus [`validate_goal_action`] und
/// [`apply_goal_action`]; dieser Block verantwortet Persistenz, Reload, Lock,
/// Revisionsvergabe und Zeitstempel.
impl GoalStore for FileGoalStore {
    /// Gibt eine Kopie des aktuellen Ziels aus dem RAM-Cache zurück.
    ///
    /// # Returns
    /// Das zuletzt dauerhaft geschriebene [`Goal`]. Der Cache wird erst nach
    /// erfolgreichem Schreiben aktualisiert und kann deshalb nie einen Stand
    /// zeigen, der nicht auf Platte steht.
    ///
    /// # Errors
    /// - [`PlanError::GoalNotFound`] wenn weder ein `Set` angewendet noch beim
    ///   Öffnen ein Stand geladen wurde.
    /// - [`PlanError::Io`] wenn der Lock vergiftet ist.
    ///
    /// # Concurrency
    /// Hält nur einen Lese-Lock; kein Dateizugriff.
    fn current(&self) -> PlanResult<Goal> {
        let inner = self.inner.read().map_err(|_| poisoned_lock())?;
        inner.goal.clone().ok_or(PlanError::GoalNotFound)
    }

    /// Gibt die Revision des aktuellen Ziels zurück (`0`, wenn keins existiert).
    ///
    /// # Concurrency
    /// Hält einen Lese-Lock; ein vergifteter Lock wird über `into_inner()`
    /// gelesen, weil die Trait-Signatur keinen Fehler zulässt.
    fn revision(&self) -> u64 {
        let inner = self.inner.read().unwrap_or_else(|error| error.into_inner());
        match &inner.goal {
            Some(goal) => goal.revision,
            None => 0,
        }
    }

    /// Validiert die Aktion, schreibt sie dauerhaft und gibt das Event zurück.
    ///
    /// # Description
    /// Reihenfolge: Lock → [`validate_goal_action`] → Kandidat mutieren →
    /// Snapshot als Temp-Datei stagen → History-Event anhängen → Snapshot per
    /// `rename` veröffentlichen → RAM-Cache aktualisieren. Der Kandidat wird
    /// erst sichtbar, wenn das Event dauerhaft geschrieben ist; scheitert der
    /// Append, wird die Temp-Datei verworfen und weder Revision noch Cache
    /// rücken vor. Eine von der Validierung abgewiesene Aktion erreicht das
    /// Dateisystem gar nicht erst — sie erzeugt weder Snapshot noch
    /// History-Zeile.
    ///
    /// Bei `Set` bleibt ein bereits vorhandenes `created_at` erhalten (Beginn
    /// der Zielverfolgung, nicht der letzten Fassung); alle übrigen Felder
    /// ersetzt `Set` vollständig.
    ///
    /// # Arguments
    /// - `action` (`GoalAction`): anzuwendende Aktion (übernimmt Eigentum, weil
    ///   sie unverändert in das persistierte Event wandert).
    /// - `actor` (`&str`): Akteur, z. B. `"human:alice"` oder `"model:gpt"`.
    ///
    /// # Returns
    /// Das dauerhaft geschriebene [`GoalEvent`] mit der neu vergebenen Revision.
    ///
    /// # Errors
    /// - [`PlanError::GoalNotFound`], [`PlanError::GoalTransitionReserved`],
    ///   [`PlanError::InvalidId`] aus [`validate_goal_action`].
    /// - [`PlanError::ActorNotAuthorized`] wenn ein `model:`-Akteur
    ///   `Achieved`/`Abandoned` erklären will.
    /// - [`PlanError::Io`] / [`PlanError::Serde`] bei Persistenzfehlern oder
    ///   vergiftetem Lock.
    ///
    /// # Concurrency
    /// Hält über Validierung, I/O und Cache-Update einen exklusiven
    /// Schreib-Lock; nebenläufige `apply`-Aufrufe werden serialisiert.
    fn apply(&self, action: GoalAction, actor: &str) -> PlanResult<GoalEvent> {
        self.apply_guarded(action, actor, &|_| Ok(()))
    }

    /// Wie [`GoalStore::apply`]; `guard` sieht das gespeicherte Ziel unter
    /// demselben exklusiven Schreib-Lock wie Validierung und Mutation (atomar
    /// gegenüber anderen Threads). Ein abgewiesener `guard` verändert nichts.
    ///
    /// # Errors
    /// Der Fehler von `guard`, sonst wie [`GoalStore::apply`].
    fn apply_guarded(
        &self,
        action: GoalAction,
        actor: &str,
        guard: &dyn Fn(Option<&Goal>) -> PlanResult<()>,
    ) -> PlanResult<GoalEvent> {
        let mut inner = self.inner.write().map_err(|_| poisoned_lock())?;
        guard(inner.goal.as_ref())?;

        // Erste Amtshandlung: die Regeln — vor jedem Dateizugriff.
        validate_goal_action(inner.goal.as_ref(), &action, actor)?;

        let now = OffsetDateTime::now_utc();
        let revision = inner.next_revision;

        // Auf einem Kandidaten mutieren: er wird erst nach dem dauerhaften
        // History-Append sichtbar.
        let mut candidate = match &action {
            GoalAction::Set { goal } => {
                info!(goal_id = %goal.id, actor = actor, "Persistentes Ziel gesetzt");
                let mut candidate = goal.clone();
                candidate.created_at = inner
                    .goal
                    .as_ref()
                    .map_or(now, |existing| existing.created_at);
                candidate
            }
            other => {
                // `validate_goal_action` hat das Ziel bereits erzwungen; der
                // Zweig hält die Funktion frei von `unwrap()`.
                let Some(current) = inner.goal.as_ref() else {
                    return Err(PlanError::GoalNotFound);
                };
                let mut candidate = current.clone();
                apply_goal_action(&mut candidate, other, now);
                candidate
            }
        };
        // Revisions- und Zeitstempelhoheit liegt beim Store.
        candidate.revision = revision;
        candidate.updated_at = now;

        let bytes = serde_json::to_vec_pretty(&candidate)?;
        let snapshot_path = Self::goal_path(&inner.root, revision);
        let (seal_bytes, link) = seal::build(inner.seal.as_ref(), revision, &bytes)?;
        let staged_snapshot = Self::stage_atomic_write(&snapshot_path, &bytes)?;
        let staged_seal = match seal::stage(&seal::seal_path(&snapshot_path), &seal_bytes) {
            Ok(staged) => staged,
            Err(error) => {
                staged_snapshot.discard();
                return Err(error);
            }
        };

        let event = GoalEvent {
            revision,
            action,
            actor: actor.to_owned(),
            applied_at: now,
        };
        if let Err(error) = Self::append_event(&inner.root, &event) {
            staged_seal.discard();
            staged_snapshot.discard();
            return Err(error);
        }
        // Siegel vor dem Snapshot (siehe `FilePlanStore`): ein Absturz
        // dazwischen hinterlässt nie einen unversiegelten neuesten Snapshot.
        if let Err(error) = staged_seal.commit() {
            staged_snapshot.discard();
            return Err(error);
        }
        staged_snapshot.commit()?;

        debug!(
            revision = revision,
            actor = actor,
            "Persistente Ziel-Aktion angewendet"
        );
        inner.next_revision = revision.saturating_add(1);
        inner.goal = Some(candidate);
        inner.seal = Some(link);
        inner.history.push(event.clone());
        Ok(event)
    }

    /// Liest die Event-History aus `history.jsonl`.
    ///
    /// # Arguments
    /// - `since` (`Option<u64>`): `None` liefert alle Events, `Some(n)` alle mit
    ///   `revision >= n` (inklusiv).
    ///
    /// # Returns
    /// `Vec<GoalEvent>` in Schreibreihenfolge; leer, wenn noch nichts
    /// geschrieben wurde.
    ///
    /// # Errors
    /// - [`PlanError::Io`] bei Lesefehler oder vergiftetem Lock.
    /// - [`PlanError::Serde`] wenn eine Zeile kein gültiges `GoalEvent` ist.
    ///
    /// # Concurrency
    /// Hält den Lese-Lock über den Dateizugriff, damit kein `apply` mitten in
    /// den Append hineinliest.
    fn history(&self, since: Option<u64>) -> PlanResult<Vec<GoalEvent>> {
        let inner = self.inner.read().map_err(|_| poisoned_lock())?;
        let path = Self::history_path(&inner.root);
        if !path.exists() {
            // Ohne Datei kann auch der RAM-Cache nichts halten: ein Event wird
            // erst nach erfolgreichem Append eingefügt.
            return Ok(Vec::new());
        }

        let reader = BufReader::new(std::fs::File::open(&path)?);
        let mut events = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let event: GoalEvent = serde_json::from_str(&line)?;
            if since.is_none_or(|from| event.revision >= from) {
                events.push(event);
            }
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::{GoalId, GoalPatch, GoalStatus, Invariant};
    use crate::test_support::TestResult;
    use crate::types::{Criterion, VerificationStep};
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Baut ein Ziel mit genau einem Akzeptanzkriterium und einer Invariante.
    fn make_goal(status: GoalStatus) -> Goal {
        Goal {
            id: GoalId::new("g-test"),
            revision: 0,
            statement: "Goal-Store fertigstellen".to_owned(),
            non_goals: vec!["kein Rewrite von harw-plan".to_owned()],
            invariants: vec![Invariant {
                id: "inv-1".to_owned(),
                statement: "keine unsafe-Blöcke".to_owned(),
                verification: vec![VerificationStep::Manual {
                    note: "review".to_owned(),
                }],
            }],
            acceptance_criteria: vec![Criterion {
                description: "alle Tests grün".to_owned(),
                verification: vec![VerificationStep::Command {
                    cmd: "cargo test".to_owned(),
                    expect_exit: 0,
                }],
            }],
            constraints: Vec::new(),
            open_questions: vec!["offene Frage".to_owned()],
            status,
            plan_id: None,
            plan_revision: None,
            evidence: Vec::new(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            tenant: None,
        }
    }

    /// Wendet eine Aktion an und gibt bei Fehlschlag `Err` zurück — Ersatz
    /// für das projektweit verbotene `unwrap()`/`panic!` (Bible R087/R165).
    fn apply_ok(store: &dyn GoalStore, action: GoalAction, actor: &str) -> TestResult<GoalEvent> {
        Ok(store.apply(action, actor)?)
    }

    /// Liest das aktuelle Ziel oder gibt `Err` zurück.
    fn current_ok(store: &dyn GoalStore) -> TestResult<Goal> {
        Ok(store.current()?)
    }

    /// Liest die History oder gibt `Err` zurück.
    fn history_ok(store: &dyn GoalStore, since: Option<u64>) -> TestResult<Vec<GoalEvent>> {
        Ok(store.history(since)?)
    }

    /// Legt ein Temp-Verzeichnis an oder gibt `Err` zurück.
    fn temp_dir() -> TestResult<TempDir> {
        Ok(TempDir::new()?)
    }

    /// Öffnet einen `FileGoalStore` auf `dir` oder gibt `Err` zurück.
    fn file_store(dir: &TempDir) -> TestResult<FileGoalStore> {
        Ok(FileGoalStore::new(dir.path())?)
    }

    /// Setzt ein aktives Ziel und liefert den Store zurück.
    fn seeded_memory_store() -> TestResult<InMemoryGoalStore> {
        let store = InMemoryGoalStore::new();
        apply_ok(
            &store,
            GoalAction::Set {
                goal: make_goal(GoalStatus::Active),
            },
            "human:alice",
        )?;
        Ok(store)
    }

    // ── InMemoryGoalStore ────────────────────────────────────────────────────

    #[test]
    fn test_apply_set_roundtrips_through_current() -> TestResult {
        let store = seeded_memory_store()?;

        let goal = current_ok(&store)?;
        assert_eq!(goal.id, GoalId::new("g-test"));
        assert_eq!(goal.statement, "Goal-Store fertigstellen");
        assert_eq!(
            goal.non_goals,
            vec!["kein Rewrite von harw-plan".to_owned()]
        );
        assert_eq!(goal.acceptance_criteria.len(), 1);
        assert_eq!(goal.invariants.len(), 1);
        assert_eq!(goal.status, GoalStatus::Active);
        Ok(())
    }

    #[test]
    fn test_current_before_any_set_returns_goal_not_found() -> TestResult {
        let store = InMemoryGoalStore::new();

        assert!(matches!(store.current(), Err(PlanError::GoalNotFound)));
        assert_eq!(store.revision(), 0);
        assert!(history_ok(&store, None)?.is_empty());
        Ok(())
    }

    #[test]
    fn test_apply_increments_revision_by_exactly_one_per_action() -> TestResult {
        let store = seeded_memory_store()?;
        assert_eq!(store.revision(), 1);

        let second = apply_ok(
            &store,
            GoalAction::AddCriterion {
                criterion: Criterion {
                    description: "Doku vollständig".to_owned(),
                    verification: Vec::new(),
                },
            },
            "human:alice",
        )?;
        assert_eq!(second.revision, 2);
        assert_eq!(store.revision(), 2);

        let third = apply_ok(&store, GoalAction::Inspect, "human:alice")?;
        assert_eq!(third.revision, 3);
        assert_eq!(current_ok(&store)?.revision, 3);
        Ok(())
    }

    #[test]
    fn test_history_returns_all_events_and_filters_from_since() -> TestResult {
        let store = seeded_memory_store()?;
        apply_ok(&store, GoalAction::Inspect, "human:alice")?;
        apply_ok(
            &store,
            GoalAction::Condense {
                summary: "verdichtet".to_owned(),
            },
            "human:alice",
        )?;

        let all = history_ok(&store, None)?;
        assert_eq!(all.len(), 3);
        assert_eq!(
            all.iter().map(|event| event.revision).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );

        let tail = history_ok(&store, Some(2))?;
        assert_eq!(
            tail.iter().map(|event| event.revision).collect::<Vec<_>>(),
            vec![2, 3],
            "history(Some(n)) muss ab n einschließlich liefern"
        );
        Ok(())
    }

    #[test]
    fn test_apply_model_actor_cannot_declare_achieved_and_leaves_goal_untouched() -> TestResult {
        let store = seeded_memory_store()?;

        let result = store.apply(
            GoalAction::SetStatus {
                status: GoalStatus::Achieved,
                reason: Some("erledigt".to_owned()),
            },
            "model:gpt",
        );

        assert!(
            matches!(result, Err(PlanError::ActorNotAuthorized { .. })),
            "ein Modell-Akteur darf Achieved nicht erklären, Ergebnis: {result:?}"
        );
        let goal = current_ok(&store)?;
        assert_eq!(
            goal.status,
            GoalStatus::Active,
            "der abgelehnte Versuch darf den Status nicht verändern"
        );
        assert_eq!(
            goal.revision, 1,
            "der abgelehnte Versuch darf die Revision nicht erhöhen"
        );
        assert_eq!(
            history_ok(&store, None)?.len(),
            1,
            "der abgelehnte Versuch darf keinen History-Eintrag erzeugen"
        );
        Ok(())
    }

    #[test]
    fn test_apply_human_actor_may_declare_achieved() -> TestResult {
        let store = seeded_memory_store()?;

        let event = apply_ok(
            &store,
            GoalAction::SetStatus {
                status: GoalStatus::Achieved,
                reason: None,
            },
            "human:alice",
        )?;

        assert_eq!(event.revision, 2);
        assert_eq!(current_ok(&store)?.status, GoalStatus::Achieved);
        Ok(())
    }

    #[test]
    fn test_apply_refine_never_removes_acceptance_criteria() -> TestResult {
        let store = seeded_memory_store()?;
        let before = current_ok(&store)?;

        apply_ok(
            &store,
            GoalAction::Refine {
                patch: GoalPatch {
                    statement: Some("neuer Wortlaut".to_owned()),
                    non_goals: Some(Vec::new()),
                    open_questions: Some(Vec::new()),
                    ..GoalPatch::default()
                },
            },
            "human:alice",
        )?;

        let after = current_ok(&store)?;
        assert_eq!(after.statement, "neuer Wortlaut");
        assert_eq!(
            after.acceptance_criteria.len(),
            before.acceptance_criteria.len(),
            "Refine darf ein Ziel nie stillschweigend schrumpfen (philosophy.md §5)"
        );
        assert_eq!(after.invariants.len(), before.invariants.len());
        assert_eq!(
            after.acceptance_criteria[0].description,
            before.acceptance_criteria[0].description
        );
        Ok(())
    }

    #[test]
    fn test_apply_non_set_action_without_goal_returns_goal_not_found() {
        let store = InMemoryGoalStore::new();

        let result = store.apply(GoalAction::Inspect, "human:alice");

        assert!(matches!(result, Err(PlanError::GoalNotFound)));
        assert_eq!(store.revision(), 0);
    }

    #[test]
    fn test_default_matches_new_for_in_memory_store() {
        let store = InMemoryGoalStore::default();

        assert_eq!(store.revision(), 0);
        assert!(matches!(store.current(), Err(PlanError::GoalNotFound)));
    }

    #[test]
    fn test_goal_stores_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InMemoryGoalStore>();
        assert_send_sync::<Arc<InMemoryGoalStore>>();
        assert_send_sync::<FileGoalStore>();
        assert_send_sync::<Arc<FileGoalStore>>();
    }

    // ── FileGoalStore ────────────────────────────────────────────────────────

    #[test]
    fn test_apply_writes_snapshot_and_history_files() -> TestResult {
        let dir = temp_dir()?;
        let store = file_store(&dir)?;

        apply_ok(
            &store,
            GoalAction::Set {
                goal: make_goal(GoalStatus::Active),
            },
            "human:alice",
        )?;

        let snapshot = dir.path().join("rev-1.json");
        assert!(snapshot.exists(), "Snapshot fehlt: {snapshot:?}");
        let history_file = dir.path().join("history.jsonl");
        assert!(history_file.exists(), "history.jsonl fehlt");

        let content = std::fs::read_to_string(&history_file)?;
        assert_eq!(
            content
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count(),
            1
        );

        let leftovers = std::fs::read_dir(dir.path())?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(leftovers, 0, "Tmp-Datei wurde nicht entfernt");
        Ok(())
    }

    #[test]
    fn test_new_reloads_goal_and_revision_from_the_same_directory() -> TestResult {
        let dir = temp_dir()?;
        {
            let store = file_store(&dir)?;
            apply_ok(
                &store,
                GoalAction::Set {
                    goal: make_goal(GoalStatus::Active),
                },
                "human:alice",
            )?;
            apply_ok(
                &store,
                GoalAction::AddCriterion {
                    criterion: Criterion {
                        description: "Doku vollständig".to_owned(),
                        verification: Vec::new(),
                    },
                },
                "human:alice",
            )?;
        }

        let reloaded = file_store(&dir)?;
        let goal = current_ok(&reloaded)?;
        assert_eq!(goal.statement, "Goal-Store fertigstellen");
        assert_eq!(
            goal.acceptance_criteria.len(),
            2,
            "das nachgereichte Kriterium muss den Neustart überdauern"
        );
        assert_eq!(
            reloaded.revision(),
            2,
            "die Revision darf nicht zurückfallen"
        );
        assert_eq!(history_ok(&reloaded, None)?.len(), 2);
        assert_eq!(
            apply_ok(&reloaded, GoalAction::Inspect, "human:alice")?.revision,
            3,
            "nach dem Reload muss die nächste Revision fortsetzen, nicht neu beginnen"
        );
        Ok(())
    }

    #[test]
    fn test_current_before_any_set_returns_goal_not_found_on_disk_store() -> TestResult {
        let dir = temp_dir()?;
        let store = file_store(&dir)?;

        assert!(matches!(store.current(), Err(PlanError::GoalNotFound)));
        assert_eq!(store.revision(), 0);
        assert!(history_ok(&store, None)?.is_empty());
        Ok(())
    }

    #[test]
    fn test_history_filters_from_since_on_disk_store() -> TestResult {
        let dir = temp_dir()?;
        let store = file_store(&dir)?;
        apply_ok(
            &store,
            GoalAction::Set {
                goal: make_goal(GoalStatus::Active),
            },
            "human:alice",
        )?;
        apply_ok(&store, GoalAction::Inspect, "human:alice")?;
        apply_ok(&store, GoalAction::Inspect, "human:alice")?;

        assert_eq!(history_ok(&store, None)?.len(), 3);
        assert_eq!(
            history_ok(&store, Some(3))?
                .iter()
                .map(|event| event.revision)
                .collect::<Vec<_>>(),
            vec![3]
        );
        Ok(())
    }

    #[test]
    fn test_apply_model_actor_rejection_writes_nothing_to_disk() -> TestResult {
        let dir = temp_dir()?;
        let store = file_store(&dir)?;
        apply_ok(
            &store,
            GoalAction::Set {
                goal: make_goal(GoalStatus::Active),
            },
            "human:alice",
        )?;

        let result = store.apply(
            GoalAction::SetStatus {
                status: GoalStatus::Achieved,
                reason: None,
            },
            "model:gpt",
        );

        assert!(
            matches!(result, Err(PlanError::ActorNotAuthorized { .. })),
            "ein Modell-Akteur darf Achieved nicht erklären, Ergebnis: {result:?}"
        );
        assert_eq!(current_ok(&store)?.status, GoalStatus::Active);
        assert_eq!(store.revision(), 1);
        assert_eq!(
            history_ok(&store, None)?.len(),
            1,
            "der abgelehnte Versuch darf keine History-Zeile schreiben"
        );
        assert!(
            !dir.path().join("rev-2.json").exists(),
            "der abgelehnte Versuch darf keinen Snapshot veröffentlichen"
        );

        // Auch ein Neustart darf den abgelehnten Versuch nicht sichtbar machen.
        let reloaded = file_store(&dir)?;
        assert_eq!(current_ok(&reloaded)?.status, GoalStatus::Active);
        assert_eq!(reloaded.revision(), 1);
        Ok(())
    }

    #[test]
    fn test_apply_refine_never_removes_acceptance_criteria_on_disk_store() -> TestResult {
        let dir = temp_dir()?;
        let store = file_store(&dir)?;
        apply_ok(
            &store,
            GoalAction::Set {
                goal: make_goal(GoalStatus::Active),
            },
            "human:alice",
        )?;

        apply_ok(
            &store,
            GoalAction::Refine {
                patch: GoalPatch {
                    statement: Some("verdichtetes Ziel".to_owned()),
                    ..GoalPatch::default()
                },
            },
            "human:alice",
        )?;

        let reloaded = file_store(&dir)?;
        let goal = current_ok(&reloaded)?;
        assert_eq!(goal.statement, "verdichtetes Ziel");
        assert_eq!(
            goal.acceptance_criteria.len(),
            1,
            "Refine darf ein Ziel nie stillschweigend schrumpfen (philosophy.md §5)"
        );
        assert_eq!(goal.invariants.len(), 1);
        Ok(())
    }

    #[test]
    fn test_apply_set_preserves_created_at_across_a_replacing_set() -> TestResult {
        let dir = temp_dir()?;
        let store = file_store(&dir)?;
        apply_ok(
            &store,
            GoalAction::Set {
                goal: make_goal(GoalStatus::Draft),
            },
            "human:alice",
        )?;
        let created_at = current_ok(&store)?.created_at;

        let mut replacement = make_goal(GoalStatus::Active);
        replacement.statement = "ersetztes Ziel".to_owned();
        apply_ok(&store, GoalAction::Set { goal: replacement }, "human:alice")?;

        let goal = current_ok(&store)?;
        assert_eq!(goal.statement, "ersetztes Ziel");
        assert_eq!(goal.revision, 2);
        assert_eq!(
            goal.created_at, created_at,
            "created_at markiert den Beginn der Zielverfolgung und darf ein Set überdauern"
        );
        Ok(())
    }

    #[test]
    fn test_stage_atomic_write_publishes_content_and_removes_the_temp_file() -> TestResult {
        let dir = temp_dir()?;
        let target = dir.path().join("rev-7.json");

        let staged = FileGoalStore::stage_atomic_write(&target, b"{\"ok\":true}")?;
        staged.commit()?;

        assert!(target.exists(), "Zieldatei fehlt nach staged write");
        let content = std::fs::read(&target)?;
        assert_eq!(content, b"{\"ok\":true}");
        let leftovers = std::fs::read_dir(dir.path())?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(leftovers, 0, "Tmp-Datei wurde nicht entfernt");
        Ok(())
    }

    #[test]
    fn test_apply_discards_the_snapshot_when_the_history_append_fails() -> TestResult {
        let dir = temp_dir()?;
        // Ein Verzeichnis an der Stelle der history.jsonl lässt den Append
        // scheitern — ohne den Store selbst zu verbiegen.
        std::fs::create_dir(dir.path().join("history.jsonl"))?;
        let store = file_store(&dir)?;

        let result = store.apply(
            GoalAction::Set {
                goal: make_goal(GoalStatus::Active),
            },
            "human:alice",
        );

        assert!(
            result.is_err(),
            "der Append muss scheitern, war: {result:?}"
        );
        assert!(matches!(store.current(), Err(PlanError::GoalNotFound)));
        assert_eq!(store.revision(), 0);
        assert!(
            !dir.path().join("rev-1.json").exists(),
            "ein fehlgeschlagener History-Append darf keinen Snapshot veröffentlichen"
        );
        Ok(())
    }

    // ── Integritätssiegel ──────────────────────────────────────────────────

    #[test]
    fn test_file_goal_store_rejects_manipulated_snapshot_on_open() -> TestResult {
        let dir = temp_dir()?;
        {
            let store = file_store(&dir)?;
            apply_ok(
                &store,
                GoalAction::Set {
                    goal: make_goal(GoalStatus::Active),
                },
                "human:alice",
            )?;
        }
        assert!(dir.path().join("rev-1.seal").exists(), "Siegel geschrieben");
        let snapshot = dir.path().join("rev-1.json");
        let original = std::fs::read_to_string(&snapshot)?;
        let tampered = original.replace("\"active\"", "\"achieved\"");
        assert_ne!(
            original, tampered,
            "Testaufbau: Status muss im Snapshot stehen"
        );
        std::fs::write(&snapshot, tampered)?;

        let result = FileGoalStore::new(dir.path());

        assert!(
            matches!(result, Err(PlanError::SealMismatch { .. })),
            "manipuliertes Ziel muss beim Öffnen abgewiesen werden"
        );
        Ok(())
    }

    #[test]
    fn test_file_goal_store_legacy_without_seal_opens_and_seals_next_write() -> TestResult {
        let dir = temp_dir()?;
        {
            let store = file_store(&dir)?;
            apply_ok(
                &store,
                GoalAction::Set {
                    goal: make_goal(GoalStatus::Active),
                },
                "human:alice",
            )?;
        }
        std::fs::remove_file(dir.path().join("rev-1.seal"))?;

        let legacy = file_store(&dir)?;
        assert_eq!(current_ok(&legacy)?.revision, 1);
        apply_ok(
            &legacy,
            GoalAction::Set {
                goal: make_goal(GoalStatus::Active),
            },
            "human:alice",
        )?;
        assert!(dir.path().join("rev-2.seal").exists());
        drop(legacy);
        assert_eq!(current_ok(&file_store(&dir)?)?.revision, 2);
        Ok(())
    }
}
