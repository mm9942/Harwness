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
//! - `retention.json` — optionale Aufbewahrungsregel des Scopes
//!   ([`Retention`]); kein Artefakt, darum kein `.md` (der Index ignoriert es).
//!
//! # Notizen
//! Jede Notiz ist ein Abschnitt `## <RFC3339>` in `NOTES.md`. Sie ist über
//! ihre 1-basierte Nummer (`<n>`/`#<n>`, Dateireihenfolge) **oder** stabil
//! über ihren Zeitstempel adressierbar ([`edit_note`], [`remove_note`]). Nur
//! Überschriften, deren Rest ein gültiger Zeitstempel ist, trennen Notizen;
//! eine Zeile dieser Form im Notiztext wird darum abgelehnt.
//!
//! # Aufbewahrung
//! [`prune_expired`] räumt abgelaufene Scopes ab (Standard: Sitzungs-Scopes
//! nach [`DEFAULT_SESSION_RETENTION_DAYS`] Tagen ohne Änderung, Projekte nie);
//! [`purge_scope`] entfernt einen Scope sofort (Sitzung archiviert). Beide
//! sind reine API für die Dream-Wartung (D5), es gibt keinen Scheduler hier.
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
//! Modell-Werkzeug derselben Sitzung) keinen Eintrag verlieren; zusätzlich
//! hält jeder Zyklus eine prozessübergreifende [`KnowledgeLock`] auf
//! `workbench/<scope>/.workbench.lock` (§Open-4 des Entwurfs: zweite TUI,
//! Gateway).
//!
//! # Fehler
//! Ungültige Eingaben (leerer Text, relativer Pfad, unsicherer Scope,
//! Überlänge) werden als [`KnowledgeError::Io`] mit
//! [`std::io::ErrorKind::InvalidInput`] gemeldet — dieselbe Konvention wie
//! `store::ensure_path_component`. Unbekannte Hypothesen melden
//! [`KnowledgeError::ArtifactNotFound`], ein zweiter Entscheid über dieselbe
//! Hypothese [`KnowledgeError::IllegalTransition`].

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::artifact::{ArtifactId, ArtifactKind, Frontmatter, KnowledgeArtifact};
use crate::error::{KnowledgeError, KnowledgeResult};
use crate::lock::KnowledgeLock;
use crate::store::KnowledgeStore;
use crate::visibility::{AgentId, VisibilityScope};

/// Serialisiert die Read-Modify-Write-Zyklen dieses Moduls prozessweit.
static WORKBENCH_LOCK: Mutex<()> = Mutex::new(());

/// Name der Sperrdatei im Scope-Verzeichnis.
const SCOPE_LOCK_FILE: &str = ".workbench.lock";

/// Hält Prozess-Mutex und Scope-Dateisperre für einen Read-Modify-Write-Zyklus.
struct ScopeGuard {
    _file: KnowledgeLock,
    _thread: MutexGuard<'static, ()>,
}

/// Sperrt einen Scope: erst den Prozess-Mutex, dann die Datei (feste
/// Reihenfolge, damit Threads nicht auf der Datei kreiseln).
fn lock_scope(store: &KnowledgeStore, scope: &WorkbenchScope) -> KnowledgeResult<ScopeGuard> {
    let thread = WORKBENCH_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let file = KnowledgeLock::acquire(
        &store
            .workbench_dir(&scope.path_component())
            .join(SCOPE_LOCK_FILE),
    )?;
    Ok(ScopeGuard {
        _file: file,
        _thread: thread,
    })
}

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

    /// Umkehrung von [`Self::path_component`]; `None` für fremde oder
    /// unsichere Verzeichnisnamen.
    #[must_use]
    pub fn from_path_component(component: &str) -> Option<Self> {
        let scope = if let Some(id) = component.strip_prefix("session:") {
            Self::Session(id.to_owned())
        } else {
            Self::Project(component.strip_prefix("project:")?.to_owned())
        };
        scope.validate().ok().map(|()| scope)
    }

    /// Leitet den Projekt-Scope aus einem Projektverzeichnis ab (letzte
    /// Pfadkomponente, klein geschrieben, alles außer `[a-z0-9._-]` wird `-`).
    ///
    /// # Rückgabe
    /// `None`, wenn der Pfad keinen brauchbaren Namen trägt (z. B. `/`).
    #[must_use]
    pub fn project_for_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?;
        let slug: String = name
            .chars()
            .map(|c| {
                let c = c.to_ascii_lowercase();
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let slug = slug.trim_matches('-').to_owned();
        let scope = Self::Project(slug);
        scope.validate().ok().map(|()| scope)
    }

    /// `true` für einen Sitzungs-Scope.
    #[must_use]
    pub const fn is_session(&self) -> bool {
        matches!(self, Self::Session(_))
    }
}

