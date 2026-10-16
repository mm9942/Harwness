//! `FindingSpool`: der dateibasierte Übergabepunkt für eingefrorene Befunde
//! (W3/C-FIND, Befunde F-074, G-037; Design „Warden-Proof v2“).
//!
//! # Verantwortungsbereich
//! Der Sammelprozess (`harw-sentinel`) schreibt jeden
//! [`harw_dod_rules::finding::FindingRecord`] hierher; der Triage-Dienst liest
//! ihn über einen Cursor ([`FindingSpool::page`]), und der Escalator liest
//! denselben Record **selbst** über [`FindingSpool::get`], bevor er
//! `harw_dod_rules::finding::triage_record` aufruft. Dieses Modul besitzt nur
//! Ablage, Benennung, Größen- und Symlinkschutz — keine Deutung einer Quelle
//! (siehe `lib.rs`, „keine Parselogik“: gelesen werden ausschließlich
//! Records, die diese Crate selbst geschrieben hat).
//!
//! # Ablage
//! - Ein Record je Datei `<seq:020>-<record-digest-hex>.json`, Rechte
//!   [`RECORD_FILE_MODE`] (`0640`), geschrieben über
//!   [`harw_fsutil::write_atomic`] (Tempdatei, `fsync`, `rename`,
//!   Verzeichnis-`fsync`).
//! - `seq` ist eine spool-lokale, monoton steigende Folgenummer. Die
//!   Dateinamen sortieren damit in Schreibreihenfolge, und ein Cursor
//!   ([`SpoolCursor`]) verliert keine später geschriebenen Records — anders
//!   als eine Sortierung nach Digest, bei der ein neuer Record mit kleinerem
//!   Digest hinter einem bereits gelesenen Cursor landen würde.
//! - Der Digest im Namen muss beim Lesen zum Inhalt passen
//!   ([`SpoolError::DigestMismatch`]); die Deserialisierung eines
//!   `FindingRecord` prüft zusätzlich Beleg- und Record-Digest.
//!
//! # Schutzmaßnahmen
//! - Das Spool-Verzeichnis selbst darf kein Symlink sein
//!   ([`SpoolError::SymlinkRejected`]); es wird mit
//!   [`harw_fsutil::open_dir_nofollow`] geöffnet, seine `(st_dev, st_ino)`
//!   werden gemerkt und vor jeder Operation neu verglichen
//!   ([`SpoolError::DirectoryReplaced`]).
//! - Ein für alle beschreibbares Verzeichnis wird abgelehnt
//!   ([`SpoolError::InsecurePermissions`]).
//! - Records werden mit [`harw_fsutil::open_beneath`] gelesen (kein Pfadglied
//!   darf ein Symlink sein); ein Symlink an einem Record-Namen wird
//!   ausdrücklich abgelehnt, nie gefolgt.
//! - Größenlimit je Record ([`SpoolLimits::max_record_bytes`]) beim
//!   Schreiben und beim Lesen (`fstat` **und** begrenztes Lesen).
//! - Obergrenze der Einträge ([`SpoolLimits::max_entries`]); ein Verzeichnis
//!   mit deutlich mehr Einträgen wird nicht vollständig in den Speicher
//!   geladen ([`SpoolError::TooManyEntries`]).
//!
//! # Voraussetzungen (nicht durch diesen Code erzwingbar)
//! - **Ein Schreiber.** Die Folgenummer wird beim Öffnen aus dem Verzeichnis
//!   bestimmt und danach prozesslokal hochgezählt. Zwei Prozesse, die
//!   gleichzeitig in denselben Spool schreiben, können dieselbe Nummer
//!   vergeben (die Kollisionsprüfung vor dem Schreiben verkleinert, schließt
//!   das Fenster aber nicht).
//! - Das Elternverzeichnis des Spools und der Spool selbst sind nur für den
//!   Schreiber beschreibbar (Deploy: eigener Benutzer, Gruppe
//!   `harw-findings` nur lesend) — dieselbe Voraussetzung wie bei
//!   [`harw_fsutil::write_atomic`]. Wer in den Spool schreiben kann, kann
//!   stimmige Records einlegen (der Record-Digest ist kein MAC).
//!
//! # Nebenläufigkeit
//! [`FindingSpool`] ist `Send + Sync`: Folgenummer und Zähler sind atomar,
//! alle anderen Felder unveränderlich. Lesen ist aus beliebig vielen Threads
//! möglich; mehrere schreibende Threads im selben Prozess erhalten
//! eindeutige Folgenummern.
//!
//! # Fehler
//! [`SpoolError`] (Alias [`SpoolResult`]). Meldungen enthalten Spool-Pfad und
//! Spool-Kennungen, aber nie Inhalt eines Records.
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_sentinel::spool::FindingSpool;
//! use std::path::Path;
//!
//! let spool = FindingSpool::open(Path::new("/var/lib/harw/findings"))?;
//! let (entries, next) = spool.page(None, 100)?;
//! for entry in &entries {
//!     match &entry.record {
//!         Ok(record) => assert_eq!(record.digest(), entry.id.digest()),
//!         Err(err) => eprintln!("defekter Eintrag {}: {err}", entry.id),
//!     }
//! }
//! let _resume_from = next;
//! # Ok::<(), harw_dod_sentinel::spool::SpoolError>(())
//! ```

use std::fmt;
use std::fs::{DirBuilder, File};
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use harw_dod_rules::finding::FindingRecord;
use harw_fsutil::{
    AtomicWriteOptions, EntryType, OpenMode, WalkLimits, WalkStop, open_beneath, open_dir_nofollow,
    walk_beneath, write_atomic,
};
use harw_types::ContentDigest;

/// Dateirechte jedes geschriebenen Records (`rw-r-----`).
pub const RECORD_FILE_MODE: u32 = 0o640;

/// Rechte eines von [`FindingSpool::open`] neu angelegten Spool-Verzeichnisses.
pub const SPOOL_DIR_MODE: u32 = 0o750;

/// Voreinstellung für [`SpoolLimits::max_record_bytes`]: 1 MiB.
pub const DEFAULT_MAX_RECORD_BYTES: u64 = 1024 * 1024;

/// Voreinstellung für [`SpoolLimits::max_entries`].
pub const DEFAULT_MAX_ENTRIES: usize = 65_536;

/// Obergrenze für `limit` in [`FindingSpool::page`].
pub const MAX_PAGE_LIMIT: usize = 1_000;

/// Dateiendung der Record-Dateien.
const RECORD_SUFFIX: &str = ".json";

/// Stellen der Folgenummer im Dateinamen (`u64::MAX` hat 20 Dezimalstellen).
const SEQ_DIGITS: usize = 20;

/// Hex-Stellen eines BLAKE3-Digests.
const DIGEST_HEX_LEN: usize = 64;

/// Zeitbudget eines einzelnen [`FindingSpool::sweep`]-Laufs.
pub const SWEEP_DEADLINE: Duration = Duration::from_secs(2);

/// Mindestabstand zwischen zwei Läufen von [`FindingSpool::maybe_sweep`].
pub const SWEEP_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// Grenzen eines [`FindingSpool`].
///
/// # Description
/// Die ersten beiden Grenzen sind Sicherheitsentscheidungen: ohne Größenlimit
/// kann ein übergroßer Record Leser und Triage-Kontext sprengen; ohne
/// Eintragslimit wächst der Speicherbedarf eines Verzeichnisscans unbegrenzt.
///
/// # Aufbewahrung (Opt-in, Standard: nie löschen)
/// `max_age` und `max_total_bytes` begrenzen die Platten-Obergrenze (ohne sie
/// im ungünstigsten Fall `max_entries` x `max_record_bytes` = 64 GiB). Der
/// Spool enthält sicherheitsrelevante Daten, daher gilt:
/// - `None` (Standard) = unbegrenzt = bisheriges Verhalten: es wird nie
///   etwas gelöscht, [`FindingSpool::sweep`] tut nichts.
/// - Auch mit gesetzten Grenzen löscht ein Sweep nur, wenn
///   `retention_apply == true`; sonst ist er ein reiner Bericht (Trockenlauf).
/// - Gelöscht werden nur **konsumierte** Records (Kennung <= Cursor, den der
///   Aufrufer an den Sweep übergibt), älteste zuerst. Unkonsumierte Records
///   werden nur mit `delete_unconsumed == true` angefasst.
/// - Reihenfolge: Alter, dann Gesamtbytes. Eine Anzahlgrenze löscht nicht:
///   `max_entries` weist weiterhin per [`SpoolError::SpoolFull`] ab.
///
/// # Examples
/// ```rust
/// use harw_dod_sentinel::spool::{SpoolLimits, DEFAULT_MAX_RECORD_BYTES};
///
/// assert_eq!(SpoolLimits::default().max_record_bytes, DEFAULT_MAX_RECORD_BYTES);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpoolLimits {
    /// Maximale Größe eines serialisierten Records in Bytes.
    pub max_record_bytes: u64,
    /// Maximale Anzahl Records im Spool; [`FindingSpool::put`] lehnt darüber ab.
    pub max_entries: usize,
    /// Records, deren Datei älter ist, sind löschbar (Opt-in, siehe oben).
    pub max_age: Option<Duration>,
    /// Summe der Record-Bytes, über der die ältesten löschbaren Records
    /// entfernt werden (Opt-in, siehe oben).
    pub max_total_bytes: Option<u64>,
    /// `false` (Standard): Sweep ist ein Trockenlauf und löscht nie.
    pub retention_apply: bool,
    /// `false` (Standard): unkonsumierte Records werden nie gelöscht.
    pub delete_unconsumed: bool,
}

