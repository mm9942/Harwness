//! Workbench: die flüchtige, aber persistente Arbeitsfläche einer Sitzung
//! bzw. eines Projekts (`docs/design/knowledge-surfaces.md` §5).
//!
//! # Verantwortung
//! Dieses Modul besitzt die Datentypen **und** die Ein-/Ausgabe der drei
//! Workbench-Dateien unter `workbench/<scope-id>/` (§1.2):
//! - `pinned/MANIFEST.md` — angeheftete absolute Pfade mit einzeiliger
//!   Begründung (`- <pfad> — <notiz>`), nie Kopien, nie Symlinks.
//! - `NOTES.md` — freie Notizen, je Eintrag ein `## <RFC3339>`-Abschnitt.
//! - `hypotheses.md` — strukturierte Hypothesenliste
//!   (`- [ ] <text> — status: testing|confirmed|rejected`).
//!
//! Jede Datei ist ein [`KnowledgeArtifact`] mit dem kanonischen
//! Frontmatter-Wrapper (§1.2) — nur so bleibt `KnowledgeIndex::rebuild`
//! frei von „übersprungenen" Dateien. Sichtbarkeit ist immer
//! [`VisibilityScope::SelfOnly`]: die Workbench ist eine Push-Fläche der
//! eigenen Sitzung (§5.2), kein Recall-Ziel.
//!
//! # Schreibpfad
//! Alle Schreibvorgänge sind Read-Modify-Write über
//! [`KnowledgeStore::write_artifact`] (Temp-Datei + `rename`, atomar). Ein
//! prozessweiter Mutex serialisiert die Read-Modify-Write-Zyklen dieses
//! Moduls, damit zwei gleichzeitige Aufrufe (Slash-Kommando und
//! Modell-Werkzeug derselben Sitzung) keinen Eintrag verlieren.
//! Prozessübergreifende Schreiber serialisiert das nicht (siehe offene Frage
//! §Open-4 des Entwurfs).
//!
//! # Fehler
//! Ungültige Eingaben (leerer Text, relativer Pfad, unsicherer Scope,
//! Überlänge) werden als [`KnowledgeError::Io`] mit
//! [`std::io::ErrorKind::InvalidInput`] gemeldet — dieselbe Konvention wie
//! `store::ensure_path_component`. Unbekannte Hypothesen melden
//! [`KnowledgeError::ArtifactNotFound`], ein zweiter Entscheid über dieselbe
//! Hypothese [`KnowledgeError::IllegalTransition`].

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact};
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::store::KnowledgeStore;
use crate::visibility::{AgentId, VisibilityScope};

/// Serialisiert die Read-Modify-Write-Zyklen dieses Moduls prozessweit.
static WORKBENCH_LOCK: Mutex<()> = Mutex::new(());

/// Obergrenze für eine einzelne Notiz in Bytes.
pub const MAX_NOTE_BYTES: usize = 16 * 1024;

/// Obergrenze für einzeilige Einträge (Hypothese, Pin-Notiz, Pfad) in Bytes.
pub const MAX_LINE_BYTES: usize = 1024;

/// Trenner zwischen Pfad und Notiz bzw. Hypothese und Status.
const SEPARATOR: &str = " — ";

/// Präfix des Statusfelds einer Hypothesenzeile.
const STATUS_MARKER: &str = " — status: ";

/// Frontmatter-`extra`-Schlüssel, unter dem der Scope jeder Datei steht.
const SCOPE_KEY: &str = "scope";

/// Scope eines Workbench-Verzeichnisses (§5.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WorkbenchScope {
    /// Sitzungsgebunden: `workbench/session:<session-id>/`.
    Session(String),
    /// Projektgebunden: `workbench/project:<slug>/`.
    Project(String),
}

impl WorkbenchScope {
    /// Name des Scope-Verzeichnisses unter `workbench/`.
    #[must_use]
    pub fn path_component(&self) -> String {
        match self {
            Self::Session(id) => format!("session:{id}"),
            Self::Project(slug) => format!("project:{slug}"),
        }
    }