/// Eine angeheftete Datei: absoluter Pfad plus Begründung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedFile {
    /// Absoluter Pfad (nie eine Kopie, nie ein Symlink).
    pub absolute_path: String,
    /// Einzeilige Begründung; darf leer sein.
    pub note: String,
    /// Zeitpunkt des (letzten) Anheftens; `None` bei Altbeständen. Liegt im
    /// Frontmatter (`extra.pinned_at`), nicht in der Manifestzeile.
    pub pinned_at: Option<jiff::Timestamp>,
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

    /// Die Notizen als adressierbare Einträge in Dateireihenfolge.
    #[must_use]
    pub fn note_entries(&self) -> Vec<WorkbenchNote> {
        parse_notes(&self.notes).1
    }
}

/// Eine einzelne Notiz aus `NOTES.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbenchNote {
    /// 1-basierte Nummer in Dateireihenfolge (verschiebt sich nach `rm`).
    pub number: usize,
    /// Zeitstempel der Überschrift (RFC3339) — die stabile Id der Notiz.
    pub recorded_at: String,
    /// Notiztext ohne umgebende Leerzeilen.
    pub text: String,
}

/// Obergrenze für einen Digest (Werkzeug `workbench.show`, Kontextbeitrag).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DigestOptions {
    /// Höchstzahl Bytes des gesamten Texts (UTF-8-sicher gekürzt).
    pub max_bytes: usize,
    /// Anzahl Notizzeilen aus dem Tail; `0` lässt Notizen weg.
    pub notes_lines: usize,
    /// `true`: auch entschiedene Hypothesen aufführen.
    pub include_decided: bool,
}

/// Rendert einen gedeckelten Klartext-Überblick eines Scopes.
///
/// # Beschreibung
/// Reihenfolge: Pins, offene Hypothesen (mit `#<n>`), optional entschiedene,
/// optional Notiz-Tail. Überschreitet der Text `max_bytes`, wird an einer
/// Zeichengrenze gekürzt und `…(gekürzt)` angehängt; das Ergebnis ist nie
/// länger als `max_bytes`. Ein leerer Scope ergibt einen leeren String.
#[must_use]
pub fn digest(bench: &Workbench, options: DigestOptions) -> String {
    let mut out = String::new();
    if !bench.pinned.is_empty() {
        out.push_str("Angeheftet:\n");
        for entry in &bench.pinned {
            out.push_str("- ");
            out.push_str(&entry.absolute_path);
            if !entry.note.is_empty() {
                out.push_str(SEPARATOR);
                out.push_str(&entry.note);
            }
            out.push('\n');
        }
    }
    let open = bench.open_hypotheses();
    if !open.is_empty() {
        out.push_str("Offene Hypothesen:\n");
        for (number, hypothesis) in &open {
            out.push_str(&format!("- #{number} {}\n", hypothesis.text));
        }
    }
    if options.include_decided {
        let decided: Vec<(usize, &Hypothesis)> = bench
            .hypotheses
            .iter()
            .enumerate()
            .filter(|(_, hypothesis)| hypothesis.status != HypothesisStatus::Testing)
            .map(|(index, hypothesis)| (index + 1, hypothesis))
            .collect();
        if !decided.is_empty() {
            out.push_str("Entschiedene Hypothesen:\n");
            for (number, hypothesis) in decided {
                out.push_str(&format!(
                    "- #{number} [{}] {}\n",
                    hypothesis.status.label(),
                    hypothesis.text
                ));
            }
        }
    }
    if options.notes_lines > 0 {
        let tail = bench.notes_tail(options.notes_lines);
        if !tail.is_empty() {
            out.push_str("Notizen (Ende):\n");
            for line in tail {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    cap_bytes(out, options.max_bytes)
}

/// Höchstzahl Bytes, die [`inspect_pin`] für die Vorschau liest.
pub const PIN_PREVIEW_MAX_BYTES: usize = 4 * 1024;

/// Höchstzahl Zeichen je Vorschauzeile.
const PIN_PREVIEW_LINE_CHARS: usize = 160;

/// Zustand einer angehefteten Datei auf der Platte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinState {
    /// Vorhanden, seit dem Anheften nicht geändert (oder ohne Zeitpunkt).
    Present,
    /// Vorhanden, nach dem Anheften geändert (Mtime > `pinned_at`).
    Changed,
    /// Existiert nicht (mehr).
    Missing,
    /// Existiert, ist aber keine reguläre Datei (z. B. ein Verzeichnis).
    NotAFile,
}

impl PinState {
    /// Stabile Bezeichnung für `OpOutput::data`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Changed => "changed",
            Self::Missing => "missing",
            Self::NotAFile => "not_a_file",
        }
    }
}

/// Ergebnis von [`inspect_pin`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinInspection {
    /// Zustand der Datei.
    pub state: PinState,
    /// Die ersten Zeilen (höchstens `preview_lines`, je Zeile gekürzt);
    /// leer bei fehlender Datei, `["(binär)"]` bei Binärinhalt.
    pub preview: Vec<String>,
}