impl Default for SpoolLimits {
    fn default() -> Self {
        Self {
            max_record_bytes: DEFAULT_MAX_RECORD_BYTES,
            max_entries: DEFAULT_MAX_ENTRIES,
            max_age: None,
            max_total_bytes: None,
            retention_apply: false,
            delete_unconsumed: false,
        }
    }
}

/// Ergebnis eines [`FindingSpool::sweep`].
///
/// # Description
/// `removed*` zählt tatsächlich gelöschte Records (nur bei
/// `applied == true`), `would_remove*` die im Trockenlauf löschbaren.
/// `over_budget` ist `true`, wenn `max_total_bytes` trotz Sweep überschritten
/// bleibt (z. B. weil der Rest unkonsumiert ist) — nichts wird dann
/// zusätzlich gelöscht.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Ob gelöscht werden durfte (`SpoolLimits::retention_apply`).
    pub applied: bool,
    /// Gelöschte Records.
    pub removed: usize,
    /// Bytes der gelöschten Records.
    pub removed_bytes: u64,
    /// Im Trockenlauf löschbare Records.
    pub would_remove: usize,
    /// Bytes der im Trockenlauf löschbaren Records.
    pub would_remove_bytes: u64,
    /// Verbleibende Records.
    pub kept: usize,
    /// Bytes der verbleibenden Records.
    pub kept_bytes: u64,
    /// Davon unkonsumiert.
    pub kept_unconsumed: usize,
    /// Einträge, die keine reguläre Datei sind (nie angefasst).
    pub skipped_non_regular: usize,
    /// `max_total_bytes` bleibt überschritten.
    pub over_budget: bool,
    /// Das Zeitbudget war erschöpft; es wurde nichts oder nur teilweise
    /// gelöscht.
    pub deadline_hit: bool,
}

/// Kennung eines Records im Spool: Folgenummer plus Record-Digest.
///
/// # Description
/// Textform `<seq:020>-<digest-hex>` (Kleinbuchstaben), Dateiname
/// `<textform>.json`. Die Ordnung (`Ord`) folgt der Folgenummer und stimmt
/// mit der bytewise Ordnung der Dateinamen überein.
///
/// # Examples
/// ```rust
/// use harw_dod_sentinel::spool::SpoolId;
///
/// let text = format!("{:020}-{}", 7, "ab".repeat(32));
/// let id = SpoolId::parse(&text).expect("gültige Kennung");
/// assert_eq!(id.seq(), 7);
/// assert_eq!(id.to_string(), text);
/// assert!(SpoolId::parse("7-abc").is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SpoolId {
    seq: u64,
    digest: ContentDigest,
}

impl SpoolId {
    /// Parst die Textform `<seq:020>-<digest-hex>`.
    ///
    /// # Arguments
    /// - `text` (`&str`): genau 20 Dezimalziffern, `-`, 64 Hex-Kleinbuchstaben.
    ///
    /// # Returns
    /// `Some(SpoolId)` bei exakt kanonischer Form, sonst `None`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let (seq_text, digest_text) = text.split_once('-')?;
        if seq_text.len() != SEQ_DIGITS || !seq_text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if digest_text.len() != DIGEST_HEX_LEN
            || !digest_text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return None;
        }
        let seq = seq_text.parse::<u64>().ok()?;
        let digest = digest_text.parse::<ContentDigest>().ok()?;
        Some(Self { seq, digest })
    }

    /// Die Folgenummer.
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Der Record-Digest, den der Inhalt haben muss.
    #[must_use]
    pub fn digest(&self) -> ContentDigest {
        self.digest
    }

    /// Der Dateiname dieses Records im Spool.
    fn file_name(&self) -> String {
        format!("{self}{RECORD_SUFFIX}")
    }

    /// Erkennt einen Record-Dateinamen; alles andere (Tempdateien,
    /// Fremddateien) ergibt `None`.
    fn from_file_name(name: &str) -> Option<Self> {
        name.strip_suffix(RECORD_SUFFIX).and_then(Self::parse)
    }
}

impl fmt::Display for SpoolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:020}-{}", self.seq, self.digest)
    }
}

/// Lesefortschritt über den Spool: „alles nach dieser Kennung“.
///
/// # Description
/// Wird von [`FindingSpool::page`] geliefert und dort wieder übergeben.
/// Persistierbar über die Textform ([`fmt::Display`] / [`SpoolCursor::parse`]),
/// identisch mit der Textform der zuletzt gelieferten [`SpoolId`].
///
/// # Examples
/// ```rust
/// use harw_dod_sentinel::spool::SpoolCursor;
///
/// let text = format!("{:020}-{}", 3, "0".repeat(64));
/// let cursor = SpoolCursor::parse(&text).expect("gültiger Cursor");
/// assert_eq!(cursor.to_string(), text);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpoolCursor {
    after: SpoolId,
}

impl SpoolCursor {
    /// Parst die Textform eines Cursors (gleich der einer [`SpoolId`]).
    ///
    /// # Returns
    /// `Some(SpoolCursor)` bei kanonischer Form, sonst `None`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        SpoolId::parse(text).map(|after| Self { after })
    }

    /// Die Kennung, nach der die nächste Seite beginnt.
    #[must_use]
    pub fn after(&self) -> &SpoolId {
        &self.after
    }
}

impl fmt::Display for SpoolCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.after.fmt(f)
    }
}

/// Ein Eintrag einer Seite aus [`FindingSpool::page`].
///
/// # Description
/// Ein defekter Eintrag (Symlink, zu groß, nicht dekodierbar, Digest passt
/// nicht zum Namen) blockiert nicht die ganze Seite, wird aber auch nicht
/// still übergangen: er erscheint mit `record: Err(..)`.
#[derive(Debug)]
pub struct SpoolEntry {
    /// Kennung des Eintrags.
    pub id: SpoolId,
    /// Der geprüfte Record oder der Grund, warum er nicht lesbar ist.
    pub record: Result<FindingRecord, SpoolError>,
}

/// Fehler des Spools.
///
/// # Description
/// Jede Variante nennt den Spool-Pfad oder die [`SpoolId`], nie Inhalt eines
/// Records. Fremdfehler bleiben über `source()` erreichbar.
///
/// # Errors
/// Von allen fehlbaren Methoden von [`FindingSpool`] erzeugt.
#[derive(Debug)]
pub enum SpoolError {
    /// Ein Dateisystemaufruf ist gescheitert.
    Io {
        /// Was gerade versucht wurde.
        op: &'static str,
        /// Betroffener Pfad (Spool-Verzeichnis oder Record-Datei).
        path: PathBuf,
        /// Ursache.
        source: io::Error,
    },
    /// Spool-Verzeichnis oder Record-Datei ist ein Symlink.
    SymlinkRejected {
        /// Der Symlink.
        path: PathBuf,
    },
    /// Der Spool-Pfad ist kein Verzeichnis.
    NotADirectory {
        /// Der Spool-Pfad.
        path: PathBuf,
    },
    /// Das Spool-Verzeichnis ist für alle beschreibbar.
    InsecurePermissions {
        /// Der Spool-Pfad.
        path: PathBuf,
        /// Die vorgefundenen Rechte-Bits.
        mode: u32,
    },
    /// Das Spool-Verzeichnis wurde seit dem Öffnen ersetzt.
    DirectoryReplaced {
        /// Der Spool-Pfad.
        path: PathBuf,
    },
    /// Unter einer Record-Kennung liegt keine reguläre Datei.
    NotARegularFile {
        /// Die betroffene Kennung.
        id: SpoolId,
    },
    /// Ein Record überschreitet [`SpoolLimits::max_record_bytes`].
    RecordTooLarge {
        /// Tatsächliche (bzw. beim Lesen mindestens festgestellte) Größe.
        len: u64,
        /// Die Grenze.
        limit: u64,
    },
    /// Der Spool hat [`SpoolLimits::max_entries`] erreicht.
    SpoolFull {
        /// Die Grenze.
        limit: usize,
    },
    /// Das Verzeichnis enthält so viele Einträge, dass ein Scan abgebrochen
    /// wurde.
    TooManyEntries {
        /// Die Scan-Grenze.
        limit: usize,
    },
    /// Unter der neu vergebenen Kennung existiert bereits ein Eintrag
    /// (zweiter Schreiber?).
    IdCollision {
        /// Die Kennung.
        id: SpoolId,
    },
    /// Die Folgenummer ist erschöpft.
    SequenceExhausted,
    /// Der Record ließ sich nicht serialisieren.
    Encode(serde_json::Error),
    /// Der Record besteht seine eigene Prüfung nach der Serialisierung nicht
    /// (inkonsistenter Beleg oder nicht verlustfrei kodierbarer Messwert).
    InvalidRecord(serde_json::Error),
    /// Ein gespeicherter Record ließ sich nicht dekodieren oder prüfen.
    Decode {
        /// Die Kennung.
        id: SpoolId,
        /// Ursache (kann Feldnamen aus der Datei nennen).
        source: serde_json::Error,
    },
    /// Der Record-Digest passt nicht zum Digest im Dateinamen.
    DigestMismatch {
        /// Die Kennung.
        id: SpoolId,
    },
}