    /// Parst die Scope-Angabe der Kommandogrammatik (`--scope=`).
    ///
    /// # Argumente
    /// - `raw` (`&str`): `session` oder `project:<slug>`.
    /// - `session_id` (`&str`): die aktive Sitzung, für `session`.
    ///
    /// # Fehler
    /// [`KnowledgeError::Io`] (`InvalidInput`) für jede andere Form oder eine
    /// unsichere Id (siehe [`Self::validate`]).
    pub fn parse(raw: &str, session_id: &str) -> KnowledgeResult<Self> {
        let scope = match raw.trim() {
            "session" => Self::Session(session_id.to_owned()),
            other => match other.strip_prefix("project:") {
                Some(slug) => Self::Project(slug.to_owned()),
                None => {
                    return Err(invalid_input(format!(
                        "unbekannter Workbench-Scope '{other}' (session | project:<slug>)"
                    )));
                }
            },
        };
        scope.validate()?;
        Ok(scope)
    }

    /// Prüft, dass die Id eine sichere einzelne Pfadkomponente ergibt.
    ///
    /// # Fehler
    /// [`KnowledgeError::Io`] (`InvalidInput`) bei leerer Id, `.`/`..`,
    /// Pfadtrennern oder Steuerzeichen.
    pub fn validate(&self) -> KnowledgeResult<()> {
        let id = match self {
            Self::Session(id) | Self::Project(id) => id.as_str(),
        };
        let unsafe_id = id.is_empty()
            || id == "."
            || id == ".."
            || id.chars().any(|c| c == '/' || c == '\\' || c.is_control());
        if unsafe_id {
            return Err(invalid_input(format!(
                "unsichere Workbench-Scope-Id: {id:?}"
            )));
        }
        Ok(())
    }
}

/// Eine angeheftete Datei: absoluter Pfad plus Begründung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedFile {
    /// Absoluter Pfad (nie eine Kopie, nie ein Symlink).
    pub absolute_path: String,
    /// Einzeilige Begründung; darf leer sein.
    pub note: String,
}

/// Eine leichte Hypothese, die das Themengedächtnis nicht verschmutzen soll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hypothesis {
    /// Einzeiliger Hypothesentext.
    pub text: String,
    /// Aktueller Status.
    pub status: HypothesisStatus,
}

/// Status einer [`Hypothesis`]; nur `Testing` darf entschieden werden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HypothesisStatus {
    /// Noch offen.
    Testing,
    /// Bestätigt.
    Confirmed,
    /// Verworfen.
    Rejected,
}

impl HypothesisStatus {
    /// Stabile Bezeichnung in `hypotheses.md`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Testing => "testing",
            Self::Confirmed => "confirmed",
            Self::Rejected => "rejected",
        }
    }

    /// Umkehrung von [`Self::label`]; `None` für unbekannte Werte.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        match label.trim() {
            "testing" => Some(Self::Testing),
            "confirmed" => Some(Self::Confirmed),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }
}

/// Der vollständige, frisch gelesene Zustand eines Workbench-Scopes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workbench {
    /// Der gelesene Scope.
    pub scope: WorkbenchScope,
    /// Angeheftete Dateien in Manifest-Reihenfolge.
    pub pinned: Vec<PinnedFile>,
    /// Roher Markdown-Body von `NOTES.md` (ohne Frontmatter).
    pub notes: String,
    /// Hypothesen in Listenreihenfolge.
    pub hypotheses: Vec<Hypothesis>,
}

impl Workbench {
    /// Nur die noch offenen Hypothesen, mit ihrer 1-basierten Nummer.
    #[must_use]
    pub fn open_hypotheses(&self) -> Vec<(usize, &Hypothesis)> {
        self.hypotheses
            .iter()
            .enumerate()
            .filter(|(_, hypothesis)| hypothesis.status == HypothesisStatus::Testing)
            .map(|(index, hypothesis)| (index + 1, hypothesis))
            .collect()
    }

    /// Die letzten `max_lines` nicht-leeren Zeilen von `NOTES.md` (Live-Tail, §5.2).
    #[must_use]
    pub fn notes_tail(&self, max_lines: usize) -> Vec<&str> {
        let lines: Vec<&str> = self
            .notes
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        let start = lines.len().saturating_sub(max_lines);
        lines[start..].to_vec()
    }

    /// `true`, wenn nichts angeheftet, notiert oder vermutet ist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pinned.is_empty() && self.notes.trim().is_empty() && self.hypotheses.is_empty()
    }
}