/// Prüft eine angeheftete Datei und liest eine kurze Vorschau.
///
/// # Beschreibung
/// Nur für die Operator-Sicht (TUI-Pane, `/workbench show`): liest höchstens
/// [`PIN_PREVIEW_MAX_BYTES`] vom Anfang der Datei. Das Modell-Werkzeug
/// `workbench.show` ruft dies bewusst **nicht** auf, weil Pins außerhalb der
/// Sandbox liegen dürfen.
#[must_use]
pub fn inspect_pin(entry: &PinnedFile, preview_lines: usize) -> PinInspection {
    use std::io::Read as _;
    let path = Path::new(&entry.absolute_path);
    let Ok(meta) = std::fs::metadata(path) else {
        return PinInspection {
            state: PinState::Missing,
            preview: Vec::new(),
        };
    };
    if !meta.is_file() {
        return PinInspection {
            state: PinState::NotAFile,
            preview: Vec::new(),
        };
    }
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| jiff::Timestamp::try_from(time).ok());
    let state = match (entry.pinned_at, modified) {
        (Some(pinned), Some(modified)) if modified > pinned => PinState::Changed,
        _ => PinState::Present,
    };
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path).and_then(|file| {
        file.take(PIN_PREVIEW_MAX_BYTES as u64)
            .read_to_end(&mut bytes)
    });
    let preview = if preview_lines == 0 || read.is_err() {
        Vec::new()
    } else if bytes.contains(&0) {
        vec!["(binär)".to_owned()]
    } else {
        String::from_utf8_lossy(&bytes)
            .lines()
            .take(preview_lines)
            .map(|line| line.chars().take(PIN_PREVIEW_LINE_CHARS).collect())
            .collect()
    };
    PinInspection { state, preview }
}