/// Ergebnis-Alias dieses Moduls.
pub type SpoolResult<T> = Result<T, SpoolError>;

impl fmt::Display for SpoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { op, path, .. } => {
                write!(f, "finding spool: failed to {op} at {}", path.display())
            }
            Self::SymlinkRejected { path } => {
                write!(f, "finding spool: refusing symlink at {}", path.display())
            }
            Self::NotADirectory { path } => {
                write!(f, "finding spool: {} is not a directory", path.display())
            }
            Self::InsecurePermissions { path, mode } => write!(
                f,
                "finding spool: directory {} is world-writable (mode {mode:04o})",
                path.display()
            ),
            Self::DirectoryReplaced { path } => write!(
                f,
                "finding spool: directory {} was replaced after opening",
                path.display()
            ),
            Self::NotARegularFile { id } => {
                write!(f, "finding spool: entry {id} is not a regular file")
            }
            Self::RecordTooLarge { len, limit } => write!(
                f,
                "finding spool: record of {len} bytes exceeds the limit of {limit} bytes"
            ),
            Self::SpoolFull { limit } => {
                write!(
                    f,
                    "finding spool: spool holds the maximum of {limit} records"
                )
            }
            Self::TooManyEntries { limit } => write!(
                f,
                "finding spool: directory scan stopped after {limit} entries"
            ),
            Self::IdCollision { id } => {
                write!(f, "finding spool: an entry named {id} already exists")
            }
            Self::SequenceExhausted => f.write_str("finding spool: sequence numbers exhausted"),
            Self::Encode(_) => f.write_str("finding spool: record could not be serialized"),
            Self::InvalidRecord(_) => {
                f.write_str("finding spool: record fails its own verification after encoding")
            }
            Self::Decode { id, .. } => {
                write!(
                    f,
                    "finding spool: record {id} could not be decoded or verified"
                )
            }
            Self::DigestMismatch { id } => write!(
                f,
                "finding spool: record content does not match the digest in its name {id}"
            ),
        }
    }
}

impl std::error::Error for SpoolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Encode(source) | Self::InvalidRecord(source) => Some(source),
            Self::Decode { source, .. } => Some(source),
            Self::SymlinkRejected { .. }
            | Self::NotADirectory { .. }
            | Self::InsecurePermissions { .. }
            | Self::DirectoryReplaced { .. }
            | Self::NotARegularFile { .. }
            | Self::RecordTooLarge { .. }
            | Self::SpoolFull { .. }
            | Self::TooManyEntries { .. }
            | Self::IdCollision { .. }
            | Self::SequenceExhausted
            | Self::DigestMismatch { .. } => None,
        }
    }
}

fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// Baut einen `Io`-Fehler-Mapper für `map_err`.
fn io_err<'a>(op: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> SpoolError + 'a {
    move |source| SpoolError::Io {
        op,
        path: path.to_path_buf(),
        source,
    }
}

/// Ein Verzeichnis eingefrorener Befunde.
///
/// # Description
/// Siehe Moduldoku für Ablageformat, Schutzmaßnahmen und Voraussetzungen.
///
/// # Concurrency
/// `Send + Sync`; siehe Moduldoku.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug)]
pub struct FindingSpool {
    dir: PathBuf,
    dev: u64,
    ino: u64,
    limits: SpoolLimits,
    next_seq: AtomicU64,
    count: AtomicUsize,
    last_sweep_secs: AtomicU64,
    open_report: SweepReport,
}

impl FindingSpool {
    /// Öffnet (und legt bei Bedarf an) einen Spool mit [`SpoolLimits::default`].
    ///
    /// # Arguments
    /// - `dir` (`&Path`): das Spool-Verzeichnis; das Elternverzeichnis muss
    ///   existieren.
    ///
    /// # Returns
    /// Den geöffneten Spool; die nächste Folgenummer liegt hinter der
    /// höchsten vorgefundenen.
    ///
    /// # Errors
    /// Siehe [`FindingSpool::open_with_limits`].
    pub fn open(dir: &Path) -> SpoolResult<Self> {
        Self::open_with_limits(dir, SpoolLimits::default())
    }

    /// Öffnet (und legt bei Bedarf an) einen Spool mit expliziten Grenzen.
    ///
    /// # Description
    /// Fehlt `dir`, wird es (nicht rekursiv) mit [`SPOOL_DIR_MODE`] angelegt.
    /// Danach: Symlinkprüfung, `open_dir_nofollow`, Rechteprüfung, Merken von
    /// `(st_dev, st_ino)`, Scan der vorhandenen Records.
    ///
    /// # Arguments
    /// - `dir` (`&Path`): das Spool-Verzeichnis.
    /// - `limits` (`SpoolLimits`): Größen- und Eintragsgrenzen.
    ///
    /// # Returns
    /// Den geöffneten Spool.
    ///
    /// # Errors
    /// - [`SpoolError::SymlinkRejected`]: `dir` ist ein Symlink.
    /// - [`SpoolError::NotADirectory`]: `dir` ist kein Verzeichnis.
    /// - [`SpoolError::InsecurePermissions`]: `dir` ist für alle beschreibbar.
    /// - [`SpoolError::TooManyEntries`]: das Verzeichnis ist übervoll.
    /// - [`SpoolError::SequenceExhausted`]: höchste Folgenummer ist `u64::MAX`.
    /// - [`SpoolError::Io`]: Anlegen, Öffnen oder Scannen scheitert.
    ///
    /// # Concurrency
    /// Nicht synchronisiert mit anderen Prozessen; siehe Moduldoku
    /// („Ein Schreiber“).
    pub fn open_with_limits(dir: &Path, limits: SpoolLimits) -> SpoolResult<Self> {
        Self::open_with_cursor(dir, limits, None)
    }