/// Liest den vollständigen Zustand eines Scopes; fehlende Dateien sind leer.
///
/// # Fehler
/// [`KnowledgeError::Io`] bei unsicherem Scope oder Lesefehlern,
/// Frontmatter-Fehler bei beschädigten Dateien.
///
/// # Nebenläufigkeit
/// Reiner Lesezugriff; liest jede Datei einzeln (kein Schnappschuss über alle drei).
pub fn load(store: &KnowledgeStore, scope: &WorkbenchScope) -> KnowledgeResult<Workbench> {
    scope.validate()?;
    let pinned = read_file(store, scope, WorkbenchFile::Manifest)?
        .map(|artifact| parse_manifest(&artifact.body))
        .unwrap_or_default();
    let notes = read_file(store, scope, WorkbenchFile::Notes)?
        .map(|artifact| artifact.body)
        .unwrap_or_default();
    let hypotheses = read_file(store, scope, WorkbenchFile::Hypotheses)?
        .map(|artifact| parse_hypotheses(&artifact.body))
        .unwrap_or_default();
    Ok(Workbench {
        scope: scope.clone(),
        pinned,
        notes,
        hypotheses,
    })
}

/// Heftet einen absoluten Pfad an; ein bereits angehefteter Pfad bekommt die
/// neue Notiz.
///
/// # Rückgabe
/// `true`, wenn der Pfad neu angeheftet wurde, `false` bei einer Ersetzung.
///
/// # Fehler
/// [`KnowledgeError::Io`] (`InvalidInput`) bei relativem Pfad, Steuerzeichen
/// im Pfad oder Überlänge; sonst Lese-/Schreibfehler.
pub fn pin(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    author: &AgentId,
    absolute_path: &str,
    note: &str,
    now: jiff::Timestamp,
) -> KnowledgeResult<bool> {
    scope.validate()?;
    let path = validate_pin_path(absolute_path)?;
    let note = single_line(note, true)?;
    let _guard = WORKBENCH_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let existing = read_file(store, scope, WorkbenchFile::Manifest)?;
    let mut pinned = existing
        .as_ref()
        .map(|artifact| parse_manifest(&artifact.body))
        .unwrap_or_default();
    let added = match pinned.iter_mut().find(|entry| entry.absolute_path == path) {
        Some(entry) => {
            entry.note = note;
            false
        }
        None => {
            pinned.push(PinnedFile {
                absolute_path: path,
                note,
            });
            true
        }
    };
    write_file(
        store,
        scope,
        WorkbenchFile::Manifest,
        existing.map(|artifact| artifact.frontmatter),
        author,
        render_manifest(&pinned),
        now,
    )?;
    Ok(added)
}

/// Löst einen angehefteten Pfad.
///
/// # Rückgabe
/// `true`, wenn der Pfad angeheftet war; `false` (ohne Schreibzugriff) sonst.
///
/// # Fehler
/// Wie [`pin`].
pub fn unpin(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    author: &AgentId,
    absolute_path: &str,
    now: jiff::Timestamp,
) -> KnowledgeResult<bool> {
    scope.validate()?;
    let path = validate_pin_path(absolute_path)?;
    let _guard = WORKBENCH_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let Some(existing) = read_file(store, scope, WorkbenchFile::Manifest)? else {
        return Ok(false);
    };
    let mut pinned = parse_manifest(&existing.body);
    let before = pinned.len();
    pinned.retain(|entry| entry.absolute_path != path);
    if pinned.len() == before {
        return Ok(false);
    }
    write_file(
        store,
        scope,
        WorkbenchFile::Manifest,
        Some(existing.frontmatter),
        author,
        render_manifest(&pinned),
        now,
    )?;
    Ok(true)
}

/// Hängt eine Notiz als `## <RFC3339>`-Abschnitt an `NOTES.md` an.
///
/// # Fehler
/// [`KnowledgeError::Io`] (`InvalidInput`) bei leerem Text oder mehr als
/// [`MAX_NOTE_BYTES`]; sonst Lese-/Schreibfehler.
pub fn append_note(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    author: &AgentId,
    text: &str,
    now: jiff::Timestamp,
) -> KnowledgeResult<()> {
    scope.validate()?;
    let text = text.trim();
    if text.is_empty() {
        return Err(invalid_input("leere Workbench-Notiz".to_owned()));
    }
    if text.len() > MAX_NOTE_BYTES {
        return Err(invalid_input(format!(
            "Workbench-Notiz ist länger als {MAX_NOTE_BYTES} Bytes"
        )));
    }
    let _guard = WORKBENCH_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let existing = read_file(store, scope, WorkbenchFile::Notes)?;
    let (frontmatter, mut body) = match existing {
        Some(artifact) => (Some(artifact.frontmatter), artifact.body),
        None => (None, String::new()),
    };
    body.push_str(&format!("## {now}\n\n{text}\n\n"));
    write_file(
        store,
        scope,
        WorkbenchFile::Notes,
        frontmatter,
        author,
        body,
        now,
    )
}