/// Kürzt `text` UTF-8-sicher auf höchstens `max_bytes` inklusive Marker.
fn cap_bytes(mut text: String, max_bytes: usize) -> String {
    const MARKER: &str = "…(gekürzt)";
    if text.len() <= max_bytes {
        return text;
    }
    let mut cut = max_bytes.saturating_sub(MARKER.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    if MARKER.len() <= max_bytes {
        text.push_str(MARKER);
    }
    text
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
        .map(|artifact| {
            let mut pinned = parse_manifest(&artifact.body);
            attach_pin_times(&mut pinned, &artifact.frontmatter);
            pinned
        })
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
    let _guard = lock_scope(store, scope)?;
    let existing = read_file(store, scope, WorkbenchFile::Manifest)?;
    let (mut pinned, mut frontmatter) = match existing {
        Some(artifact) => {
            let mut pinned = parse_manifest(&artifact.body);
            attach_pin_times(&mut pinned, &artifact.frontmatter);
            (pinned, artifact.frontmatter)
        }
        None => (
            Vec::new(),
            Frontmatter::new(author.clone(), VisibilityScope::SelfOnly, now),
        ),
    };
    let added = match pinned.iter_mut().find(|entry| entry.absolute_path == path) {
        Some(entry) => {
            entry.note = note;
            entry.pinned_at = Some(now);
            false
        }
        None => {
            pinned.push(PinnedFile {
                absolute_path: path,
                note,
                pinned_at: Some(now),
            });
            true
        }
    };
    store_pin_times(&mut frontmatter, &pinned);
    write_file(
        store,
        scope,
        WorkbenchFile::Manifest,
        Some(frontmatter),
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
    let _guard = lock_scope(store, scope)?;
    let Some(existing) = read_file(store, scope, WorkbenchFile::Manifest)? else {
        return Ok(false);
    };
    let mut pinned = parse_manifest(&existing.body);
    let mut frontmatter = existing.frontmatter;
    attach_pin_times(&mut pinned, &frontmatter);
    let before = pinned.len();
    pinned.retain(|entry| entry.absolute_path != path);
    if pinned.len() == before {
        return Ok(false);
    }
    store_pin_times(&mut frontmatter, &pinned);
    write_file(
        store,
        scope,
        WorkbenchFile::Manifest,
        Some(frontmatter),
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
    let text = validate_note_text(text)?;
    let _guard = lock_scope(store, scope)?;
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

/// Ersetzt den Text einer Notiz; ihr Zeitstempel (die Id) bleibt.
///
/// # Argumente
/// - `selector` (`&str`): `<n>`/`#<n>` (1-basiert) oder der exakte
///   Zeitstempel der Notiz.
/// - `text` (`&str`): der neue Text (Regeln wie bei [`append_note`]).
///
/// # Rückgabe
/// Die Notiz mit neuem Text.
///
/// # Fehler
/// [`KnowledgeError::ArtifactNotFound`] für eine unbekannte Notiz,
/// [`KnowledgeError::Io`] (`InvalidInput`) für ungültigen Text.
pub fn edit_note(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    author: &AgentId,
    selector: &str,
    text: &str,
    now: jiff::Timestamp,
) -> KnowledgeResult<WorkbenchNote> {
    scope.validate()?;
    let text = validate_note_text(text)?;
    rewrite_notes(store, scope, author, selector, now, |notes, index| {
        let note = notes
            .get_mut(index)
            .ok_or_else(|| KnowledgeError::ArtifactNotFound(format!("note {selector}")))?;
        note.text = text.to_owned();
        Ok(note.clone())
    })
}

/// Löscht eine Notiz; nachfolgende Nummern rücken auf, Zeitstempel bleiben.
///
/// # Rückgabe
/// Die gelöschte Notiz.
///
/// # Fehler
/// Wie [`edit_note`].
pub fn remove_note(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    author: &AgentId,
    selector: &str,
    now: jiff::Timestamp,
) -> KnowledgeResult<WorkbenchNote> {
    scope.validate()?;
    rewrite_notes(store, scope, author, selector, now, |notes, index| {
        if index >= notes.len() {
            return Err(KnowledgeError::ArtifactNotFound(format!("note {selector}")));
        }
        Ok(notes.remove(index))
    })
}

/// Read-Modify-Write von `NOTES.md` unter der Scope-Sperre.
fn rewrite_notes(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    author: &AgentId,
    selector: &str,
    now: jiff::Timestamp,
    change: impl FnOnce(&mut Vec<WorkbenchNote>, usize) -> KnowledgeResult<WorkbenchNote>,
) -> KnowledgeResult<WorkbenchNote> {
    let selector = selector.trim();
    let not_found = || KnowledgeError::ArtifactNotFound(format!("note {selector}"));
    let _guard = lock_scope(store, scope)?;
    let existing = read_file(store, scope, WorkbenchFile::Notes)?.ok_or_else(not_found)?;
    let (preamble, mut notes) = parse_notes(&existing.body);
    let index = select_note(&notes, selector).ok_or_else(not_found)?;
    let changed = change(&mut notes, index)?;
    write_file(
        store,
        scope,
        WorkbenchFile::Notes,
        Some(existing.frontmatter),
        author,
        render_notes(&preamble, &notes),
        now,
    )?;
    Ok(changed)
}

/// Findet eine Notiz: exakter Zeitstempel, sonst `#<n>`/`<n>` (1-basiert).
fn select_note(notes: &[WorkbenchNote], selector: &str) -> Option<usize> {
    if let Some(index) = notes.iter().position(|note| note.recorded_at == selector) {
        return Some(index);
    }
    let number: usize = selector
        .strip_prefix('#')
        .unwrap_or(selector)
        .parse()
        .ok()?;
    (number >= 1 && number <= notes.len()).then(|| number - 1)
}

// --- Aufbewahrung ------------------------------------------------------------

/// Standard-Aufbewahrung eines Sitzungs-Scopes in Tagen ohne Änderung.
pub const DEFAULT_SESSION_RETENTION_DAYS: u32 = 14;

/// Dateiname der Aufbewahrungsregel im Scope-Verzeichnis.
const RETENTION_FILE: &str = "retention.json";

/// Aufbewahrungsregel eines Scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum Retention {
    /// Nie automatisch aufräumen (Standard für Projekte).
    Keep,
    /// Aufräumen, wenn die letzte Änderung älter als `days` Tage ist
    /// (Standard für Sitzungen: [`DEFAULT_SESSION_RETENTION_DAYS`]).
    ExpireAfterDays {
        /// Tage ohne Änderung bis zum Aufräumen (mindestens 1).
        days: u32,
    },
}

impl Retention {
    /// Die Vorgabe je Scope-Art.
    #[must_use]
    pub const fn default_for(scope: &WorkbenchScope) -> Self {
        match scope {
            WorkbenchScope::Session(_) => Self::ExpireAfterDays {
                days: DEFAULT_SESSION_RETENTION_DAYS,
            },
            WorkbenchScope::Project(_) => Self::Keep,
        }
    }

    /// Kurzform für Anzeige und Grammatik: `keep` bzw. `<n>d`.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Keep => "keep".to_owned(),
            Self::ExpireAfterDays { days } => format!("{days}d"),
        }
    }

    /// Umkehrung von [`Self::label`] (`keep`, `<n>d` oder `<n>`, `n >= 1`).
    ///
    /// # Fehler
    /// [`KnowledgeError::Io`] (`InvalidInput`) für jede andere Form.
    pub fn parse(raw: &str) -> KnowledgeResult<Self> {
        let raw = raw.trim();
        if raw == "keep" {
            return Ok(Self::Keep);
        }
        match raw.strip_suffix('d').unwrap_or(raw).parse::<u32>() {
            Ok(days) if days >= 1 => Ok(Self::ExpireAfterDays { days }),
            _ => Err(invalid_input(format!(
                "ungültige Aufbewahrung '{raw}' (keep | <tage>d)"
            ))),
        }
    }
}

/// Liest die Aufbewahrungsregel eines Scopes; ohne Datei die Vorgabe.
///
/// # Fehler
/// [`KnowledgeError::Io`] bei unsicherem Scope, Lesefehler oder kaputtem JSON.
pub fn retention(store: &KnowledgeStore, scope: &WorkbenchScope) -> KnowledgeResult<Retention> {
    scope.validate()?;
    let path = store
        .workbench_dir(&scope.path_component())
        .join(RETENTION_FILE);
    if !path.is_file() {
        return Ok(Retention::default_for(scope));
    }
    let raw = std::fs::read_to_string(&path)?;
    serde_json::from_str(&raw).map_err(|error| {
        invalid_input(format!(
            "kaputte Aufbewahrungsregel {}: {error}",
            path.display()
        ))
    })
}

/// Setzt die Aufbewahrungsregel eines Scopes (atomar, unter der Scope-Sperre).
///
/// # Fehler
/// [`KnowledgeError::Io`] bei unsicherem Scope, `days == 0` oder Schreibfehler.
pub fn set_retention(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    policy: Retention,
) -> KnowledgeResult<()> {
    scope.validate()?;
    if policy == (Retention::ExpireAfterDays { days: 0 }) {
        return Err(invalid_input("Aufbewahrung von 0 Tagen".to_owned()));
    }
    let _guard = lock_scope(store, scope)?;
    let json = serde_json::to_string_pretty(&policy)
        .map_err(|error| invalid_input(format!("Aufbewahrung nicht serialisierbar: {error}")))?;
    crate::store::write_atomic(
        &store
            .workbench_dir(&scope.path_component())
            .join(RETENTION_FILE),
        &json,
    )
}

/// Ergebnis von [`prune_expired`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// Entfernte Scopes.
    pub removed: Vec<WorkbenchScope>,
    /// Behaltene Scopes (Regel `keep` oder noch nicht abgelaufen).
    pub kept: Vec<WorkbenchScope>,
    /// Verzeichnisse, die übersprungen wurden (fremder Name, Symlink,
    /// Lesefehler) mit Grund.
    pub skipped: Vec<(PathBuf, String)>,
}