    /// Wie [`FindingSpool::open_with_limits`], mit Aufbewahrungs-Sweep gegen
    /// den bekannten Konsum-Cursor.
    ///
    /// # Description
    /// Sind `max_age`/`max_total_bytes` gesetzt, läuft direkt nach dem Öffnen
    /// ein [`FindingSpool::sweep`] (Ergebnis: [`FindingSpool::open_report`]).
    /// Ohne gesetzte Grenzen (Standard) geschieht nichts.
    ///
    /// # Arguments
    /// - `consumed` (`Option<&SpoolCursor>`): alles bis einschließlich dieser
    ///   Kennung gilt als konsumiert; `None` = nichts konsumiert.
    ///
    /// # Errors
    /// Wie [`FindingSpool::open_with_limits`]; zusätzlich ein Fehler des
    /// Sweeps (fail closed: der Spool wird dann nicht geöffnet).
    pub fn open_with_cursor(
        dir: &Path,
        limits: SpoolLimits,
        consumed: Option<&SpoolCursor>,
    ) -> SpoolResult<Self> {
        match std::fs::symlink_metadata(dir) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(SpoolError::SymlinkRejected {
                    path: dir.to_path_buf(),
                });
            }
            Ok(meta) if !meta.is_dir() => {
                return Err(SpoolError::NotADirectory {
                    path: dir.to_path_buf(),
                });
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                match DirBuilder::new().mode(SPOOL_DIR_MODE).create(dir) {
                    Ok(()) => {}
                    // Wettlauf mit einem anderen Anleger: das folgende
                    // `open_dir_nofollow` prüft, was tatsächlich dort liegt.
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(err) => return Err(io_err("create spool directory", dir)(err)),
                }
            }
            Err(err) => return Err(io_err("inspect spool directory", dir)(err)),
        }

        let handle =
            File::from(open_dir_nofollow(dir).map_err(io_err("open spool directory", dir))?);
        let meta = handle
            .metadata()
            .map_err(io_err("stat spool directory", dir))?;
        let mode = meta.mode() & 0o7777;
        if mode & 0o002 != 0 {
            return Err(SpoolError::InsecurePermissions {
                path: dir.to_path_buf(),
                mode,
            });
        }

        let spool = Self {
            dir: dir.to_path_buf(),
            dev: meta.dev(),
            ino: meta.ino(),
            limits,
            next_seq: AtomicU64::new(1),
            count: AtomicUsize::new(0),
            last_sweep_secs: AtomicU64::new(0),
            open_report: SweepReport::default(),
        };
        let mut spool = spool;
        let ids = spool.list_ids()?;
        let next = match ids.iter().map(|(id, _)| id.seq).max() {
            Some(u64::MAX) => return Err(SpoolError::SequenceExhausted),
            Some(max) => max + 1,
            None => 1,
        };
        spool.next_seq.store(next, Ordering::SeqCst);
        spool.count.store(ids.len(), Ordering::SeqCst);
        let now = SystemTime::now();
        spool.open_report = spool.sweep(consumed, now)?;
        spool
            .last_sweep_secs
            .store(unix_secs(now), Ordering::SeqCst);
        Ok(spool)
    }

    /// Bericht des Sweeps beim Öffnen (leer, wenn keine Grenzen gesetzt).
    #[must_use]
    pub fn open_report(&self) -> &SweepReport {
        &self.open_report
    }

    /// Wendet `max_age` und `max_total_bytes` an (oder berichtet nur).
    ///
    /// # Description
    /// Ohne gesetzte Grenzen: leerer Bericht, kein Scan. Sonst werden alle
    /// Record-Dateien per `lstat` (ohne Symlink-Folge) vermessen; Nicht-Dateien
    /// und fremde Namen (Tempdateien, `*.lock`, Punktdateien) werden nie
    /// angefasst. Löschbar sind konsumierte Records (`<= consumed`), bei
    /// `delete_unconsumed` alle. Reihenfolge: Alter, dann Bytes (älteste
    /// zuerst, Neueste bleiben). Gelöscht wird nur mit `retention_apply`, und
    /// nur per `unlink`.
    ///
    /// # Errors
    /// Fail closed: jeder Fehler beim Vermessen bricht ab, **bevor**
    /// irgendetwas gelöscht wurde; ein Fehler beim Löschen beendet den Lauf.
    /// Ist das Zeitbudget ([`SWEEP_DEADLINE`]) beim Vermessen erschöpft,
    /// wird nichts gelöscht (`deadline_hit`).
    pub fn sweep(
        &self,
        consumed: Option<&SpoolCursor>,
        now: SystemTime,
    ) -> SpoolResult<SweepReport> {
        let mut report = SweepReport {
            applied: self.limits.retention_apply,
            ..SweepReport::default()
        };
        if self.limits.max_age.is_none() && self.limits.max_total_bytes.is_none() {
            return Ok(report);
        }
        let deadline = Instant::now() + SWEEP_DEADLINE;
        let ids = self.list_ids()?;

        struct Item {
            id: SpoolId,
            size: u64,
            mtime: SystemTime,
            consumed: bool,
        }
        let mut items: Vec<Item> = Vec::with_capacity(ids.len());
        for (id, entry_type) in ids {
            if Instant::now() >= deadline {
                report.deadline_hit = true;
                return Ok(report);
            }
            if entry_type != EntryType::File {
                report.skipped_non_regular += 1;
                continue;
            }
            let path = self.dir.join(id.file_name());
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(meta) => meta,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(io_err("inspect spool record", &path)(err)),
            };
            if !meta.is_file() {
                report.skipped_non_regular += 1;
                continue;
            }
            let mtime = meta
                .modified()
                .map_err(io_err("read spool record mtime", &path))?;
            let is_consumed = consumed.is_some_and(|c| id <= c.after);
            items.push(Item {
                id,
                size: meta.len(),
                mtime,
                consumed: is_consumed,
            });
        }

        let eligible = |item: &Item| item.consumed || self.limits.delete_unconsumed;
        let mut doomed = vec![false; items.len()];
        if let Some(max_age) = self.limits.max_age {
            for (flag, item) in doomed.iter_mut().zip(&items) {
                let aged = now
                    .duration_since(item.mtime)
                    .is_ok_and(|age| age > max_age);
                *flag = aged && eligible(item);
            }
        }
        if let Some(cap) = self.limits.max_total_bytes {
            let mut total: u64 = items
                .iter()
                .zip(&doomed)
                .filter(|(_, d)| !**d)
                .fold(0u64, |acc, (item, _)| acc.saturating_add(item.size));
            // `items` ist nach Folgenummer, also nach Schreibreihenfolge,
            // sortiert: älteste zuerst.
            for (flag, item) in doomed.iter_mut().zip(&items) {
                if total <= cap {
                    break;
                }
                if !*flag && eligible(item) {
                    *flag = true;
                    total = total.saturating_sub(item.size);
                }
            }
            report.over_budget = total > cap;
        }

        let apply = self.limits.retention_apply;
        if apply && doomed.iter().any(|d| *d) {
            self.verified_dir()?;
        }
        for (item, doom) in items.iter().zip(&doomed) {
            let keep = |report: &mut SweepReport| {
                report.kept += 1;
                report.kept_bytes = report.kept_bytes.saturating_add(item.size);
                report.kept_unconsumed += usize::from(!item.consumed);
            };
            if !*doom {
                keep(&mut report);
                continue;
            }
            if !apply {
                report.would_remove += 1;
                report.would_remove_bytes = report.would_remove_bytes.saturating_add(item.size);
                continue;
            }
            if Instant::now() >= deadline {
                report.deadline_hit = true;
                keep(&mut report);
                continue;
            }
            let path = self.dir.join(item.id.file_name());
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    report.removed += 1;
                    report.removed_bytes = report.removed_bytes.saturating_add(item.size);
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(io_err("remove spool record", &path)(err)),
            }
        }
        if report.removed > 0 {
            let mut current = self.count.load(Ordering::SeqCst);
            loop {
                let next = current.saturating_sub(report.removed);
                match self
                    .count
                    .compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst)
                {
                    Ok(_) => break,
                    Err(observed) => current = observed,
                }
            }
        }
        Ok(report)
    }

    /// Günstiger periodischer Aufruf: führt [`FindingSpool::sweep`] höchstens
    /// alle [`SWEEP_MIN_INTERVAL`] aus.
    ///
    /// # Returns
    /// `Ok(None)`, wenn noch nicht wieder dran (oder keine Grenzen gesetzt);
    /// sonst den Bericht.
    ///
    /// # Errors
    /// Wie [`FindingSpool::sweep`].
    pub fn maybe_sweep(
        &self,
        consumed: Option<&SpoolCursor>,
        now: SystemTime,
    ) -> SpoolResult<Option<SweepReport>> {
        if self.limits.max_age.is_none() && self.limits.max_total_bytes.is_none() {
            return Ok(None);
        }
        let secs = unix_secs(now);
        let last = self.last_sweep_secs.load(Ordering::SeqCst);
        if last != 0 && secs.saturating_sub(last) < SWEEP_MIN_INTERVAL.as_secs() {
            return Ok(None);
        }
        self.last_sweep_secs.store(secs, Ordering::SeqCst);
        self.sweep(consumed, now).map(Some)
    }

    /// Der Pfad des Spool-Verzeichnisses.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Die Grenzen dieses Spools.
    #[must_use]
    pub fn limits(&self) -> SpoolLimits {
        self.limits
    }

    /// Legt einen Record ab.
    ///
    /// # Description
    /// Serialisiert `record` als JSON, prüft Größe und Selbstkonsistenz
    /// (Rundreise durch die prüfende Deserialisierung), vergibt die nächste
    /// Folgenummer und schreibt atomar mit [`RECORD_FILE_MODE`].
    ///
    /// # Arguments
    /// - `record` (`&FindingRecord`): der abzulegende Record.
    ///
    /// # Returns
    /// Die [`SpoolId`] des neuen Eintrags.
    ///
    /// # Errors
    /// - [`SpoolError::Encode`] / [`SpoolError::InvalidRecord`]: nicht
    ///   (verlustfrei) kodierbar oder inkonsistent.
    /// - [`SpoolError::RecordTooLarge`]: über [`SpoolLimits::max_record_bytes`].
    /// - [`SpoolError::SpoolFull`]: [`SpoolLimits::max_entries`] erreicht.
    /// - [`SpoolError::DirectoryReplaced`]: Verzeichnis ausgetauscht.
    /// - [`SpoolError::IdCollision`]: Name bereits belegt.
    /// - [`SpoolError::SequenceExhausted`]: keine Folgenummer mehr.
    /// - [`SpoolError::Io`]: Schreiben scheitert.
    ///
    /// # Concurrency
    /// Threadsicher innerhalb eines Prozesses; siehe Moduldoku für mehrere
    /// Prozesse.
    pub fn put(&self, record: &FindingRecord) -> SpoolResult<SpoolId> {
        let bytes = serde_json::to_vec(record).map_err(SpoolError::Encode)?;
        let len = bytes.len() as u64;
        if len > self.limits.max_record_bytes {
            return Err(SpoolError::RecordTooLarge {
                len,
                limit: self.limits.max_record_bytes,
            });
        }
        // Schreiberseitig dieselbe Prüfung wie beim Lesen: ein Record, der sie
        // nicht besteht, soll nicht als Giftpille im Spool landen.
        serde_json::from_slice::<FindingRecord>(&bytes).map_err(SpoolError::InvalidRecord)?;

        if self.count.load(Ordering::SeqCst) >= self.limits.max_entries {
            // Einträge können extern entfernt worden sein (Aufbewahrung).
            let current = self.list_ids()?.len();
            self.count.store(current, Ordering::SeqCst);
            if current >= self.limits.max_entries {
                return Err(SpoolError::SpoolFull {
                    limit: self.limits.max_entries,
                });
            }
        }

        self.verified_dir()?;
        // Compare-and-swap von Hand: `fetch_update` ist auf der aktuellen
        // Toolchain veraltet, das neue `try_update` erst ab Rust 1.95 stabil —
        // die MSRV des Projekts ist 1.85. Ein Überlauf beendet die Vergabe
        // ausdrücklich, statt still auf 0 zurückzuspringen.
        let seq = {
            let mut current = self.next_seq.load(Ordering::SeqCst);
            loop {
                let Some(next) = current.checked_add(1) else {
                    return Err(SpoolError::SequenceExhausted);
                };
                match self.next_seq.compare_exchange(
                    current,
                    next,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(previous) => break previous,
                    Err(observed) => current = observed,
                }
            }
        };
        let id = SpoolId {
            seq,
            digest: record.digest(),
        };
        let path = self.dir.join(id.file_name());
        match std::fs::symlink_metadata(&path) {
            Ok(_) => return Err(SpoolError::IdCollision { id }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_err("inspect spool record", &path)(err)),
        }
        write_atomic(
            &path,
            &bytes,
            AtomicWriteOptions::with_mode(RECORD_FILE_MODE),
        )
        .map_err(io_err("write spool record", &path))?;
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(id)
    }

    /// Liest einen Record.
    ///
    /// # Description
    /// Symlinkfest (`open_beneath`), größenbegrenzt, mit vollständiger
    /// Record-Prüfung und Abgleich des Digests gegen den Namen.
    ///
    /// # Arguments
    /// - `id` (`&SpoolId`): die Kennung aus [`FindingSpool::put`] oder
    ///   [`FindingSpool::page`].
    ///
    /// # Returns
    /// `Ok(Some(record))` bei Erfolg, `Ok(None)`, wenn kein Eintrag existiert.
    ///
    /// # Errors
    /// - [`SpoolError::SymlinkRejected`]: der Eintrag ist ein Symlink.
    /// - [`SpoolError::NotARegularFile`]: der Eintrag ist keine Datei.
    /// - [`SpoolError::RecordTooLarge`]: über [`SpoolLimits::max_record_bytes`].
    /// - [`SpoolError::Decode`]: nicht dekodierbar oder inkonsistent.
    /// - [`SpoolError::DigestMismatch`]: Inhalt passt nicht zum Namen.
    /// - [`SpoolError::DirectoryReplaced`] / [`SpoolError::Io`].
    ///
    /// # Concurrency
    /// Aus beliebigen Threads aufrufbar.
    pub fn get(&self, id: &SpoolId) -> SpoolResult<Option<FindingRecord>> {
        let dir = self.verified_dir()?;
        self.load(&dir, id)
    }

    /// Liefert die nächste Seite von Records nach `cursor`.
    ///
    /// # Description
    /// Einträge in Schreibreihenfolge (Folgenummer). Tempdateien und
    /// Fremddateien werden ignoriert; defekte Record-Einträge erscheinen mit
    /// `record: Err(..)` (siehe [`SpoolEntry`]). Einträge, die zwischen Scan
    /// und Lesen entfernt wurden, werden übersprungen, der Cursor rückt aber
    /// über sie hinweg.
    ///
    /// # Arguments
    /// - `cursor` (`Option<&SpoolCursor>`): `None` beginnt am Anfang.
    /// - `limit` (`usize`): höchstens so viele Einträge, gekappt auf
    ///   [`MAX_PAGE_LIMIT`]; `0` liefert eine leere Seite.
    ///
    /// # Returns
    /// `(entries, next)`. `next` ist `Some`, sobald mindestens ein Eintrag
    /// betrachtet wurde, und zeigt hinter den letzten; bei leerer Seite
    /// `None` — der Aufrufer behält dann seinen bisherigen Cursor. Eine Seite
    /// mit weniger als `limit` Einträgen bedeutet: derzeit nichts weiter.
    ///
    /// # Errors
    /// - [`SpoolError::TooManyEntries`]: Verzeichnis übervoll.
    /// - [`SpoolError::DirectoryReplaced`] / [`SpoolError::Io`]: Scan scheitert.
    ///
    /// # Concurrency
    /// Aus beliebigen Threads aufrufbar.
    pub fn page(
        &self,
        cursor: Option<&SpoolCursor>,
        limit: usize,
    ) -> SpoolResult<(Vec<SpoolEntry>, Option<SpoolCursor>)> {
        let limit = limit.min(MAX_PAGE_LIMIT);
        if limit == 0 {
            return Ok((Vec::new(), None));
        }
        let ids = self.list_ids()?;
        let dir = self.verified_dir()?;
        let mut entries = Vec::with_capacity(limit);
        let mut last_seen: Option<SpoolId> = None;
        for (id, entry_type) in ids
            .into_iter()
            .filter(|(id, _)| cursor.is_none_or(|c| *id > c.after))
            .take(limit)
        {
            last_seen = Some(id.clone());
            let record = match entry_type {
                EntryType::File => match self.load(&dir, &id) {
                    Ok(Some(record)) => Ok(record),
                    Ok(None) => continue,
                    Err(err) => Err(err),
                },
                EntryType::Symlink => Err(SpoolError::SymlinkRejected {
                    path: self.dir.join(id.file_name()),
                }),
                EntryType::Dir | EntryType::Other => {
                    Err(SpoolError::NotARegularFile { id: id.clone() })
                }
            };
            entries.push(SpoolEntry { id, record });
        }
        Ok((entries, last_seen.map(|after| SpoolCursor { after })))
    }

    /// Öffnet das Spool-Verzeichnis neu und prüft, dass es dasselbe ist.
    fn verified_dir(&self) -> SpoolResult<File> {
        let handle = File::from(
            open_dir_nofollow(&self.dir).map_err(io_err("reopen spool directory", &self.dir))?,
        );
        let meta = handle
            .metadata()
            .map_err(io_err("stat spool directory", &self.dir))?;
        if meta.dev() != self.dev || meta.ino() != self.ino {
            return Err(SpoolError::DirectoryReplaced {
                path: self.dir.clone(),
            });
        }
        Ok(handle)
    }

    /// Scan-Budget: Records plus Spielraum für Tempdateien und Fremdeinträge.
    fn scan_budget(&self) -> usize {
        self.limits.max_entries.saturating_mul(2).saturating_add(64)
    }

    /// Listet alle Record-Einträge, sortiert nach Folgenummer.
    fn list_ids(&self) -> SpoolResult<Vec<(SpoolId, EntryType)>> {
        self.verified_dir()?;
        let budget = self.scan_budget();
        let limits = WalkLimits {
            max_depth: 1,
            max_entries: budget,
            deadline: None,
        };
        let mut walk =
            walk_beneath(&self.dir, limits).map_err(io_err("scan spool directory", &self.dir))?;
        let mut ids = Vec::new();
        for item in walk.by_ref() {
            let entry = item.map_err(io_err("scan spool directory", &self.dir))?;
            let Some(name) = entry.rel_path.to_str() else {
                continue;
            };
            if let Some(id) = SpoolId::from_file_name(name) {
                ids.push((id, entry.entry_type));
            }
        }
        if walk.stopped() == Some(WalkStop::EntryLimit) {
            return Err(SpoolError::TooManyEntries { limit: budget });
        }
        // Wurde das Verzeichnis während des Scans getauscht, gehört das
        // Ergebnis nicht zu diesem Spool.
        self.verified_dir()?;
        ids.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(ids)
    }

    /// Lädt einen Record relativ zum geprüften Verzeichnis-Deskriptor.
    fn load(&self, dir: &File, id: &SpoolId) -> SpoolResult<Option<FindingRecord>> {
        let name = id.file_name();
        let path = self.dir.join(&name);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(SpoolError::SymlinkRejected { path });
            }
            Ok(meta) if !meta.is_file() => {
                return Err(SpoolError::NotARegularFile { id: id.clone() });
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(io_err("inspect spool record", &path)(err)),
        }
        let file = match open_beneath(dir.as_fd(), Path::new(&name), OpenMode::read_only()) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(io_err("open spool record", &path)(err)),
        };
        let meta = file
            .metadata()
            .map_err(io_err("stat spool record", &path))?;
        if !meta.is_file() {
            return Err(SpoolError::NotARegularFile { id: id.clone() });
        }
        let limit = self.limits.max_record_bytes;
        if meta.len() > limit {
            return Err(SpoolError::RecordTooLarge {
                len: meta.len(),
                limit,
            });
        }
        let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
        file.take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(io_err("read spool record", &path))?;
        let read = bytes.len() as u64;
        if read > limit {
            return Err(SpoolError::RecordTooLarge { len: read, limit });
        }
        let record: FindingRecord =
            serde_json::from_slice(&bytes).map_err(|source| SpoolError::Decode {
                id: id.clone(),
                source,
            })?;
        if record.digest() != id.digest {
            return Err(SpoolError::DigestMismatch { id: id.clone() });
        }
        Ok(Some(record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    use harw_code_graph::lockfile::LockedPackage;
    use harw_dod_rules::advisory::{Advisory, correlate_advisories};
    use harw_dod_signals::{DriftSeverity, EventKind, SecurityEvent, SecurityEvidence, Severity};
    use harw_types::SensorId;
    use jiff::Timestamp;
    use semver::VersionReq;

    use crate::test_support::{TestError, TestResult, ctx};

    // Erzeugt `n` echte Records über die öffentliche Advisory-Korrelation
    // (neben `run_rules_checked` eine der beiden öffentlichen Prägestellen
    // für `Finding<RuleChecked>`; sie erlaubt hier `n` unterscheidbare
    // Befunde).
    fn records(n: usize, detail: &str) -> TestResult<Vec<FindingRecord>> {
        let locked = vec![LockedPackage {
            name: "example-crate".to_owned(),
            version: "1.9.0".to_owned(),
            source: None,
            checksum: None,
        }];
        let range = VersionReq::parse("<1.10.0").map_err(ctx("version range parses"))?;
        let advisories: Vec<Advisory> = (0..n)
            .map(|i| Advisory {
                id: format!("RUSTSEC-2024-{i:04}"),
                crate_name: "example-crate".to_owned(),
                vulnerable_ranges: vec![range.clone()],
                severity: Severity::High,
                summary: format!("{detail} #{i}"),
            })
            .collect();
        let events: Vec<SecurityEvent> = (0..n)
            .map(|i| SecurityEvent {
                sensor: SensorId::from_str("workspace-0"),
                observed_at: Timestamp::UNIX_EPOCH,
                actor: None,
                kind: EventKind::StructureDrift {
                    severity: DriftSeverity::High,
                    detail: format!("{detail} #{i}"),
                },
            })
            .collect();
        let evidence = SecurityEvidence::capture(vec![], events, Timestamp::UNIX_EPOCH)
            .map_err(ctx("test evidence encodes"))?;
        Ok(
            correlate_advisories(&advisories, &locked, Timestamp::UNIX_EPOCH)
                .iter()
                .map(|finding| finding.record(evidence.clone()))
                .collect(),
        )
    }

    fn one_record() -> TestResult<FindingRecord> {
        records(1, "unexpected setuid binary")?
            .into_iter()
            .next()
            .ok_or(TestError::Missing("advisory matches the locked version"))
    }

    fn spool_dir(root: &tempfile::TempDir) -> PathBuf {
        root.path().join("findings")
    }

    #[test]
    fn test_put_then_get_returns_identical_record() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spool = FindingSpool::open(&spool_dir(&root)).map_err(ctx("spool opens"))?;
        let record = one_record()?;
        let id = spool.put(&record).map_err(ctx("put succeeds"))?;
        assert_eq!(id.digest(), record.digest());
        let back = spool.get(&id).map_err(ctx("get succeeds"))?;
        assert_eq!(back, Some(record));
        Ok(())
    }

    #[test]
    fn test_put_writes_file_with_mode_0640() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spool = FindingSpool::open(&spool_dir(&root)).map_err(ctx("spool opens"))?;
        let id = spool.put(&one_record()?).map_err(ctx("put succeeds"))?;
        let meta =
            std::fs::metadata(spool.dir().join(id.file_name())).map_err(ctx("record exists"))?;
        assert_eq!(meta.permissions().mode() & 0o777, RECORD_FILE_MODE);
        Ok(())
    }

    #[test]
    fn test_get_missing_id_returns_none() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spool = FindingSpool::open(&spool_dir(&root)).map_err(ctx("spool opens"))?;
        let id = SpoolId {
            seq: 42,
            digest: ContentDigest::of(b"absent"),
        };
        assert!(matches!(spool.get(&id), Ok(None)));
        Ok(())
    }

    #[test]
    fn test_page_returns_records_in_write_order_with_cursor() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spool = FindingSpool::open(&spool_dir(&root)).map_err(ctx("spool opens"))?;
        let written: Vec<SpoolId> = records(3, "drift")?
            .iter()
            .map(|record| spool.put(record).map_err(ctx("put succeeds")))
            .collect::<TestResult<Vec<_>>>()?;

        let (first, cursor) = spool.page(None, 2).map_err(ctx("first page"))?;
        assert_eq!(
            first.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
            written[..2].to_vec()
        );
        assert!(first.iter().all(|e| e.record.is_ok()));
        let cursor = cursor.ok_or(TestError::Missing("non-empty page yields a cursor"))?;

        let (second, cursor2) = spool.page(Some(&cursor), 2).map_err(ctx("second page"))?;
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].id, written[2]);
        let cursor2 = cursor2.ok_or(TestError::Missing("non-empty page yields a cursor"))?;

        let (third, cursor3) = spool.page(Some(&cursor2), 2).map_err(ctx("third page"))?;
        assert!(third.is_empty());
        assert!(cursor3.is_none());
        Ok(())
    }

    #[test]
    fn test_page_cursor_sees_records_written_later() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spool = FindingSpool::open(&spool_dir(&root)).map_err(ctx("spool opens"))?;
        let mut batch = records(2, "late")?.into_iter();
        let first = batch.next().ok_or(TestError::Missing("record"))?;
        spool.put(&first).map_err(ctx("put"))?;
        let (_, cursor) = spool.page(None, 10).map_err(ctx("page"))?;
        let cursor = cursor.ok_or(TestError::Missing("cursor"))?;
        let second = batch.next().ok_or(TestError::Missing("record"))?;
        let later = spool.put(&second).map_err(ctx("put"))?;
        let (entries, _) = spool.page(Some(&cursor), 10).map_err(ctx("page"))?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, later);
        Ok(())
    }

    #[test]
    fn test_page_with_zero_limit_is_empty() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spool = FindingSpool::open(&spool_dir(&root)).map_err(ctx("spool opens"))?;
        spool.put(&one_record()?).map_err(ctx("put succeeds"))?;
        let (entries, cursor) = spool.page(None, 0).map_err(ctx("page"))?;
        assert!(entries.is_empty());
        assert!(cursor.is_none());
        Ok(())
    }

    #[test]
    fn test_reopen_continues_sequence() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let dir = spool_dir(&root);
        let mut batch = records(2, "reopen")?.into_iter();
        let first = {
            let spool = FindingSpool::open(&dir).map_err(ctx("spool opens"))?;
            let record = batch.next().ok_or(TestError::Missing("record"))?;
            spool.put(&record).map_err(ctx("put"))?
        };
        let spool = FindingSpool::open(&dir).map_err(ctx("spool reopens"))?;
        let record = batch.next().ok_or(TestError::Missing("record"))?;
        let second = spool.put(&record).map_err(ctx("put"))?;
        assert!(second.seq() > first.seq());
        Ok(())
    }

    #[test]
    fn test_open_rejects_symlinked_spool_dir() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let target = root.path().join("real");
        std::fs::create_dir(&target).map_err(ctx("target dir"))?;
        let link = root.path().join("findings");
        std::os::unix::fs::symlink(&target, &link).map_err(ctx("symlink"))?;
        let Err(err) = FindingSpool::open(&link) else {
            return Err(TestError::Unexpected(
                "symlinked dir rejected: expected Err".into(),
            ));
        };
        assert!(matches!(err, SpoolError::SymlinkRejected { .. }));
        Ok(())
    }

    #[test]
    fn test_open_rejects_world_writable_dir() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let dir = spool_dir(&root);
        std::fs::create_dir(&dir).map_err(ctx("dir"))?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777))
            .map_err(ctx("chmod"))?;
        let Err(err) = FindingSpool::open(&dir) else {
            return Err(TestError::Unexpected(
                "world-writable dir rejected: expected Err".into(),
            ));
        };
        assert!(matches!(err, SpoolError::InsecurePermissions { .. }));
        Ok(())
    }

    #[test]
    fn test_get_rejects_symlinked_record() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spool = FindingSpool::open(&spool_dir(&root)).map_err(ctx("spool opens"))?;
        let id = spool.put(&one_record()?).map_err(ctx("put succeeds"))?;
        let path = spool.dir().join(id.file_name());
        let outside = root.path().join("outside.json");
        std::fs::rename(&path, &outside).map_err(ctx("move record out"))?;
        std::os::unix::fs::symlink(&outside, &path).map_err(ctx("symlink record"))?;

        let Err(err) = spool.get(&id) else {
            return Err(TestError::Unexpected(
                "symlinked record rejected: expected Err".into(),
            ));
        };
        assert!(matches!(err, SpoolError::SymlinkRejected { .. }));

        let (entries, _) = spool.page(None, 10).map_err(ctx("page still works"))?;
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            entries[0].record,
            Err(SpoolError::SymlinkRejected { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_put_rejects_record_over_size_limit() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let limits = SpoolLimits {
            max_record_bytes: 64,
            max_entries: DEFAULT_MAX_ENTRIES,
            ..SpoolLimits::default()
        };
        let spool =
            FindingSpool::open_with_limits(&spool_dir(&root), limits).map_err(ctx("opens"))?;
        let Err(err) = spool.put(&one_record()?) else {
            return Err(TestError::Unexpected(
                "oversized record rejected: expected Err".into(),
            ));
        };
        assert!(matches!(err, SpoolError::RecordTooLarge { limit: 64, .. }));
        let (entries, _) = spool.page(None, 10).map_err(ctx("page"))?;
        assert!(entries.is_empty());
        Ok(())
    }

    #[test]
    fn test_get_rejects_stored_record_over_size_limit() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let dir = spool_dir(&root);
        let opened = FindingSpool::open(&dir).map_err(ctx("spool opens"))?;
        let id = opened.put(&one_record()?).map_err(ctx("put succeeds"))?;
        let limits = SpoolLimits {
            max_record_bytes: 64,
            max_entries: DEFAULT_MAX_ENTRIES,
            ..SpoolLimits::default()
        };
        let small = FindingSpool::open_with_limits(&dir, limits).map_err(ctx("reopens"))?;
        let Err(err) = small.get(&id) else {
            return Err(TestError::Unexpected(
                "oversized stored record rejected: expected Err".into(),
            ));
        };
        assert!(matches!(err, SpoolError::RecordTooLarge { limit: 64, .. }));
        Ok(())
    }

    #[test]
    fn test_get_rejects_tampered_record_content() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spool = FindingSpool::open(&spool_dir(&root)).map_err(ctx("spool opens"))?;
        let id = spool.put(&one_record()?).map_err(ctx("put succeeds"))?;
        let path = spool.dir().join(id.file_name());
        let bytes = std::fs::read(&path).map_err(ctx("read"))?;
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).map_err(ctx("json"))?;
        value["severity"] = serde_json::Value::String("info".to_owned());
        let encoded = serde_json::to_vec(&value).map_err(ctx("encode"))?;
        std::fs::write(&path, encoded).map_err(ctx("write"))?;
        let Err(err) = spool.get(&id) else {
            return Err(TestError::Unexpected(
                "tampered record rejected: expected Err".into(),
            ));
        };
        assert!(matches!(err, SpoolError::Decode { .. }));
        Ok(())
    }

    #[test]
    fn test_get_rejects_record_under_foreign_digest_name() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spool = FindingSpool::open(&spool_dir(&root)).map_err(ctx("spool opens"))?;
        let mut batch = records(2, "swap")?.into_iter();
        let first = batch.next().ok_or(TestError::Missing("record"))?;
        let a = spool.put(&first).map_err(ctx("put"))?;
        let second = batch.next().ok_or(TestError::Missing("record"))?;
        let b = spool.put(&second).map_err(ctx("put"))?;
        let bytes_b = std::fs::read(spool.dir().join(b.file_name())).map_err(ctx("read b"))?;
        std::fs::write(spool.dir().join(a.file_name()), bytes_b).map_err(ctx("overwrite a"))?;
        let Err(err) = spool.get(&a) else {
            return Err(TestError::Unexpected(
                "digest mismatch detected: expected Err".into(),
            ));
        };
        assert!(matches!(err, SpoolError::DigestMismatch { .. }));
        Ok(())
    }

    #[test]
    fn test_put_rejects_when_spool_is_full() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let limits = SpoolLimits {
            max_record_bytes: DEFAULT_MAX_RECORD_BYTES,
            max_entries: 1,
            ..SpoolLimits::default()
        };
        let spool =
            FindingSpool::open_with_limits(&spool_dir(&root), limits).map_err(ctx("opens"))?;
        let mut batch = records(2, "full")?.into_iter();
        let first = batch.next().ok_or(TestError::Missing("record"))?;
        spool.put(&first).map_err(ctx("first put"))?;
        let second = batch.next().ok_or(TestError::Missing("record"))?;
        let Err(err) = spool.put(&second) else {
            return Err(TestError::Unexpected(
                "second put rejected: expected Err".into(),
            ));
        };
        assert!(matches!(err, SpoolError::SpoolFull { limit: 1 }));
        Ok(())
    }

    // --- Aufbewahrung (Opt-in) -------------------------------------------

    // Legt `n` Records ab, datiert Record `i` auf `now - (n - i) * 1h` zurück
    // und liefert die Kennungen (älteste zuerst) samt `now`.
    fn aged_spool(
        root: &tempfile::TempDir,
        limits: SpoolLimits,
        n: usize,
    ) -> TestResult<(FindingSpool, Vec<SpoolId>, SystemTime)> {
        let spool =
            FindingSpool::open_with_limits(&spool_dir(root), limits).map_err(ctx("opens"))?;
        let now = SystemTime::now();
        let mut ids = Vec::new();
        for (i, record) in records(n, "retention")?.iter().enumerate() {
            let id = spool.put(record).map_err(ctx("put"))?;
            let age = Duration::from_secs(3600 * (n - i) as u64);
            let when = now
                .checked_sub(age)
                .ok_or(TestError::Missing("time arithmetic"))?;
            let path = spool.dir().join(id.file_name());
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .and_then(|f| f.set_modified(when))
                .map_err(ctx("set mtime"))?;
            ids.push(id);
        }
        Ok((spool, ids, now))
    }

    fn cursor_of(id: &SpoolId) -> TestResult<SpoolCursor> {
        SpoolCursor::parse(&id.to_string()).ok_or(TestError::Missing("cursor parses"))
    }

    fn exists(spool: &FindingSpool, id: &SpoolId) -> bool {
        spool.dir().join(id.file_name()).exists()
    }

    fn nth(ids: &[SpoolId], n: usize) -> TestResult<&SpoolId> {
        ids.get(n).ok_or(TestError::Missing("spool id"))
    }

    #[test]
    fn test_default_limits_never_delete_anything() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let (spool, ids, now) = aged_spool(&root, SpoolLimits::default(), 3)?;
        let last = nth(&ids, 2)?;
        let far_future = now + Duration::from_secs(10 * 365 * 86_400);
        let report = spool
            .sweep(Some(&cursor_of(last)?), far_future)
            .map_err(ctx("sweep"))?;
        assert_eq!(report, SweepReport::default());
        assert!(
            spool
                .maybe_sweep(None, far_future)
                .map_err(ctx("maybe"))?
                .is_none()
        );
        assert!(ids.iter().all(|id| exists(&spool, id)));
        Ok(())
    }

    #[test]
    fn test_limits_without_apply_only_report() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let limits = SpoolLimits {
            max_age: Some(Duration::from_secs(90 * 60)),
            ..SpoolLimits::default()
        };
        let (spool, ids, now) = aged_spool(&root, limits, 3)?;
        let last = nth(&ids, 2)?;
        let report = spool
            .sweep(Some(&cursor_of(last)?), now)
            .map_err(ctx("sweep"))?;
        // Alter 3h und 2h überschreiten 90 min, 1h nicht.
        assert!(!report.applied);
        assert_eq!(
            (report.removed, report.would_remove, report.kept),
            (0, 2, 1)
        );
        assert!(ids.iter().all(|id| exists(&spool, id)));
        Ok(())
    }

    #[test]
    fn test_apply_age_removes_only_consumed_records() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let limits = SpoolLimits {
            max_age: Some(Duration::from_secs(90 * 60)),
            retention_apply: true,
            ..SpoolLimits::default()
        };
        let (spool, ids, now) = aged_spool(&root, limits, 3)?;
        // Nur der älteste ist konsumiert; der zweite ist zu alt, aber
        // unkonsumiert und bleibt.
        let first = nth(&ids, 0)?;
        let report = spool
            .sweep(Some(&cursor_of(first)?), now)
            .map_err(ctx("sweep"))?;
        assert_eq!(
            (report.removed, report.kept, report.kept_unconsumed),
            (1, 2, 2)
        );
        assert!(!exists(&spool, first));
        assert!(ids.iter().skip(1).all(|id| exists(&spool, id)));
        // Ohne Cursor wird nichts gelöscht.
        let report = spool.sweep(None, now).map_err(ctx("sweep"))?;
        assert_eq!(report.removed, 0);
        Ok(())
    }

    #[test]
    fn test_apply_bytes_removes_oldest_consumed_and_keeps_newest() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let probe_dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let (probe, probe_ids, _) = aged_spool(&probe_dir, SpoolLimits::default(), 4)?;
        let size = std::fs::metadata(probe.dir().join(nth(&probe_ids, 0)?.file_name()))
            .map_err(ctx("stat"))?
            .len();
        let limits = SpoolLimits {
            max_total_bytes: Some(size * 2 + size / 2),
            retention_apply: true,
            ..SpoolLimits::default()
        };
        let (spool, ids, now) = aged_spool(&root, limits, 4)?;
        let third = nth(&ids, 2)?;
        let report = spool
            .sweep(Some(&cursor_of(third)?), now)
            .map_err(ctx("sweep"))?;
        assert_eq!(report.removed, 2, "two oldest consumed records go");
        assert!(!report.over_budget);
        assert!(!exists(&spool, nth(&ids, 0)?) && !exists(&spool, nth(&ids, 1)?));
        assert!(exists(&spool, nth(&ids, 2)?) && exists(&spool, nth(&ids, 3)?));
        Ok(())
    }

    #[test]
    fn test_bytes_cap_reports_over_budget_when_nothing_is_consumed() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let limits = SpoolLimits {
            max_total_bytes: Some(1),
            retention_apply: true,
            ..SpoolLimits::default()
        };
        let (spool, ids, now) = aged_spool(&root, limits, 2)?;
        let report = spool.sweep(None, now).map_err(ctx("sweep"))?;
        assert!(report.over_budget);
        assert_eq!((report.removed, report.kept), (0, 2));
        assert!(ids.iter().all(|id| exists(&spool, id)));
        Ok(())
    }

    #[test]
    fn test_delete_unconsumed_is_a_separate_opt_in() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let limits = SpoolLimits {
            max_age: Some(Duration::from_secs(90 * 60)),
            retention_apply: true,
            delete_unconsumed: true,
            ..SpoolLimits::default()
        };
        let (spool, _ids, now) = aged_spool(&root, limits, 3)?;
        let report = spool.sweep(None, now).map_err(ctx("sweep"))?;
        assert_eq!((report.removed, report.kept), (2, 1));
        Ok(())
    }

    #[test]
    fn test_sweep_never_touches_foreign_files_or_symlinks() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let limits = SpoolLimits {
            max_age: Some(Duration::from_secs(1)),
            retention_apply: true,
            delete_unconsumed: true,
            ..SpoolLimits::default()
        };
        let (spool, ids, now) = aged_spool(&root, limits, 1)?;
        let outside = root.path().join("outside.txt");
        std::fs::write(&outside, b"keep").map_err(ctx("write outside"))?;
        let lock = spool.dir().join("spool.lock");
        std::fs::write(&lock, b"x").map_err(ctx("write lock"))?;
        let temp = spool.dir().join(".tmp-record");
        std::fs::write(&temp, b"x").map_err(ctx("write temp"))?;
        let link_id = SpoolId::parse(&format!("{:020}-{}", 99, "ab".repeat(32)))
            .ok_or(TestError::Missing("link id"))?;
        std::os::unix::fs::symlink(&outside, spool.dir().join(link_id.file_name()))
            .map_err(ctx("symlink"))?;
        let report = spool.sweep(None, now).map_err(ctx("sweep"))?;
        assert_eq!(report.removed, 1);
        assert_eq!(report.skipped_non_regular, 1);
        assert!(!exists(&spool, nth(&ids, 0)?));
        assert!(lock.exists() && temp.exists() && outside.exists());
        assert!(
            std::fs::symlink_metadata(spool.dir().join(link_id.file_name())).is_ok(),
            "symlink entry left alone"
        );
        Ok(())
    }

    #[test]
    fn test_spool_full_unchanged_when_retention_is_not_applied() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let limits = SpoolLimits {
            max_entries: 1,
            max_age: Some(Duration::from_secs(1)),
            ..SpoolLimits::default()
        };
        let (spool, _ids, _now) = aged_spool(&root, limits, 1)?;
        let record = records(2, "again")?
            .into_iter()
            .nth(1)
            .ok_or(TestError::Missing("record"))?;
        let Err(err) = spool.put(&record) else {
            return Err(TestError::Unexpected("expected SpoolFull".into()));
        };
        assert!(matches!(err, SpoolError::SpoolFull { limit: 1 }));
        Ok(())
    }

    #[test]
    fn test_open_sweeps_and_reports_and_maybe_sweep_is_rate_limited() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let limits = SpoolLimits {
            max_age: Some(Duration::from_secs(90 * 60)),
            retention_apply: true,
            delete_unconsumed: true,
            ..SpoolLimits::default()
        };
        let (spool, _ids, _now) = aged_spool(&root, limits, 3)?;
        drop(spool);
        let reopened =
            FindingSpool::open_with_limits(&spool_dir(&root), limits).map_err(ctx("reopen"))?;
        assert_eq!(reopened.open_report().removed, 2);
        let now = SystemTime::now();
        assert!(
            reopened
                .maybe_sweep(None, now)
                .map_err(ctx("maybe"))?
                .is_none()
        );
        let later = now + SWEEP_MIN_INTERVAL + Duration::from_secs(1);
        assert!(
            reopened
                .maybe_sweep(None, later)
                .map_err(ctx("maybe"))?
                .is_some()
        );
        Ok(())
    }

    #[test]
    fn test_spool_id_parse_accepts_only_canonical_form() -> TestResult {
        let digest = "ab".repeat(32);
        let good = format!("{:020}-{digest}", 5);
        let id = SpoolId::parse(&good).ok_or(TestError::Missing("canonical id parses"))?;
        assert_eq!(id.to_string(), good);
        assert_eq!(SpoolId::from_file_name(&format!("{good}.json")), Some(id));
        assert!(SpoolId::parse(&format!("{:020}-{}", 5, "AB".repeat(32))).is_none());
        assert!(SpoolId::parse(&format!("5-{digest}")).is_none());
        assert!(SpoolId::from_file_name(&format!(".{good}.json.1.tmp")).is_none());
        Ok(())
    }

    #[test]
    fn test_spool_cursor_roundtrips_through_text() -> TestResult {
        let text = format!("{:020}-{}", 9, "0".repeat(64));
        let cursor = SpoolCursor::parse(&text).ok_or(TestError::Missing("cursor parses"))?;
        assert_eq!(cursor.after().seq(), 9);
        assert_eq!(cursor.to_string(), text);
        Ok(())
    }

    #[test]
    fn test_spool_error_display_names_limit() {
        let err = SpoolError::RecordTooLarge {
            len: 100,
            limit: 64,
        };
        assert_eq!(
            err.to_string(),
            "finding spool: record of 100 bytes exceeds the limit of 64 bytes"
        );
    }
}