/// Fügt eine offene Hypothese (`testing`) an.
///
/// # Rückgabe
/// Die 1-basierte Nummer der neuen Hypothese.
///
/// # Fehler
/// [`KnowledgeError::Io`] (`InvalidInput`) bei leerem Text oder Überlänge.
pub fn add_hypothesis(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    author: &AgentId,
    text: &str,
    now: jiff::Timestamp,
) -> KnowledgeResult<usize> {
    scope.validate()?;
    let text = single_line(text, false)?;
    let _guard = WORKBENCH_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let existing = read_file(store, scope, WorkbenchFile::Hypotheses)?;
    let mut hypotheses = existing
        .as_ref()
        .map(|artifact| parse_hypotheses(&artifact.body))
        .unwrap_or_default();
    hypotheses.push(Hypothesis {
        text,
        status: HypothesisStatus::Testing,
    });
    let number = hypotheses.len();
    write_file(
        store,
        scope,
        WorkbenchFile::Hypotheses,
        existing.map(|artifact| artifact.frontmatter),
        author,
        render_hypotheses(&hypotheses),
        now,
    )?;
    Ok(number)
}

/// Bestätigt eine offene Hypothese (`testing -> confirmed`).
///
/// # Argumente
/// - `selector` (`&str`): exakter Hypothesentext, sonst `#<n>` bzw. `<n>`
///   (1-basiert).
///
/// # Fehler
/// [`KnowledgeError::ArtifactNotFound`] für eine unbekannte Hypothese,
/// [`KnowledgeError::IllegalTransition`] für eine bereits entschiedene.
pub fn confirm(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    author: &AgentId,
    selector: &str,
    now: jiff::Timestamp,
) -> KnowledgeResult<Hypothesis> {
    decide(
        store,
        scope,
        author,
        selector,
        HypothesisStatus::Confirmed,
        now,
    )
}

/// Verwirft eine offene Hypothese (`testing -> rejected`).
///
/// # Fehler
/// Wie [`confirm`].
pub fn reject(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    author: &AgentId,
    selector: &str,
    now: jiff::Timestamp,
) -> KnowledgeResult<Hypothesis> {
    decide(
        store,
        scope,
        author,
        selector,
        HypothesisStatus::Rejected,
        now,
    )
}

/// Gemeinsamer Pfad hinter [`confirm`]/[`reject`].
fn decide(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    author: &AgentId,
    selector: &str,
    target: HypothesisStatus,
    now: jiff::Timestamp,
) -> KnowledgeResult<Hypothesis> {
    scope.validate()?;
    let selector = selector.trim();
    let _guard = WORKBENCH_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let Some(existing) = read_file(store, scope, WorkbenchFile::Hypotheses)? else {
        return Err(KnowledgeError::ArtifactNotFound(format!(
            "hypothesis {selector}"
        )));
    };
    let mut hypotheses = parse_hypotheses(&existing.body);
    let index = select_hypothesis(&hypotheses, selector)
        .ok_or_else(|| KnowledgeError::ArtifactNotFound(format!("hypothesis {selector}")))?;
    let Some(hypothesis) = hypotheses.get_mut(index) else {
        return Err(KnowledgeError::ArtifactNotFound(format!(
            "hypothesis {selector}"
        )));
    };
    if hypothesis.status != HypothesisStatus::Testing {
        return Err(KnowledgeError::IllegalTransition {
            from: hypothesis.status.label().to_owned(),
            to: target.label().to_owned(),
        });
    }
    hypothesis.status = target;
    let decided = hypothesis.clone();
    write_file(
        store,
        scope,
        WorkbenchFile::Hypotheses,
        Some(existing.frontmatter),
        author,
        render_hypotheses(&hypotheses),
        now,
    )?;
    Ok(decided)
}