/// Räumt abgelaufene Workbench-Scopes ab (Wartung, D5).
///
/// # Beschreibung
/// Geht jedes Verzeichnis unter `workbench/` durch, liest seine
/// [`Retention`] und die letzte Änderung (jüngstes `updated_at` der drei
/// Dateien, sonst die Verzeichnis-Mtime) und entfernt den Scope, wenn
/// `now - letzte Änderung > days`. `keep` wird nie entfernt. Symlinks und
/// fremde Namen werden nie angefasst. Ein Fehler an einem Scope bricht den
/// Lauf nicht ab, sondern landet in [`PruneReport::skipped`].
///
/// # Fehler
/// [`KnowledgeError::Io`] nur, wenn `workbench/` selbst nicht lesbar ist
/// (ein fehlendes Verzeichnis ergibt einen leeren Bericht).
pub fn prune_expired(store: &KnowledgeStore, now: jiff::Timestamp) -> KnowledgeResult<PruneReport> {
    let mut report = PruneReport::default();
    let root = store.root().join("workbench");
    if !root.is_dir() {
        return Ok(report);
    }
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path();
        let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_dir());
        let scope = entry
            .file_name()
            .to_str()
            .and_then(WorkbenchScope::from_path_component);
        let Some(scope) = scope.filter(|_| is_dir) else {
            report
                .skipped
                .push((path, "kein Workbench-Scope-Verzeichnis".to_owned()));
            continue;
        };
        match scope_expired(store, &scope, now) {
            Ok(true) => match purge_scope(store, &scope) {
                Ok(_) => report.removed.push(scope),
                Err(error) => report.skipped.push((path, error.to_string())),
            },
            Ok(false) => report.kept.push(scope),
            Err(error) => report.skipped.push((path, error.to_string())),
        }
    }
    Ok(report)
}

/// `true`, wenn der Scope nach seiner Regel abgelaufen ist.
fn scope_expired(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
    now: jiff::Timestamp,
) -> KnowledgeResult<bool> {
    let Retention::ExpireAfterDays { days } = retention(store, scope)? else {
        return Ok(false);
    };
    let Some(last) = last_activity(store, scope)? else {
        return Ok(false);
    };
    let age = now.as_second().saturating_sub(last.as_second());
    Ok(age > i64::from(days).saturating_mul(86_400))
}

/// Jüngste Änderung eines Scopes: `updated_at` der Dateien, sonst Mtime.
fn last_activity(
    store: &KnowledgeStore,
    scope: &WorkbenchScope,
) -> KnowledgeResult<Option<jiff::Timestamp>> {
    let mut latest: Option<jiff::Timestamp> = None;
    for file in [
        WorkbenchFile::Notes,
        WorkbenchFile::Manifest,
        WorkbenchFile::Hypotheses,
    ] {
        if let Some(artifact) = read_file(store, scope, file)? {
            let updated = artifact.frontmatter.updated_at;
            latest = Some(latest.map_or(updated, |current| current.max(updated)));
        }
    }
    if latest.is_some() {
        return Ok(latest);
    }
    let modified = std::fs::metadata(store.workbench_dir(&scope.path_component()))
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| jiff::Timestamp::try_from(time).ok());
    Ok(modified)
}