/// Findet eine Hypothese: zuerst exakter Text, dann `#<n>`/`<n>` (1-basiert).
fn select_hypothesis(hypotheses: &[Hypothesis], selector: &str) -> Option<usize> {
    if let Some(index) = hypotheses
        .iter()
        .position(|hypothesis| hypothesis.text == selector)
    {
        return Some(index);
    }
    let number: usize = selector
        .strip_prefix('#')
        .unwrap_or(selector)
        .parse()
        .ok()?;
    (number >= 1 && number <= hypotheses.len()).then(|| number - 1)
}

// --- Dateien -----------------------------------------------------------------

/// Die drei Dateien eines Workbench-Scopes (§1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkbenchFile {
    Notes,
    Manifest,
    Hypotheses,
}

impl WorkbenchFile {
    /// Pfad relativ zum Scope-Verzeichnis, ohne `.md`.
    const fn local(self) -> &'static str {
        match self {
            Self::Notes => "NOTES",
            Self::Manifest => "pinned/MANIFEST",
            Self::Hypotheses => "hypotheses",
        }
    }

    /// Artefakt-Art der Datei.
    const fn kind(self) -> ArtifactKind {
        match self {
            Self::Notes | Self::Manifest => ArtifactKind::WorkbenchNote,
            Self::Hypotheses => ArtifactKind::WorkbenchHypothesis,
        }
    }

    fn path(self, store: &KnowledgeStore, scope: &WorkbenchScope) -> PathBuf {
        let dir = store.workbench_dir(&scope.path_component());
        match self {
            Self::Notes => dir.join("NOTES.md"),
            Self::Manifest => dir.join("pinned").join("MANIFEST.md"),
            Self::Hypotheses => dir.join("hypotheses.md"),
        }
    }

    /// Stabile Id im Format, das `KnowledgeIndex::rebuild` aus dem Pfad ableitet.
    fn artifact_id(self, scope: &WorkbenchScope) -> ArtifactId {
        ArtifactId::new(format!(
            "workbench/{}/{}",
            scope.path_component(),
            self.local()
        ))
    }
}

/// Liest eine Workbench-Datei; `None`, wenn sie (noch) nicht existiert.
fn read_file(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    file: WorkbenchFile,
) -> KnowledgeResult<Option<KnowledgeArtifact>> {
    let path = file.path(store, scope);
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(store.read_artifact(
        &path,
        file.artifact_id(scope),
        file.kind(),
    )?))
}

/// Schreibt eine Workbench-Datei atomar; erhält `created_at`/Autor einer
/// bestehenden Datei.
fn write_file(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    file: WorkbenchFile,
    existing: Option<Frontmatter>,
    author: &AgentId,
    body: String,
    now: jiff::Timestamp,
) -> KnowledgeResult<()> {
    let mut frontmatter = existing
        .unwrap_or_else(|| Frontmatter::new(author.clone(), VisibilityScope::SelfOnly, now));
    frontmatter.touch(now);
    frontmatter.extra.insert(
        SCOPE_KEY.to_owned(),
        serde_json::Value::String(scope.path_component()),
    );
    let artifact = KnowledgeArtifact::new(file.artifact_id(scope), file.kind(), frontmatter, body);
    store.write_artifact(&file.path(store, scope), &artifact)
}

// --- Formate -----------------------------------------------------------------

fn render_manifest(pinned: &[PinnedFile]) -> String {
    let mut out = String::new();
    for entry in pinned {
        out.push_str("- ");
        out.push_str(&entry.absolute_path);
        if !entry.note.is_empty() {
            out.push_str(SEPARATOR);
            out.push_str(&entry.note);
        }
        out.push('\n');
    }
    out
}

fn parse_manifest(body: &str) -> Vec<PinnedFile> {
    body.lines()
        .filter_map(|line| line.trim_end().strip_prefix("- "))
        .map(|rest| match rest.split_once(SEPARATOR) {
            Some((path, note)) => PinnedFile {
                absolute_path: path.trim().to_owned(),
                note: note.trim().to_owned(),
            },
            None => PinnedFile {
                absolute_path: rest.trim().to_owned(),
                note: String::new(),
            },
        })
        .filter(|entry| !entry.absolute_path.is_empty())
        .collect()
}