/// Entfernt einen Scope vollständig (z. B. wenn seine Sitzung archiviert
/// wurde). Hält dabei die Scope-Sperre.
///
/// # Rückgabe
/// `true`, wenn ein Verzeichnis entfernt wurde; `false`, wenn es fehlte.
///
/// # Fehler
/// [`KnowledgeError::Io`] bei unsicherem Scope, wenn der Scope-Pfad ein
/// Symlink ist, oder bei Löschfehlern.
pub fn purge_scope(store: &KnowledgeStore, scope: &WorkbenchScope) -> KnowledgeResult<bool> {
    scope.validate()?;
    let dir = store.workbench_dir(&scope.path_component());
    match std::fs::symlink_metadata(&dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
        Ok(meta) if !meta.file_type().is_dir() => {
            return Err(invalid_input(format!(
                "Workbench-Scope ist kein Verzeichnis: {}",
                dir.display()
            )));
        }
        Ok(_) => {}
    }
    let _guard = lock_scope(store, scope)?;
    std::fs::remove_dir_all(&dir)?;
    Ok(true)
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
    let _guard = lock_scope(store, scope)?;
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
    let _guard = lock_scope(store, scope)?;
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
                pinned_at: None,
            },
            None => PinnedFile {
                absolute_path: rest.trim().to_owned(),
                note: String::new(),
                pinned_at: None,
            },
        })
        .filter(|entry| !entry.absolute_path.is_empty())
        .collect()
}

/// `Some(zeitstempel)`, wenn `line` eine Notiz-Überschrift `## <RFC3339>` ist.
fn note_heading(line: &str) -> Option<&str> {
    let stamp = line.trim_end().strip_prefix("## ")?.trim();
    stamp.parse::<jiff::Timestamp>().ok().map(|_| stamp)
}

/// Zerlegt `NOTES.md` in Vorspann (Text vor der ersten Überschrift) und Notizen.
fn parse_notes(body: &str) -> (String, Vec<WorkbenchNote>) {
    let mut preamble = String::new();
    let mut notes: Vec<WorkbenchNote> = Vec::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    let finish = |current: Option<(String, Vec<&str>)>, notes: &mut Vec<WorkbenchNote>| {
        if let Some((recorded_at, lines)) = current {
            notes.push(WorkbenchNote {
                number: notes.len() + 1,
                recorded_at,
                text: lines.join("\n").trim().to_owned(),
            });
        }
    };
    for line in body.lines() {
        if let Some(stamp) = note_heading(line) {
            finish(current.take(), &mut notes);
            current = Some((stamp.to_owned(), Vec::new()));
        } else if let Some((_, lines)) = current.as_mut() {
            lines.push(line);
        } else {
            preamble.push_str(line);
            preamble.push('\n');
        }
    }
    finish(current, &mut notes);
    (preamble, notes)
}

/// Umkehrung von [`parse_notes`] im Format von [`append_note`].
fn render_notes(preamble: &str, notes: &[WorkbenchNote]) -> String {
    let mut out = String::new();
    if !preamble.trim().is_empty() {
        out.push_str(preamble.trim_end());
        out.push_str("\n\n");
    }
    for note in notes {
        out.push_str(&format!("## {}\n\n{}\n\n", note.recorded_at, note.text));
    }
    out
}

/// Frontmatter-`extra`-Schlüssel der Anheft-Zeitpunkte (`{pfad: RFC3339}`).
const PINNED_AT_KEY: &str = "pinned_at";

/// Übernimmt die Anheft-Zeitpunkte aus dem Frontmatter (tolerant).
fn attach_pin_times(pinned: &mut [PinnedFile], frontmatter: &Frontmatter) {
    let Some(times) = frontmatter
        .extra
        .get(PINNED_AT_KEY)
        .and_then(serde_json::Value::as_object)
    else {
        return;
    };
    for entry in pinned {
        entry.pinned_at = times
            .get(&entry.absolute_path)
            .and_then(serde_json::Value::as_str)
            .and_then(|raw| raw.parse().ok());
    }
}

/// Schreibt die Anheft-Zeitpunkte der aktuellen Pins ins Frontmatter.
fn store_pin_times(frontmatter: &mut Frontmatter, pinned: &[PinnedFile]) {
    let times: serde_json::Map<String, serde_json::Value> = pinned
        .iter()
        .filter_map(|entry| {
            entry.pinned_at.map(|at| {
                (
                    entry.absolute_path.clone(),
                    serde_json::Value::String(at.to_string()),
                )
            })
        })
        .collect();
    frontmatter
        .extra
        .insert(PINNED_AT_KEY.to_owned(), serde_json::Value::Object(times));
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

/// Prüft einen Notiztext: nicht leer, höchstens [`MAX_NOTE_BYTES`], keine
/// Zeile in Form einer Notiz-Überschrift (sie würde die Notiz spalten).
fn validate_note_text(text: &str) -> KnowledgeResult<&str> {
    let text = text.trim();
    if text.is_empty() {
        return Err(invalid_input("leere Workbench-Notiz".to_owned()));
    }
    if text.len() > MAX_NOTE_BYTES {
        return Err(invalid_input(format!(
            "Workbench-Notiz ist länger als {MAX_NOTE_BYTES} Bytes"
        )));
    }
    if text.lines().any(|line| note_heading(line).is_some()) {
        return Err(invalid_input(
            "Workbench-Notiz enthält eine Zeile '## <Zeitstempel>' (reserviert als Trenner)"
                .to_owned(),
        ));
    }
    Ok(text)
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
                    note: "neu begründet".to_owned(),
                    pinned_at: Some(at(1)?),
                },
                PinnedFile {
                    absolute_path: other.clone(),
                    note: String::new(),
                    pinned_at: Some(at(2)?),
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
    fn notes_can_be_edited_and_removed_by_number_or_timestamp() -> TestResult {
        let store = temporary_store("note-edit")?;
        append_note(&store, &session(), &author(), "eins", at(0)?)?;
        append_note(&store, &session(), &author(), "zwei\nzweite Zeile", at(1)?)?;
        append_note(&store, &session(), &author(), "drei", at(2)?)?;
        let notes = load(&store, &session())?.note_entries();
        assert_eq!(notes.len(), 3);
        assert_eq!(notes[1].text, "zwei\nzweite Zeile");
        assert_eq!(notes[2].number, 3);

        let edited = edit_note(&store, &session(), &author(), "#2", "zwei neu", at(3)?)?;
        assert_eq!(edited.recorded_at, notes[1].recorded_at);
        let removed = remove_note(&store, &session(), &author(), "1", at(4)?)?;
        assert_eq!(removed.text, "eins");
        // Zeitstempel bleibt nach dem Aufrücken stabil adressierbar.
        let by_stamp = remove_note(&store, &session(), &author(), &notes[2].recorded_at, at(5)?)?;
        assert_eq!(by_stamp.text, "drei");

        let rest = load(&store, &session())?.note_entries();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].text, "zwei neu");
        assert_eq!(rest[0].number, 1);

        assert!(matches!(
            remove_note(&store, &session(), &author(), "#9", at(6)?),
            Err(KnowledgeError::ArtifactNotFound(_))
        ));
        assert!(edit_note(&store, &session(), &author(), "1", "  ", at(7)?).is_err());
        let heading = format!("x\n## {}\ny", at(8)?);
        assert!(edit_note(&store, &session(), &author(), "1", &heading, at(8)?).is_err());
        assert!(append_note(&store, &session(), &author(), &heading, at(8)?).is_err());
        // Ein Markdown-Abschnitt ohne Zeitstempel bleibt Teil der Notiz.
        append_note(&store, &session(), &author(), "## Plan\n- a", at(9)?)?;
        assert_eq!(load(&store, &session())?.note_entries().len(), 2);
        let (_index, report) = crate::index::KnowledgeIndex::rebuild_with_report(&store)?;
        assert!(report.is_clean());
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn unknown_note_in_an_empty_scope_is_not_found() -> TestResult {
        let store = temporary_store("note-missing")?;
        assert!(matches!(
            edit_note(&store, &session(), &author(), "1", "x", at(0)?),
            Err(KnowledgeError::ArtifactNotFound(_))
        ));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn digest_is_capped_and_lists_pins_and_open_hypotheses() -> TestResult {
        let store = temporary_store("digest")?;
        pin(
            &store,
            &session(),
            &author(),
            &absolute("d.rs"),
            "warum",
            at(0)?,
        )?;
        add_hypothesis(&store, &session(), &author(), "offen", at(1)?)?;
        add_hypothesis(&store, &session(), &author(), "zu", at(2)?)?;
        reject(&store, &session(), &author(), "#2", at(3)?)?;
        append_note(&store, &session(), &author(), "notiz-ende", at(4)?)?;
        let bench = load(&store, &session())?;
        let short = digest(
            &bench,
            DigestOptions {
                max_bytes: 2048,
                notes_lines: 0,
                include_decided: false,
            },
        );
        assert!(short.contains("d.rs — warum"), "{short}");
        assert!(short.contains("#1 offen"), "{short}");
        assert!(!short.contains("zu"), "{short}");
        assert!(!short.contains("notiz-ende"), "{short}");
        let full = digest(
            &bench,
            DigestOptions {
                max_bytes: 4096,
                notes_lines: 4,
                include_decided: true,
            },
        );
        assert!(
            full.contains("[rejected] zu") && full.contains("notiz-ende"),
            "{full}"
        );
        let tiny = digest(
            &bench,
            DigestOptions {
                max_bytes: 20,
                notes_lines: 4,
                include_decided: true,
            },
        );
        assert!(tiny.len() <= 20 && tiny.ends_with("(gekürzt)"), "{tiny}");
        let empty = load(&store, &WorkbenchScope::Session("leer".to_owned()))?;
        assert!(
            digest(
                &empty,
                DigestOptions {
                    max_bytes: 100,
                    notes_lines: 3,
                    include_decided: true
                }
            )
            .is_empty()
        );
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn inspect_pin_reports_state_and_preview() -> TestResult {
        let store = temporary_store("inspect")?;
        let file = store.root().join("datei.rs");
        std::fs::write(&file, "fn a() {}\nfn b() {}\nfn c() {}\n")
            .map_err(ctx("write pinned file"))?;
        let path = file.to_string_lossy().into_owned();
        let far_future = jiff::Timestamp::from_second(4_000_000_000)?;
        pin(&store, &session(), &author(), &path, "", far_future)?;
        let entry = load(&store, &session())?
            .pinned
            .into_iter()
            .next()
            .ok_or(TestError::Missing("pinned entry"))?;
        assert_eq!(entry.pinned_at, Some(far_future));
        let seen = inspect_pin(&entry, 2);
        assert_eq!(seen.state, PinState::Present);
        assert_eq!(seen.preview, vec!["fn a() {}", "fn b() {}"]);

        let stale = PinnedFile {
            pinned_at: Some(jiff::Timestamp::UNIX_EPOCH),
            ..entry.clone()
        };
        assert_eq!(inspect_pin(&stale, 1).state, PinState::Changed);
        let missing = PinnedFile {
            absolute_path: absolute("gibt-es-nicht-4711.rs"),
            ..entry.clone()
        };
        assert_eq!(inspect_pin(&missing, 3).state, PinState::Missing);
        let dir = PinnedFile {
            absolute_path: store.root().to_string_lossy().into_owned(),
            ..entry
        };
        assert_eq!(inspect_pin(&dir, 3).state, PinState::NotAFile);
        // Unpin entfernt auch den Zeitpunkt.
        unpin(&store, &session(), &author(), &path, far_future)?;
        pin(&store, &session(), &author(), &path, "", at(0)?)?;
        assert_eq!(load(&store, &session())?.pinned[0].pinned_at, Some(at(0)?));
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn scope_helpers_round_trip_and_derive_project_slugs() {
        let scope = WorkbenchScope::Session("abc".to_owned());
        assert_eq!(
            WorkbenchScope::from_path_component(&scope.path_component()),
            Some(scope)
        );
        assert_eq!(WorkbenchScope::from_path_component("other"), None);
        assert_eq!(WorkbenchScope::from_path_component("session:"), None);
        assert_eq!(
            WorkbenchScope::project_for_path(Path::new("/home/u/My Repo")),
            Some(WorkbenchScope::Project("my-repo".to_owned()))
        );
        assert_eq!(WorkbenchScope::project_for_path(Path::new("/")), None);
    }

    #[test]
    fn retention_defaults_parse_and_persist() -> TestResult {
        let store = temporary_store("retention")?;
        let project = WorkbenchScope::Project("p".to_owned());
        assert_eq!(retention(&store, &project)?, Retention::Keep);
        assert_eq!(
            retention(&store, &session())?,
            Retention::ExpireAfterDays {
                days: DEFAULT_SESSION_RETENTION_DAYS
            }
        );
        assert_eq!(
            Retention::parse("3d")?,
            Retention::ExpireAfterDays { days: 3 }
        );
        assert_eq!(Retention::parse("keep")?, Retention::Keep);
        assert!(Retention::parse("0d").is_err());
        assert!(Retention::parse("bald").is_err());
        set_retention(&store, &session(), Retention::Keep)?;
        assert_eq!(retention(&store, &session())?, Retention::Keep);
        assert!(set_retention(&store, &project, Retention::ExpireAfterDays { days: 0 }).is_err());
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn prune_removes_only_expired_session_scopes() -> TestResult {
        let store = temporary_store("prune")?;
        let old = WorkbenchScope::Session("alt".to_owned());
        let fresh = WorkbenchScope::Session("neu".to_owned());
        let kept = WorkbenchScope::Session("behalten".to_owned());
        let project = WorkbenchScope::Project("p".to_owned());
        let day = 86_400;
        append_note(&store, &old, &author(), "alt", at(0)?)?;
        append_note(&store, &fresh, &author(), "neu", at(20 * day)?)?;
        append_note(&store, &kept, &author(), "alt aber keep", at(0)?)?;
        set_retention(&store, &kept, Retention::Keep)?;
        append_note(&store, &project, &author(), "projekt", at(0)?)?;
        std::fs::create_dir_all(store.root().join("workbench").join("fremd"))
            .map_err(ctx("create foreign dir"))?;

        let report = prune_expired(&store, at(21 * day)?)?;
        assert_eq!(report.removed, vec![old.clone()]);
        assert_eq!(report.kept.len(), 3);
        assert_eq!(report.skipped.len(), 1);
        assert!(load(&store, &old)?.is_empty());
        assert!(!load(&store, &fresh)?.is_empty());
        assert!(!load(&store, &project)?.is_empty());

        assert!(purge_scope(&store, &fresh)?);
        assert!(!purge_scope(&store, &fresh)?);
        let empty = temporary_store("prune-empty")?;
        assert_eq!(prune_expired(&empty, at(0)?)?, PruneReport::default());
        std::fs::remove_dir_all(empty.root()).ok();
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