fn render_hypotheses(hypotheses: &[Hypothesis]) -> String {
    let mut out = String::new();
    for hypothesis in hypotheses {
        let checkbox = if hypothesis.status == HypothesisStatus::Testing {
            "[ ]"
        } else {
            "[x]"
        };
        out.push_str(&format!(
            "- {checkbox} {}{STATUS_MARKER}{}\n",
            hypothesis.text,
            hypothesis.status.label()
        ));
    }
    out
}

fn parse_hypotheses(body: &str) -> Vec<Hypothesis> {
    body.lines()
        .filter_map(|line| {
            let line = line.trim_end();
            let rest = line
                .strip_prefix("- [ ] ")
                .or_else(|| line.strip_prefix("- [x] "))?;
            let (text, status) = rest.rsplit_once(STATUS_MARKER)?;
            Some(Hypothesis {
                text: text.trim().to_owned(),
                status: HypothesisStatus::from_label(status)?,
            })
        })
        .collect()
}

// --- Validierung -------------------------------------------------------------

fn invalid_input(detail: String) -> KnowledgeError {
    KnowledgeError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        detail,
    ))
}

/// Faltet Zeilenumbrüche zu Leerzeichen, trimmt und prüft die Länge.
fn single_line(text: &str, allow_empty: bool) -> KnowledgeResult<String> {
    let folded: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let folded = folded.trim().to_owned();
    if folded.is_empty() && !allow_empty {
        return Err(invalid_input("leerer Workbench-Eintrag".to_owned()));
    }
    if folded.len() > MAX_LINE_BYTES {
        return Err(invalid_input(format!(
            "Workbench-Eintrag ist länger als {MAX_LINE_BYTES} Bytes"
        )));
    }
    Ok(folded)
}

/// Verlangt einen absoluten, steuerzeichenfreien Pfad in Grenzen.
fn validate_pin_path(raw: &str) -> KnowledgeResult<String> {
    let trimmed = raw.trim();
    if trimmed.chars().any(char::is_control) || trimmed.contains(SEPARATOR) {
        return Err(invalid_input(format!(
            "Pfad enthält unzulässige Zeichen: {trimmed:?}"
        )));
    }
    if trimmed.len() > MAX_LINE_BYTES {
        return Err(invalid_input(format!(
            "Pfad ist länger als {MAX_LINE_BYTES} Bytes"
        )));
    }
    if !Path::new(trimmed).is_absolute() {
        return Err(invalid_input(format!(
            "nur absolute Pfade dürfen angeheftet werden: {trimmed}"
        )));
    }
    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn temporary_store(label: &str) -> TestResult<KnowledgeStore> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-knowledge-workbench-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).map_err(ctx("create temporary knowledge root"))?;
        Ok(KnowledgeStore::new(&root))
    }

    fn session() -> WorkbenchScope {
        WorkbenchScope::Session("s-1".to_owned())
    }

    fn author() -> AgentId {
        AgentId::new("agent-1")
    }

    fn at(second: i64) -> TestResult<jiff::Timestamp> {
        Ok(jiff::Timestamp::from_second(1_700_000_000 + second)?)
    }

    fn absolute(name: &str) -> String {
        std::env::temp_dir()
            .join(name)
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn scopes_are_namespaced_in_the_filesystem() {
        assert_eq!(
            WorkbenchScope::Project("harwness".to_owned()).path_component(),
            "project:harwness"
        );
    }

    #[test]
    fn scope_parse_accepts_session_and_project_only() -> TestResult {
        assert_eq!(
            WorkbenchScope::parse("session", "abc")?,
            WorkbenchScope::Session("abc".to_owned())
        );
        assert_eq!(
            WorkbenchScope::parse("project:harw", "abc")?,
            WorkbenchScope::Project("harw".to_owned())
        );
        assert!(WorkbenchScope::parse("global", "abc").is_err());
        assert!(WorkbenchScope::parse("project:../etc", "abc").is_err());
        assert!(WorkbenchScope::parse("project:", "abc").is_err());
        Ok(())
    }

    #[test]
    fn load_of_a_fresh_scope_is_empty() -> TestResult {
        let store = temporary_store("fresh")?;
        let workbench = load(&store, &session())?;
        assert!(workbench.is_empty());
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn pin_replace_and_unpin_round_trip() -> TestResult {
        let store = temporary_store("pin")?;
        let path = absolute("pinned-a.rs");
        assert!(pin(
            &store,
            &session(),
            &author(),
            &path,
            "Einstieg",
            at(0)?
        )?);
        assert!(!pin(
            &store,
            &session(),
            &author(),
            &path,
            "neu begründet",
            at(1)?
        )?);
        let other = absolute("pinned-b.rs");
        assert!(pin(&store, &session(), &author(), &other, "", at(2)?)?);

        let workbench = load(&store, &session())?;
        assert_eq!(
            workbench.pinned,
            vec![
                PinnedFile {
                    absolute_path: path.clone(),
                    note: "neu begründet".to_owned()
                },
                PinnedFile {
                    absolute_path: other.clone(),
                    note: String::new()
                },
            ]
        );

        assert!(unpin(&store, &session(), &author(), &path, at(3)?)?);
        assert!(!unpin(&store, &session(), &author(), &path, at(4)?)?);
        assert_eq!(load(&store, &session())?.pinned.len(), 1);
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn pin_refuses_relative_paths() -> TestResult {
        let store = temporary_store("pin-relative")?;
        let Err(error) = pin(&store, &session(), &author(), "src/lib.rs", "", at(0)?) else {
            return Err(TestError::Unexpected(
                "a relative path must be refused".to_owned(),
            ));
        };
        assert!(matches!(error, KnowledgeError::Io(_)));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn notes_append_and_tail() -> TestResult {
        let store = temporary_store("notes")?;
        append_note(&store, &session(), &author(), "erste Notiz", at(0)?)?;
        append_note(&store, &session(), &author(), "zweite\nmehrzeilig", at(1)?)?;
        let workbench = load(&store, &session())?;
        assert!(workbench.notes.contains("erste Notiz"));
        assert!(workbench.notes.contains("zweite\nmehrzeilig"));
        assert_eq!(workbench.notes_tail(1), vec!["mehrzeilig"]);
        assert!(append_note(&store, &session(), &author(), "   ", at(2)?).is_err());
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn hypotheses_add_confirm_reject_and_refuse_a_second_decision() -> TestResult {
        let store = temporary_store("hypotheses")?;
        assert_eq!(
            add_hypothesis(&store, &session(), &author(), "Cache ist kalt", at(0)?)?,
            1
        );
        assert_eq!(
            add_hypothesis(&store, &session(), &author(), "Lock wird gehalten", at(1)?)?,
            2
        );

        let confirmed = confirm(&store, &session(), &author(), "Cache ist kalt", at(2)?)?;
        assert_eq!(confirmed.status, HypothesisStatus::Confirmed);
        let rejected = reject(&store, &session(), &author(), "#2", at(3)?)?;
        assert_eq!(rejected.text, "Lock wird gehalten");

        let Err(error) = reject(&store, &session(), &author(), "1", at(4)?) else {
            return Err(TestError::Unexpected(
                "a decided hypothesis must not be decided twice".to_owned(),
            ));
        };
        assert!(matches!(error, KnowledgeError::IllegalTransition { .. }));

        let Err(missing) = confirm(&store, &session(), &author(), "#9", at(5)?) else {
            return Err(TestError::Unexpected(
                "an unknown hypothesis must be reported".to_owned(),
            ));
        };
        assert!(matches!(missing, KnowledgeError::ArtifactNotFound(_)));

        let workbench = load(&store, &session())?;
        assert_eq!(workbench.hypotheses.len(), 2);
        assert!(workbench.open_hypotheses().is_empty());
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn workbench_files_stay_indexable() -> TestResult {
        let store = temporary_store("indexable")?;
        append_note(&store, &session(), &author(), "notiz", at(0)?)?;
        add_hypothesis(&store, &session(), &author(), "h", at(1)?)?;
        pin(&store, &session(), &author(), &absolute("x.rs"), "", at(2)?)?;
        let (_index, report) = crate::index::KnowledgeIndex::rebuild_with_report(&store)?;
        assert!(report.is_clean(), "workbench files must parse as artifacts");
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn scopes_are_isolated() -> TestResult {
        let store = temporary_store("isolated")?;
        append_note(&store, &session(), &author(), "nur Sitzung", at(0)?)?;
        let project = WorkbenchScope::Project("harw".to_owned());
        assert!(load(&store, &project)?.is_empty());
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }
}
