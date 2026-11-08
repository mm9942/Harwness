//! Laufzeitprüfung der eBPF-Objekte gegen das SHA-256-Manifest des
//! BPF-Builds (PL-100 Phase 4, THREAT-MODEL B23, INVENTORY H12).
//!
//! # Zweck
//! `harw-dod/bpf/scripts/write-manifest.sh` schreibt beim Bau neben die vier
//! Objekte ein Manifest mit dem SHA-256-Digest jedes Objekts;
//! `harw-dod/scripts/install.sh` installiert es root-eigen mit Modus 0644 als
//! `$(BPFDIR)/manifest.json`. Bis zu diesem Modul hat die Sonde es nie
//! gelesen: ein ausgetauschtes Objekt mit passendem Programm- und Map-Namen
//! wäre geladen worden. Jetzt wird jedes aufgelöste Objekt gegen seinen
//! Manifesteintrag geprüft, **bevor** es an `harw_dod_bpf::RealBpfLoader`
//! geht ([`verify_resolved_objects`], aufgerufen von `main::run`).
//!
//! # Das Manifest
//! Trotz der Endung `.json` ist die Datei TOML (der Name steht in mehreren
//! Build- und Installationsskripten außerhalb dieser Crate). Schema 1, genau
//! so, wie `write-manifest.sh` es erzeugt:
//!
//! ```text
//! schema_version = 1
//! wire_abi_version = 1
//! bpf_target = "bpfel"
//! host_arch = "x86_64"
//! clang_version = "..."
//! bpftool_version = "..."
//! vmlinux_btf_sha256 = "<64 Hexziffern>"
//!
//! [[objects]]
//! name = "exec.bpf.o"
//! program = "dod_sched_process_exec"
//! attach = "sched:sched_process_exec"
//! sha256 = "<64 Hexziffern>"
//! maps = ["EVENTS", "SCOPE_CGROUP_IDS", "LOSS_COUNTS", "SEQUENCE"]
//! ```
//!
//! Die Grammatik ist streng ([`BpfManifest::parse`]): ein anderes Schema,
//! eine andere Wire-ABI oder ein anderes BPF-Ziel gelten als nicht
//! unterstützt; unbekannte oder fehlende Schlüssel, unbekannte oder doppelte
//! Objektnamen, Programm- und Anheftnamen, die nicht den
//! `harw_dod_bpf`-Vertragskonstanten entsprechen, ein anderer Map-Satz als
//! `harw_dod_bpf::REQUIRED_MAP_NAMES` und ein Digest, der nicht aus genau 64
//! Hexziffern besteht, verwerfen das ganze Manifest. Ein Manifest darf
//! weniger als vier Objekte nennen; ein Objekt ohne Eintrag wird dann bei der
//! Prüfung abgelehnt ([`IntegrityFailure::EntryMissing`]). `host_arch`, die
//! Werkzeugversionen und `vmlinux_btf_sha256` werden nur auf ihre Form
//! geprüft, nicht gegen den laufenden Host.
//!
//! `src/testdata/manifest.sample.toml` ist die echte Ausgabe von
//! `write-manifest.sh` für feste Platzhaltereingaben; ein Test parst sie und
//! prüft ihre Digests, und
//! `make -C harw-dod/bpf check-manifest-writer` vergleicht die aktuelle
//! Skriptausgabe mit ihr. So können Schreiber und Parser nicht unbemerkt
//! auseinanderlaufen (bis PL-100 Phase 4 schrieb das Skript den BTF-Digest
//! mit Zeilenumbruch in den String, und jedes echte Manifest war ungültiges
//! TOML).
//!
//! # Vertrauen in die Datei
//! Ein Digest schützt nur, solange das Manifest selbst nicht austauschbar
//! ist. [`load_manifest`] öffnet es deshalb über eine Deskriptorkette ab `/`
//! (`openat` mit `O_NOFOLLOW`, dieselbe Bauweise wie `harw-dod-config` für
//! `/etc/harw-dod/config.toml`) und verlangt für **jede** Verzeichnisstufe
//! und für die Datei selbst: kein Symlink, der erwartete Typ, Eigentümer
//! root ([`MANIFEST_OWNER_UID`]) und weder gruppen- noch weltschreibbar
//! (`mode & 0o022 == 0`). Eine schreibbare Zwischenstufe ließe einen
//! unprivilegierten Nutzer einen eigenen Baum an diese Stelle verschieben.
//! Die Objekte selbst brauchen keine Eigentumsprüfung: ihre Bytes sind über
//! den Digest an das vertrauenswürdige Manifest gebunden.
//!
//! Ein Entwicklungsaufbau (`HARW_DOD_BPF_DIR`, Objekte unter dem eigenen
//! Home) braucht deshalb ein root-eigenes Manifest in einem root-eigenen
//! Verzeichnis, z. B. nach `make install`; ein Manifest im Arbeitsbaum des
//! Entwicklers wird abgelehnt.
//!
//! # Warum keine Einbettung zur Bauzeit
//! Die Objekte entstehen außerhalb von Cargo (`harw-dod/bpf/Makefile`) und
//! werden zur Laufzeit vom Datenträger gelesen (`crate::sensors`). Ein
//! eingebettetes Manifest würde Rust- und BPF-Build koppeln und jeden
//! Objektneubau an einen Neubau der Sonde binden. Geprüft wird deshalb zur
//! Laufzeit gegen eine root-eigene Datei.
//!
//! # Welche Bytes geprüft werden
//! Genau die Bytes, die `crate::sensors` einmal gelesen hat und die als
//! `harw_dod_bpf::BpfProgramSource::Embedded` an den Lader gehen. Es gibt
//! kein zweites Lesen und damit kein Fenster zwischen Prüfung und Laden.
//!
//! # Fehlerbild
//! Jede Abweichung ist ein harter Startabbruch
//! (`crate::error::ProbeError::BpfObjectIntegrity`), nie eine Degradierung:
//! ein Objekt, das vorhanden ist, aber nicht zum Manifest passt, ist ein
//! Manipulationsverdacht, kein Betriebsfall. Ein **fehlendes** Objekt bleibt
//! dagegen die dokumentierte Degradierung aus `crate::sensors`; es wird gar
//! nicht erst geprüft. `main::run` meldet vor dem Abbruch jeden gewählten
//! Sensor einmal als `SensorDegraded` an den Sentinel; `main` endet mit dem
//! Exit-Status 78 (`EX_CONFIG`), den die systemd-Unit nicht neu startet.
//!
//! # Protokoll
//! Fehler tragen nur den Objektnamen und einen festen Grund
//! ([`IntegrityFailure`]): keinen Pfad, keinen Digest, keinen Dateiinhalt.
//! Der Manifestpfad erscheint nur auf `info`, die genaue verletzte Regel nur
//! auf `debug`.
//!
//! # Exportierte Typen
//! [`MANIFEST_OWNER_UID`], [`MAX_MANIFEST_BYTES`], [`MANIFEST_SCHEMA_VERSION`],
//! [`IntegrityFailure`], [`BpfManifest`], [`load_manifest`],
//! [`verify_resolved_objects`].
//!
//! # Nebenläufigkeit
//! Läuft einmal beim Start im Hauptthread, vor Landlock und vor jedem
//! Ladeschritt. [`BpfManifest`] ist unveränderliche Daten (`Send + Sync`).
//!
//! # Fehler
//! [`IntegrityFailure`] für die Bausteine, `crate::error::ProbeError` für
//! [`verify_resolved_objects`].
//!
//! # Examples
//! ```rust,ignore
//! use std::path::Path;
//!
//! use crate::bpf_manifest::verify_resolved_objects;
//! use crate::sensors::resolve_procmon_objects;
//!
//! let objects = resolve_procmon_objects(None, None)?;
//! let refs: Vec<_> = objects.iter().collect();
//! verify_resolved_objects(
//!     Some(Path::new("/usr/local/lib/harw-dod/bpf/manifest.json")),
//!     &refs,
//! )?;
//! ```

use std::fmt;
use std::fs::File;
use std::io::{ErrorKind, Read as _};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Component, Path};

use harw_dod_bpf::BpfProgramSource;
use rustix::fs::{CWD, FileType, Mode, OFlags, RawMode};
use rustix::io::{Errno, retry_on_intr};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::error::ProbeError;
use crate::sensors::{BpfObjectKind, ResolvedBpfObject};

/// Erwarteter Eigentümer des Manifests und jeder Verzeichnisstufe darüber:
/// **root** (uid 0).
///
/// # Description
/// `harw-dod/scripts/install.sh` legt Manifest und Objektverzeichnis als
/// root an; dieselbe Regel wie `harw-dod-config` für die Systemkonfiguration.
pub const MANIFEST_OWNER_UID: u32 = 0;

/// Größenobergrenze des Manifests: **64 KiB**.
///
/// # Description
/// Ein Schema-1-Manifest mit vier Objekten hat unter 2 KiB. Die Grenze
/// verhindert nur, dass ein falsch konfigurierter Pfad unbegrenzt in den
/// Speicher dieses privilegierten Prozesses gelesen wird.
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

/// Die einzige unterstützte Schemaversion des Manifests: **1**.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Das einzige unterstützte BPF-Ziel (`clang -target bpfel`).
const MANIFEST_BPF_TARGET: &str = "bpfel";

/// Länge eines SHA-256-Digests in Bytes.
const SHA256_LEN: usize = 32;

/// Die vier Objekte, die ein Manifest nennen darf.
const KNOWN_OBJECTS: [BpfObjectKind; 4] = [
    BpfObjectKind::Exec,
    BpfObjectKind::Exit,
    BpfObjectKind::TcpV4Connect,
    BpfObjectKind::TcpV6Connect,
];

/// Warum ein Objekt oder das Manifest die Integritätsprüfung nicht besteht.
///
/// # Description
/// Inhaltsfrei: kein Pfad, kein Digest, kein Dateiinhalt (siehe Moduldoku,
/// Abschnitt „Protokoll"). Jede Variante ist ein harter Startabbruch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityFailure {
    /// Mindestens ein Objekt soll geladen werden, aber `--bpf-manifest`
    /// fehlt.
    ManifestNotConfigured,
    /// Der Manifestpfad ist nicht absolut oder enthält ein `.`- oder
    /// `..`-Segment (an beliebiger Stelle, auch innen oder am Ende).
    ManifestPathInvalid,
    /// Das Manifest oder eine Verzeichnisstufe ließ sich nicht öffnen oder
    /// lesen (z. B. `NotFound`, `PermissionDenied`).
    ManifestUnreadable(ErrorKind),
    /// Das Manifest oder eine Verzeichnisstufe darüber ist ein Symlink, hat
    /// den falschen Typ, gehört nicht root oder ist gruppen- bzw.
    /// weltschreibbar.
    ManifestUntrusted,
    /// Das Manifest überschreitet [`MAX_MANIFEST_BYTES`].
    ManifestTooLarge,
    /// Das Manifest ist kein gültiges Schema-1-Manifest (siehe Moduldoku,
    /// Abschnitt „Das Manifest").
    ManifestMalformed,
    /// Schema, Wire-ABI oder BPF-Ziel des Manifests werden nicht unterstützt.
    ManifestUnsupported,
    /// Das Manifest hat keinen Eintrag für dieses Objekt.
    EntryMissing,
    /// Der SHA-256-Digest der gelesenen Objektbytes weicht vom Manifest ab.
    DigestMismatch,
    /// Die Objektbytes liegen nicht eingelesen vor
    /// (`BpfProgramSource::Path` statt `Embedded`); ohne die zu ladenden
    /// Bytes ist keine Prüfung möglich.
    SourceNotEmbedded,
}

impl fmt::Display for IntegrityFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ManifestNotConfigured => {
                f.write_str("no bpf object manifest is configured (--bpf-manifest)")
            }
            Self::ManifestPathInvalid => f.write_str(
                "the bpf object manifest path must be absolute and free of '.' and '..' components",
            ),
            Self::ManifestUnreadable(kind) => {
                write!(f, "the bpf object manifest is unreadable: {kind}")
            }
            Self::ManifestUntrusted => f.write_str(
                "the bpf object manifest or a directory above it is a symlink, has the wrong file type, is not owned by root or is group- or world-writable",
            ),
            Self::ManifestTooLarge => write!(
                f,
                "the bpf object manifest exceeds the limit of {MAX_MANIFEST_BYTES} bytes"
            ),
            Self::ManifestMalformed => f.write_str("the bpf object manifest is malformed"),
            Self::ManifestUnsupported => f.write_str(
                "the bpf object manifest has an unsupported schema, wire ABI or bpf target",
            ),
            Self::EntryMissing => f.write_str("the manifest has no entry for this object"),
            Self::DigestMismatch => {
                f.write_str("the sha256 digest of the object does not match the manifest")
            }
            Self::SourceNotEmbedded => {
                f.write_str("the object bytes were not read before verification")
            }
        }
    }
}

/// Ein geprüfter Manifesteintrag: Objekt und erwarteter Digest.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ManifestEntry {
    // Das Objekt, das dieser Eintrag beschreibt.
    object: BpfObjectKind,
    // Der erwartete SHA-256-Digest der Objektbytes.
    sha256: [u8; SHA256_LEN],
}

/// Ein geprüftes Schema-1-Manifest.
///
/// # Description
/// Entsteht nur über [`BpfManifest::parse`] (bzw. [`load_manifest`]) und
/// enthält dann höchstens einen Eintrag je bekanntem Objekt, jeweils mit
/// vertragsgemäßem Programm-, Anheft- und Map-Satz.
///
/// # Concurrency
/// Unveränderliche Daten, `Send + Sync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BpfManifest {
    // Die Einträge in Manifest-Reihenfolge, jedes Objekt höchstens einmal.
    entries: Vec<ManifestEntry>,
}

/// Rohform des Manifests, wie `write-manifest.sh` sie schreibt.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    // Schemaversion; nur 1 wird unterstützt.
    schema_version: u32,
    // Wire-ABI der Objekte; muss `harw_dod_bpf::WIRE_VERSION_V1` sein.
    wire_abi_version: u32,
    // BPF-Ziel des Compilers; nur `bpfel`.
    bpf_target: String,
    // Architektur des Bauhosts (nur auf Form geprüft).
    host_arch: String,
    // Gesperrte clang-Version (nur auf Form geprüft).
    clang_version: String,
    // Gesperrte bpftool-Version (nur auf Form geprüft).
    bpftool_version: String,
    // SHA-256 der Ziel-BTF (nur auf Form geprüft).
    vmlinux_btf_sha256: String,
    // Ein Eintrag je Objekt.
    objects: Vec<RawObject>,
}

/// Rohform eines `[[objects]]`-Eintrags.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawObject {
    // Dateiname, z. B. `exec.bpf.o`.
    name: String,
    // Programmname im ELF-Objekt.
    program: String,
    // Anheftpunkt.
    attach: String,
    // SHA-256 der Objektdatei als 64 Hexziffern.
    sha256: String,
    // Map-Namen des Objekts.
    maps: Vec<String>,
}

impl BpfManifest {
    /// Parst und prüft den Text eines Schema-1-Manifests.
    ///
    /// # Arguments
    /// - `text` (`&str`): der Dateiinhalt.
    ///
    /// # Returns
    /// Das geprüfte Manifest.
    ///
    /// # Errors
    /// [`IntegrityFailure::ManifestUnsupported`] für ein anderes Schema, eine
    /// andere Wire-ABI oder ein anderes BPF-Ziel;
    /// [`IntegrityFailure::ManifestMalformed`] für jede andere Verletzung der
    /// Grammatik (siehe Moduldoku, Abschnitt „Das Manifest").
    ///
    /// # Examples
    /// ```rust,ignore
    /// let manifest = BpfManifest::parse(&text)?;
    /// manifest.verify(BpfObjectKind::Exec, &exec_bytes)?;
    /// ```
    pub fn parse(text: &str) -> Result<Self, IntegrityFailure> {
        let table = text.parse::<toml::Table>().map_err(|err| {
            tracing::debug!(offset = ?err.span(), "bpf object manifest is not valid TOML");
            IntegrityFailure::ManifestMalformed
        })?;
        // Die Schemaversion zuerst und für sich: ein Manifest eines späteren
        // Schemas soll als „nicht unterstützt" scheitern, nicht als
        // „fehlerhaft" an einem neuen Schlüssel.
        match table
            .get("schema_version")
            .and_then(toml::Value::as_integer)
        {
            Some(version) if version == i64::from(MANIFEST_SCHEMA_VERSION) => {}
            Some(_) => return Err(unsupported("schema_version")),
            None => return Err(malformed("schema_version missing or not an integer")),
        }
        let raw = table
            .try_into::<RawManifest>()
            .map_err(|_| malformed("schema 1 grammar"))?;
        Self::from_raw(raw)
    }

    /// Prüft die Rohform gegen Schema 1 und die Vertragskonstanten.
    fn from_raw(raw: RawManifest) -> Result<Self, IntegrityFailure> {
        if raw.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(unsupported("schema_version"));
        }
        if raw.wire_abi_version != u32::from(harw_dod_bpf::WIRE_VERSION_V1) {
            return Err(unsupported("wire_abi_version"));
        }
        if raw.bpf_target != MANIFEST_BPF_TARGET {
            return Err(unsupported("bpf_target"));
        }
        if raw.host_arch.is_empty()
            || raw.clang_version.is_empty()
            || raw.bpftool_version.is_empty()
        {
            return Err(malformed("empty build provenance"));
        }
        if decode_sha256(&raw.vmlinux_btf_sha256).is_none() {
            return Err(malformed("vmlinux_btf_sha256 is not 64 hex digits"));
        }
        if raw.objects.is_empty() {
            return Err(malformed("no objects"));
        }

        let mut entries: Vec<ManifestEntry> = Vec::with_capacity(raw.objects.len());
        for object in raw.objects {
            let kind = KNOWN_OBJECTS
                .into_iter()
                .find(|known| known.file_name() == object.name)
                .ok_or_else(|| malformed("unknown object name"))?;
            if entries.iter().any(|entry| entry.object == kind) {
                return Err(malformed("duplicate object name"));
            }
            let (program, attach) = contract_names(kind);
            if object.program != program || object.attach != attach {
                return Err(malformed(
                    "program or attach point differs from the contract",
                ));
            }
            if !is_required_map_set(&object.maps) {
                return Err(malformed("map set differs from the contract"));
            }
            let sha256 = decode_sha256(&object.sha256)
                .ok_or_else(|| malformed("object sha256 is not 64 hex digits"))?;
            entries.push(ManifestEntry {
                object: kind,
                sha256,
            });
        }
        Ok(Self { entries })
    }

    /// Prüft die Bytes eines Objekts gegen seinen Manifesteintrag.
    ///
    /// # Arguments
    /// - `object` (`BpfObjectKind`): welches Objekt.
    /// - `bytes` (`&[u8]`): genau die Bytes, die geladen werden sollen.
    ///
    /// # Returns
    /// `Ok(())`, wenn der SHA-256-Digest der Bytes dem Eintrag entspricht.
    ///
    /// # Errors
    /// [`IntegrityFailure::EntryMissing`] ohne Eintrag,
    /// [`IntegrityFailure::DigestMismatch`] bei abweichendem Digest.
    pub fn verify(&self, object: BpfObjectKind, bytes: &[u8]) -> Result<(), IntegrityFailure> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.object == object)
            .ok_or(IntegrityFailure::EntryMissing)?;
        let actual = Sha256::digest(bytes);
        if actual.as_slice() == entry.sha256.as_slice() {
            Ok(())
        } else {
            Err(IntegrityFailure::DigestMismatch)
        }
    }
}

/// Liest und prüft das Manifest unter einem absoluten Pfad.
///
/// # Description
/// Öffnet den Pfad über eine Deskriptorkette ab `/`; jede Stufe und die
/// Datei müssen root gehören und dürfen weder gruppen- noch weltschreibbar
/// sein (siehe Moduldoku, Abschnitt „Vertrauen in die Datei"). Liest
/// höchstens [`MAX_MANIFEST_BYTES`] und parst mit [`BpfManifest::parse`].
///
/// # Arguments
/// - `path` (`&Path`): `--bpf-manifest`.
///
/// # Returns
/// Das geprüfte Manifest.
///
/// # Errors
/// [`IntegrityFailure::ManifestPathInvalid`],
/// [`IntegrityFailure::ManifestUnreadable`],
/// [`IntegrityFailure::ManifestUntrusted`],
/// [`IntegrityFailure::ManifestTooLarge`] und die Fehler von
/// [`BpfManifest::parse`].
///
/// # Concurrency
/// Blockierende Dateisystemaufrufe; nur beim Start im Hauptthread.
pub fn load_manifest(path: &Path) -> Result<BpfManifest, IntegrityFailure> {
    // Vor `strip_prefix`: das kürzt führende und abschließende `.`-Segmente
    // stillschweigend weg.
    if has_dot_segment(path) {
        return Err(IntegrityFailure::ManifestPathInvalid);
    }
    let relative = path
        .strip_prefix("/")
        .map_err(|_| IntegrityFailure::ManifestPathInvalid)?;
    load_manifest_below(Path::new("/"), relative, MANIFEST_OWNER_UID)
}

/// Wie [`load_manifest`], aber ab einem frei gewählten, selbst geprüften
/// Anker und mit frei gewähltem Eigentümer — die Produktion nutzt `/` und
/// root, Tests ein eigenes Temporärverzeichnis und die eigene uid.
fn load_manifest_below(
    anchor: &Path,
    relative: &Path,
    owner: u32,
) -> Result<BpfManifest, IntegrityFailure> {
    let file = open_trusted_file(anchor, relative, owner)?;
    let text = read_bounded(file)?;
    BpfManifest::parse(&text)
}

/// Öffnet `anchor/relative` über eine geprüfte Deskriptorkette.
///
/// # Description
/// `relative` darf nur aus normalen Namen bestehen (kein `.`, kein `..`,
/// mindestens ein Name). Der Anker und jede Zwischenstufe werden als
/// Verzeichnis geöffnet, der letzte Name als reguläre Datei; jede Stufe mit
/// `O_NOFOLLOW` und der Eigentums- und Modusprüfung.
fn open_trusted_file(anchor: &Path, relative: &Path, owner: u32) -> Result<File, IntegrityFailure> {
    // `Path::components` entfernt innere und abschließende `.` still; die
    // Regel „kein `.`" wird deshalb zuerst auf den Rohbytes geprüft.
    if has_dot_segment(relative) {
        return Err(IntegrityFailure::ManifestPathInvalid);
    }
    let mut names: Vec<&Path> = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(name) => names.push(Path::new(name)),
            Component::RootDir
            | Component::CurDir
            | Component::ParentDir
            | Component::Prefix(_) => {
                return Err(IntegrityFailure::ManifestPathInvalid);
            }
        }
    }
    let Some((file_name, directories)) = names.split_last() else {
        return Err(IntegrityFailure::ManifestPathInvalid);
    };
    let mut directory = open_trusted_directory(CWD, anchor, owner)?;
    for name in directories {
        directory = open_trusted_directory(directory.as_fd(), name, owner)?;
    }
    open_trusted_regular_file(directory.as_fd(), file_name, owner)
}

/// `true`, wenn ein `/`-getrenntes Segment des Pfads genau `.` ist.
///
/// # Description
/// Arbeitet auf den Rohbytes, weil `Path::components` innere und
/// abschließende `.`-Segmente stillschweigend entfernt (`/a/./b` und
/// `/a/b/.` ergäben dieselben Komponenten wie `/a/b`). Leere Segmente
/// (`//`) bleiben erlaubt; `..` erkennt die Komponentenprüfung.
fn has_dot_segment(path: &Path) -> bool {
    path.as_os_str()
        .as_encoded_bytes()
        .split(|byte| *byte == b'/')
        .any(|segment| segment == b".")
}

/// Öffnet eine Verzeichnisstufe ohne Symlink-Auflösung und prüft sie.
fn open_trusted_directory(
    parent: impl AsFd,
    name: &Path,
    owner: u32,
) -> Result<OwnedFd, IntegrityFailure> {
    let parent = parent.as_fd();
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = retry_on_intr(|| rustix::fs::openat(parent, name, flags, Mode::empty()))
        .map_err(open_failure)?;
    let stat = rustix::fs::fstat(&fd).map_err(io_failure)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return Err(untrusted("directory component is not a directory"));
    }
    ensure_trusted_mode(stat.st_uid, stat.st_mode, owner)?;
    Ok(fd)
}

/// Öffnet die Manifestdatei ohne Symlink-Auflösung und prüft Typ, Eigentum,
/// Modus und Größe.
///
/// # Description
/// `O_NONBLOCK` beim Öffnen verhindert, dass ein FIFO an dieser Stelle den
/// Start blockiert; nach der Typprüfung wird das Flag wieder gelöscht (wie
/// in `harw-dod-config`).
fn open_trusted_regular_file(
    parent: impl AsFd,
    name: &Path,
    owner: u32,
) -> Result<File, IntegrityFailure> {
    let parent = parent.as_fd();
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let fd = retry_on_intr(|| rustix::fs::openat(parent, name, flags, Mode::empty()))
        .map_err(open_failure)?;
    let stat = rustix::fs::fstat(&fd).map_err(io_failure)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(untrusted("manifest is not a regular file"));
    }
    ensure_trusted_mode(stat.st_uid, stat.st_mode, owner)?;
    if u64::try_from(stat.st_size).unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES {
        return Err(IntegrityFailure::ManifestTooLarge);
    }
    let status = rustix::fs::fcntl_getfl(&fd).map_err(io_failure)?;
    if status.contains(OFlags::NONBLOCK) {
        rustix::fs::fcntl_setfl(&fd, status.difference(OFlags::NONBLOCK)).map_err(io_failure)?;
    }
    Ok(File::from(fd))
}

/// Die Vertrauensregel jeder Stufe: Eigentümer `owner`, weder gruppen- noch
/// weltschreibbar.
fn ensure_trusted_mode(uid: u32, mode: RawMode, owner: u32) -> Result<(), IntegrityFailure> {
    if uid != owner {
        return Err(untrusted("unexpected owner"));
    }
    if mode & 0o022 != 0 {
        return Err(untrusted("group- or world-writable"));
    }
    Ok(())
}

/// Liest höchstens [`MAX_MANIFEST_BYTES`] als UTF-8.
///
/// # Description
/// Begrenzt über `Read::take` auf ein Byte mehr als die Grenze, damit
/// Überlänge erkennbar ist. Bewusst kein Einlesen „bis zum Streamende" ohne
/// Grenze (siehe `crate::push_only_guard`).
fn read_bounded(file: File) -> Result<String, IntegrityFailure> {
    let mut text = String::new();
    file.take(MAX_MANIFEST_BYTES.saturating_add(1))
        .read_to_string(&mut text)
        .map_err(|err| {
            if err.kind() == ErrorKind::InvalidData {
                malformed("not UTF-8")
            } else {
                IntegrityFailure::ManifestUnreadable(err.kind())
            }
        })?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES {
        return Err(IntegrityFailure::ManifestTooLarge);
    }
    Ok(text)
}

/// Bildet einen `openat`-Fehler ab: ein Symlink (`ELOOP`) oder eine
/// Nicht-Verzeichnis-Stufe (`ENOTDIR`) ist nicht vertrauenswürdig, alles
/// andere unlesbar.
fn open_failure(errno: Errno) -> IntegrityFailure {
    if errno == Errno::LOOP || errno == Errno::NOTDIR {
        untrusted("symlink or non-directory component")
    } else {
        io_failure(errno)
    }
}

/// Bildet einen sonstigen Systemaufruffehler auf
/// [`IntegrityFailure::ManifestUnreadable`] ab.
fn io_failure(errno: Errno) -> IntegrityFailure {
    IntegrityFailure::ManifestUnreadable(std::io::Error::from(errno).kind())
}

/// [`IntegrityFailure::ManifestMalformed`] mit der verletzten Regel auf `debug`.
fn malformed(check: &'static str) -> IntegrityFailure {
    tracing::debug!(check, "bpf object manifest rejected as malformed");
    IntegrityFailure::ManifestMalformed
}

/// [`IntegrityFailure::ManifestUnsupported`] mit dem Feld auf `debug`.
fn unsupported(field: &'static str) -> IntegrityFailure {
    tracing::debug!(field, "bpf object manifest rejected as unsupported");
    IntegrityFailure::ManifestUnsupported
}

/// [`IntegrityFailure::ManifestUntrusted`] mit der verletzten Regel auf `debug`.
fn untrusted(check: &'static str) -> IntegrityFailure {
    tracing::debug!(check, "bpf object manifest path rejected as untrusted");
    IntegrityFailure::ManifestUntrusted
}

/// Programm- und Anheftname, die der v1-Vertrag für ein Objekt verlangt.
fn contract_names(object: BpfObjectKind) -> (&'static str, &'static str) {
    match object {
        BpfObjectKind::Exec => (
            harw_dod_bpf::EXEC_PROGRAM_NAME,
            harw_dod_bpf::EXEC_ATTACH_POINT,
        ),
        BpfObjectKind::Exit => (
            harw_dod_bpf::EXIT_PROGRAM_NAME,
            harw_dod_bpf::EXIT_ATTACH_POINT,
        ),
        BpfObjectKind::TcpV4Connect => (
            harw_dod_bpf::TCP_V4_CONNECT_PROGRAM_NAME,
            harw_dod_bpf::TCP_V4_CONNECT_ATTACH_POINT,
        ),
        BpfObjectKind::TcpV6Connect => (
            harw_dod_bpf::TCP_V6_CONNECT_PROGRAM_NAME,
            harw_dod_bpf::TCP_V6_CONNECT_ATTACH_POINT,
        ),
    }
}

/// `true`, wenn `maps` genau `harw_dod_bpf::REQUIRED_MAP_NAMES` enthält
/// (Reihenfolge egal, keine Dubletten, nichts zusätzlich).
fn is_required_map_set(maps: &[String]) -> bool {
    maps.len() == harw_dod_bpf::REQUIRED_MAP_NAMES.len()
        && harw_dod_bpf::REQUIRED_MAP_NAMES
            .iter()
            .all(|required| maps.iter().any(|map| map == required))
}

/// Dekodiert genau 64 Hexziffern (Groß- oder Kleinschreibung) zu einem
/// SHA-256-Digest.
fn decode_sha256(text: &str) -> Option<[u8; SHA256_LEN]> {
    let digits = text.as_bytes();
    if digits.len() != SHA256_LEN.checked_mul(2)? {
        return None;
    }
    let mut digest = [0_u8; SHA256_LEN];
    for (byte, pair) in digest.iter_mut().zip(digits.chunks_exact(2)) {
        let &[high, low] = pair else {
            return None;
        };
        *byte = hex_value(high)?
            .checked_mul(16)?
            .checked_add(hex_value(low)?)?;
    }
    Some(digest)
}

/// Der Wert einer Hexziffer.
fn hex_value(digit: u8) -> Option<u8> {
    char::from(digit)
        .to_digit(16)
        .and_then(|value| u8::try_from(value).ok())
}

/// Prüft alle aufgelösten Objekte gegen das Manifest, bevor eines geladen
/// wird.
///
/// # Description
/// Ohne aufgelöstes Objekt gibt es nichts zu prüfen (fehlende Objekte
/// degradieren ihren Sensor, siehe `crate::sensors`), und das Manifest wird
/// nicht gelesen. Sonst muss `manifest_path` gesetzt sein; das Manifest
/// wird mit [`load_manifest`] gelesen und jedes Objekt mit
/// [`BpfManifest::verify`] gegen genau die eingelesenen Bytes geprüft. Der
/// erste Verstoß wird auf `error` protokolliert (Objektname und Grund, kein
/// Pfad, kein Digest) und zurückgegeben.
///
/// # Arguments
/// - `manifest_path` (`Option<&Path>`): `--bpf-manifest`.
/// - `objects` (`&[&ResolvedBpfObject]`): die aufgelösten Objekte aller
///   gewählten Sensoren.
///
/// # Returns
/// Die Zahl der geprüften Objekte.
///
/// # Errors
/// [`ProbeError::BpfObjectIntegrity`] mit `object = None` für ein fehlendes,
/// nicht vertrauenswürdiges oder ungültiges Manifest, mit dem Objekt für
/// einen fehlenden Eintrag oder einen abweichenden Digest.
///
/// # Concurrency
/// Blockierend; nur beim Start im Hauptthread, vor Landlock.
pub fn verify_resolved_objects(
    manifest_path: Option<&Path>,
    objects: &[&ResolvedBpfObject],
) -> Result<usize, ProbeError> {
    if objects.is_empty() {
        return Ok(0);
    }
    let Some(path) = manifest_path else {
        return Err(refuse(None, IntegrityFailure::ManifestNotConfigured));
    };
    tracing::info!(
        path = %path.display(),
        objects = objects.len(),
        "verifying bpf objects against the sha256 manifest"
    );
    let manifest = load_manifest(path).map_err(|failure| refuse(None, failure))?;
    verify_against(&manifest, objects)
}

/// Prüft jedes Objekt gegen ein bereits geladenes Manifest.
fn verify_against(
    manifest: &BpfManifest,
    objects: &[&ResolvedBpfObject],
) -> Result<usize, ProbeError> {
    for resolved in objects {
        let BpfProgramSource::Embedded(bytes) = &resolved.source else {
            return Err(refuse(
                Some(resolved.object),
                IntegrityFailure::SourceNotEmbedded,
            ));
        };
        manifest
            .verify(resolved.object, bytes)
            .map_err(|failure| refuse(Some(resolved.object), failure))?;
    }
    tracing::info!(
        objects = objects.len(),
        "bpf objects match the sha256 manifest"
    );
    Ok(objects.len())
}

/// Protokolliert eine Ablehnung (ohne Pfad und Digest) und baut den Fehler.
fn refuse(object: Option<BpfObjectKind>, failure: IntegrityFailure) -> ProbeError {
    match object {
        Some(object) => tracing::error!(
            object = %object,
            failure = %failure,
            "bpf object failed manifest verification; refusing to load"
        ),
        None => tracing::error!(
            failure = %failure,
            "bpf object manifest rejected; refusing to load"
        ),
    }
    ProbeError::BpfObjectIntegrity { object, failure }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::io::ErrorKind;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::path::{Path, PathBuf};

    use harw_dod_bpf::BpfProgramSource;
    use sha2::{Digest as _, Sha256};

    use super::{
        BpfManifest, IntegrityFailure, KNOWN_OBJECTS, MANIFEST_OWNER_UID, MAX_MANIFEST_BYTES,
        contract_names, has_dot_segment, load_manifest, load_manifest_below, verify_against,
        verify_resolved_objects,
    };
    use crate::error::ProbeError;
    use crate::sensors::{
        BpfObjectKind, ResolvedBpfObject, resolve_flow_objects, resolve_procmon_objects,
    };
    use crate::test_support::{TestError, TestResult, ctx};

    /// Relativer Manifestpfad unter dem Testanker.
    const MANIFEST: &str = "bpf/manifest.json";

    /// Die ersten vier Bytes jeder ELF-Datei (wie `crate::sensors`).
    const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];

    /// Fixture-Objektbytes: ELF-Kennung plus objektspezifischer Rest, damit
    /// jedes Objekt einen eigenen Digest hat.
    fn object_bytes(object: BpfObjectKind) -> Vec<u8> {
        let mut bytes = ELF_MAGIC.to_vec();
        bytes.extend_from_slice(b"-fixture-");
        bytes.extend_from_slice(object.file_name().as_bytes());
        bytes
    }

    /// SHA-256 als 64 kleine Hexziffern, wie `sha256sum` sie schreibt.
    fn sha256_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Kopf eines Schema-1-Manifests mit frei wählbarer Schema-, ABI- und
    /// Zielangabe.
    fn header(schema: u32, abi: u32, target: &str) -> String {
        format!(
            "schema_version = {schema}\nwire_abi_version = {abi}\nbpf_target = \"{target}\"\nhost_arch = \"x86_64\"\nclang_version = \"Ubuntu clang version 18.1.3\"\nbpftool_version = \"bpftool v7.4.0\"\nvmlinux_btf_sha256 = \"{}\"\n",
            sha256_hex(b"fixture vmlinux btf")
        )
    }

    /// Ein `[[objects]]`-Eintrag mit frei wählbaren Feldern.
    fn entry(name: &str, program: &str, attach: &str, sha256: &str, maps: &str) -> String {
        format!(
            "\n[[objects]]\nname = \"{name}\"\nprogram = \"{program}\"\nattach = \"{attach}\"\nsha256 = \"{sha256}\"\nmaps = {maps}\n"
        )
    }

    /// Der Map-Satz, den `write-manifest.sh` schreibt.
    const MAPS: &str = "[\"EVENTS\", \"SCOPE_CGROUP_IDS\", \"LOSS_COUNTS\", \"SEQUENCE\"]";

    /// Ein vertragsgemäßer Eintrag für ein Objekt mit gegebenem Digest.
    fn contract_entry(object: BpfObjectKind, sha256: &str) -> String {
        let (program, attach) = contract_names(object);
        entry(object.file_name(), program, attach, sha256, MAPS)
    }

    /// Ein gültiges Manifest für die übergebenen Objekte (Fixture-Digests).
    fn manifest_for(objects: &[BpfObjectKind]) -> String {
        let mut text = header(1, 1, "bpfel");
        for &object in objects {
            text.push_str(&contract_entry(object, &sha256_hex(&object_bytes(object))));
        }
        text
    }

    /// Setzt den Modus einer Datei oder eines Verzeichnisses explizit (die
    /// umask des Testlaufs soll das Ergebnis nicht bestimmen).
    fn chmod(path: &Path, mode: u32) -> TestResult {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(ctx("set fixture permissions"))
    }

    /// Ein Testanker: Temporärverzeichnis mit `bpf/` (0755), den vier
    /// Fixture-Objekten und einem Manifest (0644).
    struct Fixture {
        // Hält das Temporärverzeichnis am Leben.
        dir: tempfile::TempDir,
        // Eigentümer der Fixture-Dateien (die uid des Testlaufs).
        owner: u32,
    }

    impl Fixture {
        /// Legt die Fixture mit dem gegebenen Manifesttext an.
        fn new(manifest: &str) -> TestResult<Self> {
            let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
            chmod(dir.path(), 0o700)?;
            let bpf = dir.path().join("bpf");
            std::fs::create_dir(&bpf).map_err(ctx("create bpf dir"))?;
            chmod(&bpf, 0o755)?;
            for object in KNOWN_OBJECTS {
                let path = bpf.join(object.file_name());
                std::fs::write(&path, object_bytes(object)).map_err(ctx("write object"))?;
                chmod(&path, 0o644)?;
            }
            let manifest_path = dir.path().join(MANIFEST);
            std::fs::write(&manifest_path, manifest).map_err(ctx("write manifest"))?;
            chmod(&manifest_path, 0o644)?;
            let owner = std::fs::metadata(dir.path())
                .map_err(ctx("stat tempdir"))?
                .uid();
            Ok(Self { dir, owner })
        }

        /// Der Anker (das Temporärverzeichnis).
        fn anchor(&self) -> &Path {
            self.dir.path()
        }

        /// Absoluter Pfad unterhalb des Ankers.
        fn path(&self, relative: &str) -> PathBuf {
            self.dir.path().join(relative)
        }

        /// Lädt das Manifest mit der uid des Testlaufs als Eigentümer.
        fn load(&self) -> Result<BpfManifest, IntegrityFailure> {
            load_manifest_below(self.anchor(), Path::new(MANIFEST), self.owner)
        }

        /// Löst alle vier Objekte über die produktiven Funktionen auf
        /// (explizite Pfade, die Prozessumgebung bleibt unberührt).
        fn resolve_all(&self) -> TestResult<Vec<ResolvedBpfObject>> {
            let exec = self.path("bpf/exec.bpf.o");
            let exit = self.path("bpf/exit.bpf.o");
            let v4 = self.path("bpf/tcp_v4_connect.bpf.o");
            let v6 = self.path("bpf/tcp_v6_connect.bpf.o");
            let [exec, exit] = resolve_procmon_objects(Some(exec.as_path()), Some(exit.as_path()))
                .map_err(|missing| TestError::Unexpected(format!("{missing:?}")))?;
            let [v4, v6] = resolve_flow_objects(Some(v4.as_path()), Some(v6.as_path()))
                .map_err(|missing| TestError::Unexpected(format!("{missing:?}")))?;
            Ok(vec![exec, exit, v4, v6])
        }
    }

    /// Erwartet einen bestimmten Fehler beim Laden.
    fn expect_load_failure(
        result: Result<BpfManifest, IntegrityFailure>,
        expected: IntegrityFailure,
    ) -> TestResult {
        match result {
            Err(failure) if failure == expected => Ok(()),
            other => Err(TestError::Unexpected(format!(
                "expected {expected:?}, got {other:?}"
            ))),
        }
    }

    /// Erwartet einen bestimmten Fehler beim Parsen.
    fn expect_parse_failure(text: &str, expected: IntegrityFailure) -> TestResult {
        expect_load_failure(BpfManifest::parse(text), expected)
    }

    /// Erwartet `ProbeError::BpfObjectIntegrity` mit Objekt und Grund.
    fn expect_refusal(
        result: Result<usize, ProbeError>,
        object: Option<BpfObjectKind>,
        failure: IntegrityFailure,
    ) -> TestResult<ProbeError> {
        match result {
            Err(ProbeError::BpfObjectIntegrity {
                object: actual_object,
                failure: actual_failure,
            }) if actual_object == object && actual_failure == failure => {
                Ok(ProbeError::BpfObjectIntegrity { object, failure })
            }
            other => Err(TestError::Unexpected(format!(
                "expected refusal {object:?}/{failure:?}, got {other:?}"
            ))),
        }
    }

    #[test]
    fn test_matching_objects_verify_against_a_trusted_manifest() -> TestResult {
        let fixture = Fixture::new(&manifest_for(&KNOWN_OBJECTS))?;
        let manifest = fixture.load().map_err(ctx("trusted manifest loads"))?;
        let resolved = fixture.resolve_all()?;
        let refs: Vec<&ResolvedBpfObject> = resolved.iter().collect();
        let verified = verify_against(&manifest, &refs).map_err(ctx("matching objects verify"))?;
        assert_eq!(verified, 4);
        Ok(())
    }

    #[test]
    fn test_uppercase_digests_are_accepted() -> TestResult {
        let mut text = header(1, 1, "bpfel");
        let bytes = object_bytes(BpfObjectKind::Exec);
        text.push_str(&contract_entry(
            BpfObjectKind::Exec,
            &sha256_hex(&bytes).to_ascii_uppercase(),
        ));
        let manifest = BpfManifest::parse(&text).map_err(ctx("uppercase digest parses"))?;
        manifest
            .verify(BpfObjectKind::Exec, &bytes)
            .map_err(ctx("uppercase digest matches"))?;
        Ok(())
    }

    #[test]
    fn test_modified_object_is_refused_with_digest_mismatch() -> TestResult {
        let fixture = Fixture::new(&manifest_for(&KNOWN_OBJECTS))?;
        // Das Objekt nach dem Schreiben des Manifests austauschen: gleiche
        // ELF-Kennung, gleiche Länge, ein anderes Byte.
        let mut tampered = object_bytes(BpfObjectKind::TcpV4Connect);
        let last = tampered
            .last_mut()
            .ok_or(TestError::Missing("fixture object bytes"))?;
        *last = last.wrapping_add(1);
        std::fs::write(fixture.path("bpf/tcp_v4_connect.bpf.o"), &tampered)
            .map_err(ctx("tamper with object"))?;

        let manifest = fixture.load().map_err(ctx("trusted manifest loads"))?;
        let resolved = fixture.resolve_all()?;
        let refs: Vec<&ResolvedBpfObject> = resolved.iter().collect();
        let err = expect_refusal(
            verify_against(&manifest, &refs),
            Some(BpfObjectKind::TcpV4Connect),
            IntegrityFailure::DigestMismatch,
        )?;

        // Die Meldung nennt das Objekt, aber weder Pfad noch Digest.
        let text = err.to_string();
        assert!(text.contains("tcp_v4_connect.bpf.o"));
        assert!(!text.contains('/'));
        assert!(!text.contains(&fixture.anchor().display().to_string()));
        assert!(!text.contains(&sha256_hex(&tampered)));
        assert!(!text.contains(&sha256_hex(&object_bytes(BpfObjectKind::TcpV4Connect))));
        Ok(())
    }

    #[test]
    fn test_object_without_manifest_entry_is_refused() -> TestResult {
        let fixture = Fixture::new(&manifest_for(&[
            BpfObjectKind::Exit,
            BpfObjectKind::TcpV4Connect,
            BpfObjectKind::TcpV6Connect,
        ]))?;
        let manifest = fixture.load().map_err(ctx("partial manifest loads"))?;
        let resolved = fixture.resolve_all()?;

        // Ohne `exec.bpf.o` passen die übrigen Objekte weiterhin.
        let others: Vec<&ResolvedBpfObject> = resolved
            .iter()
            .filter(|object| object.object != BpfObjectKind::Exec)
            .collect();
        assert_eq!(
            verify_against(&manifest, &others).map_err(ctx("listed objects verify"))?,
            3
        );

        let refs: Vec<&ResolvedBpfObject> = resolved.iter().collect();
        expect_refusal(
            verify_against(&manifest, &refs),
            Some(BpfObjectKind::Exec),
            IntegrityFailure::EntryMissing,
        )?;
        Ok(())
    }

    #[test]
    fn test_group_or_world_writable_manifest_is_untrusted() -> TestResult {
        for mode in [0o666, 0o664, 0o646, 0o662, 0o620, 0o602] {
            let fixture = Fixture::new(&manifest_for(&KNOWN_OBJECTS))?;
            chmod(&fixture.path(MANIFEST), mode)?;
            expect_load_failure(fixture.load(), IntegrityFailure::ManifestUntrusted)?;
        }
        Ok(())
    }

    #[test]
    fn test_owner_only_writable_manifest_is_trusted() -> TestResult {
        for mode in [0o644, 0o600, 0o640, 0o444, 0o400] {
            let fixture = Fixture::new(&manifest_for(&KNOWN_OBJECTS))?;
            chmod(&fixture.path(MANIFEST), mode)?;
            fixture
                .load()
                .map_err(|failure| TestError::Unexpected(format!("mode {mode:o}: {failure}")))?;
        }
        Ok(())
    }

    #[test]
    fn test_manifest_of_another_owner_is_untrusted() -> TestResult {
        let fixture = Fixture::new(&manifest_for(&KNOWN_OBJECTS))?;
        let stranger = fixture
            .owner
            .checked_add(1)
            .ok_or(TestError::Missing("a second uid"))?;
        expect_load_failure(
            load_manifest_below(fixture.anchor(), Path::new(MANIFEST), stranger),
            IntegrityFailure::ManifestUntrusted,
        )
    }

    #[test]
    fn test_writable_manifest_directory_is_untrusted() -> TestResult {
        for mode in [0o777, 0o775, 0o757] {
            let fixture = Fixture::new(&manifest_for(&KNOWN_OBJECTS))?;
            chmod(&fixture.path("bpf"), mode)?;
            expect_load_failure(fixture.load(), IntegrityFailure::ManifestUntrusted)?;
        }
        Ok(())
    }

    #[test]
    fn test_symlinked_manifest_is_untrusted() -> TestResult {
        let fixture = Fixture::new(&manifest_for(&KNOWN_OBJECTS))?;
        let real = fixture.path("bpf/real-manifest.toml");
        std::fs::rename(fixture.path(MANIFEST), &real).map_err(ctx("move manifest"))?;
        std::os::unix::fs::symlink(&real, fixture.path(MANIFEST))
            .map_err(ctx("symlink manifest"))?;
        expect_load_failure(fixture.load(), IntegrityFailure::ManifestUntrusted)
    }

    #[test]
    fn test_symlinked_manifest_directory_is_untrusted() -> TestResult {
        let fixture = Fixture::new(&manifest_for(&KNOWN_OBJECTS))?;
        std::os::unix::fs::symlink(fixture.path("bpf"), fixture.path("linked"))
            .map_err(ctx("symlink directory"))?;
        expect_load_failure(
            load_manifest_below(
                fixture.anchor(),
                Path::new("linked/manifest.json"),
                fixture.owner,
            ),
            IntegrityFailure::ManifestUntrusted,
        )
    }

    #[test]
    fn test_directory_in_place_of_the_manifest_is_untrusted() -> TestResult {
        let fixture = Fixture::new(&manifest_for(&KNOWN_OBJECTS))?;
        std::fs::remove_file(fixture.path(MANIFEST)).map_err(ctx("remove manifest"))?;
        std::fs::create_dir(fixture.path(MANIFEST)).map_err(ctx("directory as manifest"))?;
        chmod(&fixture.path(MANIFEST), 0o755)?;
        expect_load_failure(fixture.load(), IntegrityFailure::ManifestUntrusted)
    }

    #[test]
    fn test_missing_manifest_is_unreadable() -> TestResult {
        let fixture = Fixture::new(&manifest_for(&KNOWN_OBJECTS))?;
        std::fs::remove_file(fixture.path(MANIFEST)).map_err(ctx("remove manifest"))?;
        expect_load_failure(
            fixture.load(),
            IntegrityFailure::ManifestUnreadable(ErrorKind::NotFound),
        )
    }

    #[test]
    fn test_oversized_manifest_is_too_large() -> TestResult {
        let mut text = manifest_for(&KNOWN_OBJECTS);
        let padding = usize::try_from(MAX_MANIFEST_BYTES).map_err(ctx("limit fits usize"))?;
        text.push('#');
        text.push_str(&"x".repeat(padding));
        let fixture = Fixture::new(&text)?;
        expect_load_failure(fixture.load(), IntegrityFailure::ManifestTooLarge)
    }

    #[test]
    fn test_non_utf8_manifest_is_malformed() -> TestResult {
        let fixture = Fixture::new("")?;
        std::fs::write(fixture.path(MANIFEST), [0xff_u8, 0xfe, 0xfd])
            .map_err(ctx("write non-UTF-8 manifest"))?;
        expect_load_failure(fixture.load(), IntegrityFailure::ManifestMalformed)
    }

    #[test]
    fn test_manifest_path_must_be_absolute_and_plain() -> TestResult {
        for path in [
            "relative/manifest.json",
            "/",
            "/usr/../manifest.json",
            "manifest.json",
            // `.`-Segmente vorn, innen und am Ende, die `Path::components`
            // sonst stillschweigend entfernte.
            "/./usr/local/lib/harw-dod/bpf/manifest.json",
            "/usr/local/lib/harw-dod/bpf/./manifest.json",
            "/usr/local/lib/harw-dod/bpf/manifest.json/.",
            "./manifest.json",
        ] {
            expect_load_failure(
                load_manifest(Path::new(path)),
                IntegrityFailure::ManifestPathInvalid,
            )
            .map_err(|err| TestError::Unexpected(format!("{path}: {err}")))?;
        }
        Ok(())
    }

    #[test]
    fn test_dot_segment_below_the_anchor_is_rejected_although_the_file_exists() -> TestResult {
        let fixture = Fixture::new(&manifest_for(&KNOWN_OBJECTS))?;
        // Die Datei existiert und wäre vertrauenswürdig; abgelehnt wird allein
        // die Pfadform.
        fixture.load().map_err(ctx("plain path loads"))?;
        for relative in [
            "bpf/./manifest.json",
            "./bpf/manifest.json",
            "bpf/manifest.json/.",
        ] {
            expect_load_failure(
                load_manifest_below(fixture.anchor(), Path::new(relative), fixture.owner),
                IntegrityFailure::ManifestPathInvalid,
            )
            .map_err(|err| TestError::Unexpected(format!("{relative}: {err}")))?;
        }
        Ok(())
    }

    #[test]
    fn test_dot_segment_detection_is_exact() {
        for path in ["/a/./b", "/./a", "/a/.", ".", "./a", "a/./b"] {
            assert!(has_dot_segment(Path::new(path)), "{path}");
        }
        // `..`, versteckte Namen und leere Segmente sind kein `.`-Segment.
        for path in ["/a/../b", "/a/.hidden", "/a/b.", "/a//b", "/a/b/", "/", "a"] {
            assert!(!has_dot_segment(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn test_production_owner_is_root() {
        assert_eq!(MANIFEST_OWNER_UID, 0);
    }

    #[test]
    fn test_unsupported_schema_abi_or_target_is_rejected() -> TestResult {
        let entries = contract_entry(
            BpfObjectKind::Exec,
            &sha256_hex(&object_bytes(BpfObjectKind::Exec)),
        );
        for head in [
            header(2, 1, "bpfel"),
            header(1, 2, "bpfel"),
            header(1, 1, "bpfeb"),
        ] {
            expect_parse_failure(
                &format!("{head}{entries}"),
                IntegrityFailure::ManifestUnsupported,
            )?;
        }
        // Ein späteres Schema mit neuem Schlüssel ist „nicht unterstützt",
        // nicht „fehlerhaft".
        expect_parse_failure(
            &format!("{}future_key = 1\n{entries}", header(2, 1, "bpfel")),
            IntegrityFailure::ManifestUnsupported,
        )
    }

    #[test]
    fn test_malformed_manifests_are_rejected() -> TestResult {
        let exec_hash = sha256_hex(&object_bytes(BpfObjectKind::Exec));
        let exec = contract_entry(BpfObjectKind::Exec, &exec_hash);
        let head = header(1, 1, "bpfel");
        let (exec_program, exec_attach) = contract_names(BpfObjectKind::Exec);
        let cases = [
            ("not toml", "this is = = not toml".to_owned()),
            (
                "no schema version",
                head.replacen("schema_version = 1\n", "", 1),
            ),
            ("no objects", head.clone()),
            ("empty objects", format!("{head}objects = []\n")),
            ("unknown top-level key", format!("extra = 1\n{head}{exec}")),
            (
                "missing provenance",
                format!("{}{exec}", head.replacen("host_arch = \"x86_64\"\n", "", 1)),
            ),
            (
                "empty provenance",
                format!("{}{exec}", head.replacen("\"x86_64\"", "\"\"", 1)),
            ),
            (
                "bad btf digest",
                format!(
                    "{}{exec}",
                    head.replacen(&sha256_hex(b"fixture vmlinux btf"), "abc", 1)
                ),
            ),
            ("duplicate object", format!("{head}{exec}{exec}")),
            (
                "unknown object",
                format!(
                    "{head}{}",
                    entry("evil.bpf.o", exec_program, exec_attach, &exec_hash, MAPS)
                ),
            ),
            (
                "program mismatch",
                format!(
                    "{head}{}",
                    entry("exec.bpf.o", "dod_other", exec_attach, &exec_hash, MAPS)
                ),
            ),
            (
                "attach mismatch",
                format!(
                    "{head}{}",
                    entry(
                        "exec.bpf.o",
                        exec_program,
                        "sched:sched_process_exit",
                        &exec_hash,
                        MAPS
                    )
                ),
            ),
            (
                "map set mismatch",
                format!(
                    "{head}{}",
                    entry(
                        "exec.bpf.o",
                        exec_program,
                        exec_attach,
                        &exec_hash,
                        "[\"EVENTS\", \"EVENTS\", \"LOSS_COUNTS\", \"SEQUENCE\"]"
                    )
                ),
            ),
            (
                "extra map",
                format!(
                    "{head}{}",
                    entry(
                        "exec.bpf.o",
                        exec_program,
                        exec_attach,
                        &exec_hash,
                        "[\"EVENTS\", \"SCOPE_CGROUP_IDS\", \"LOSS_COUNTS\", \"SEQUENCE\", \"EXTRA\"]"
                    )
                ),
            ),
            (
                "short digest",
                format!("{head}{}", contract_entry(BpfObjectKind::Exec, "abc123")),
            ),
            (
                "long digest",
                format!(
                    "{head}{}",
                    contract_entry(BpfObjectKind::Exec, &format!("{exec_hash}00"))
                ),
            ),
            (
                "non-hex digest",
                format!(
                    "{head}{}",
                    contract_entry(
                        BpfObjectKind::Exec,
                        &format!(
                            "zz{}",
                            exec_hash
                                .get(2..)
                                .ok_or(TestError::Missing("digest tail"))?
                        )
                    )
                ),
            ),
            (
                "unknown object key",
                format!("{head}{exec}extra = \"value\"\n"),
            ),
        ];
        for (case, text) in cases {
            expect_parse_failure(&text, IntegrityFailure::ManifestMalformed)
                .map_err(|err| TestError::Unexpected(format!("{case}: {err}")))?;
        }
        Ok(())
    }

    #[test]
    fn test_reordered_map_set_is_accepted() -> TestResult {
        let exec_hash = sha256_hex(&object_bytes(BpfObjectKind::Exec));
        let (program, attach) = contract_names(BpfObjectKind::Exec);
        let text = format!(
            "{}{}",
            header(1, 1, "bpfel"),
            entry(
                "exec.bpf.o",
                program,
                attach,
                &exec_hash,
                "[\"SEQUENCE\", \"LOSS_COUNTS\", \"SCOPE_CGROUP_IDS\", \"EVENTS\"]"
            )
        );
        BpfManifest::parse(&text).map_err(ctx("reordered maps parse"))?;
        Ok(())
    }

    /// Die echte Ausgabe von `harw-dod/bpf/scripts/write-manifest.sh` für die
    /// festen Platzhaltereingaben aus
    /// `harw-dod/bpf/scripts/check-manifest-writer.sh` (`host_arch` auf
    /// `x86_64` normiert). Neu erzeugen mit `check-manifest-writer.sh --update`.
    const WRITER_SAMPLE: &str = include_str!("testdata/manifest.sample.toml");

    /// Die Platzhalter-Objektbytes, aus denen `check-manifest-writer.sh` die
    /// Probe erzeugt.
    fn writer_fixture_bytes(object: BpfObjectKind) -> Vec<u8> {
        format!("harw-dod manifest writer fixture: {}\n", object.file_name()).into_bytes()
    }

    /// Die Platzhalter-BTF-Bytes von `check-manifest-writer.sh`.
    const WRITER_FIXTURE_BTF: &[u8] = b"harw-dod manifest writer fixture: vmlinux BTF\n";

    #[test]
    fn test_manifest_writer_output_parses_and_binds_every_object() -> TestResult {
        let manifest =
            BpfManifest::parse(WRITER_SAMPLE).map_err(ctx("the writer's own output parses"))?;
        assert_eq!(manifest.entries.len(), KNOWN_OBJECTS.len());
        for object in KNOWN_OBJECTS {
            manifest
                .verify(object, &writer_fixture_bytes(object))
                .map_err(|failure| TestError::Unexpected(format!("{object}: {failure}")))?;
            // Die Bytes eines anderen Objekts passen nicht.
            let other = if object == BpfObjectKind::Exec {
                BpfObjectKind::Exit
            } else {
                BpfObjectKind::Exec
            };
            assert_eq!(
                manifest.verify(object, &writer_fixture_bytes(other)),
                Err(IntegrityFailure::DigestMismatch)
            );
        }
        Ok(())
    }

    #[test]
    fn test_manifest_writer_output_has_one_line_per_value() {
        // Der BTF-Digest steht als 64 Hexziffern in einer Zeile, so wie
        // `sha256sum` ihn für die Platzhalter-BTF ausgibt.
        let btf_line = format!(
            "vmlinux_btf_sha256 = \"{}\"\n",
            sha256_hex(WRITER_FIXTURE_BTF)
        );
        assert!(WRITER_SAMPLE.contains(&btf_line), "{btf_line}");
        for line in WRITER_SAMPLE.lines() {
            assert!(
                line.is_empty() || line == "[[objects]]" || line.contains(" = "),
                "{line:?}"
            );
        }
    }

    #[test]
    fn test_manifest_with_a_newline_inside_the_btf_digest_is_malformed() -> TestResult {
        // Die Form, die `write-manifest.sh` vor PL-100 Phase 4 schrieb: der
        // Zeilenumbruch von `sha256sum` stand im TOML-String.
        let btf = sha256_hex(WRITER_FIXTURE_BTF);
        let good = format!("vmlinux_btf_sha256 = \"{btf}\"\n");
        let broken = format!("vmlinux_btf_sha256 = \"{btf}\n\"\n");
        if !WRITER_SAMPLE.contains(&good) {
            return Err(TestError::Missing("btf digest line in the writer sample"));
        }
        expect_parse_failure(
            &WRITER_SAMPLE.replacen(&good, &broken, 1),
            IntegrityFailure::ManifestMalformed,
        )
    }

    #[test]
    fn test_contract_names_match_the_manifest_writer() {
        // Dieselben Tripel wie `harw-dod/bpf/scripts/write-manifest.sh`.
        assert_eq!(
            contract_names(BpfObjectKind::Exec),
            ("dod_sched_process_exec", "sched:sched_process_exec")
        );
        assert_eq!(
            contract_names(BpfObjectKind::Exit),
            ("dod_sched_process_exit", "sched:sched_process_exit")
        );
        assert_eq!(
            contract_names(BpfObjectKind::TcpV4Connect),
            ("dod_tcp_v4_connect", "tcp_v4_connect")
        );
        assert_eq!(
            contract_names(BpfObjectKind::TcpV6Connect),
            ("dod_tcp_v6_connect", "tcp_v6_connect")
        );
    }

    #[test]
    fn test_no_resolved_object_needs_no_manifest() -> TestResult {
        assert_eq!(
            verify_resolved_objects(None, &[]).map_err(ctx("nothing to verify"))?,
            0
        );
        Ok(())
    }

    #[test]
    fn test_resolved_objects_without_configured_manifest_are_refused() -> TestResult {
        let object = ResolvedBpfObject {
            object: BpfObjectKind::Exec,
            path: PathBuf::from("/usr/local/lib/harw-dod/bpf/exec.bpf.o"),
            source: BpfProgramSource::Embedded(Cow::Owned(object_bytes(BpfObjectKind::Exec))),
        };
        expect_refusal(
            verify_resolved_objects(None, &[&object]),
            None,
            IntegrityFailure::ManifestNotConfigured,
        )?;
        Ok(())
    }

    #[test]
    fn test_untrusted_manifest_path_is_refused_without_object() -> TestResult {
        let object = ResolvedBpfObject {
            object: BpfObjectKind::Exit,
            path: PathBuf::from("/usr/local/lib/harw-dod/bpf/exit.bpf.o"),
            source: BpfProgramSource::Embedded(Cow::Owned(object_bytes(BpfObjectKind::Exit))),
        };
        expect_refusal(
            verify_resolved_objects(Some(Path::new("relative/manifest.json")), &[&object]),
            None,
            IntegrityFailure::ManifestPathInvalid,
        )?;
        Ok(())
    }

    #[test]
    fn test_object_not_read_into_memory_is_refused() -> TestResult {
        let manifest = BpfManifest::parse(&manifest_for(&KNOWN_OBJECTS))
            .map_err(ctx("valid manifest parses"))?;
        let object = ResolvedBpfObject {
            object: BpfObjectKind::Exec,
            path: PathBuf::from("/usr/local/lib/harw-dod/bpf/exec.bpf.o"),
            source: BpfProgramSource::Path(PathBuf::from("/usr/local/lib/harw-dod/bpf/exec.bpf.o")),
        };
        expect_refusal(
            verify_against(&manifest, &[&object]),
            Some(BpfObjectKind::Exec),
            IntegrityFailure::SourceNotEmbedded,
        )?;
        Ok(())
    }

    #[test]
    fn test_failure_messages_carry_no_path() {
        for failure in [
            IntegrityFailure::ManifestNotConfigured,
            IntegrityFailure::ManifestPathInvalid,
            IntegrityFailure::ManifestUnreadable(ErrorKind::PermissionDenied),
            IntegrityFailure::ManifestUntrusted,
            IntegrityFailure::ManifestTooLarge,
            IntegrityFailure::ManifestMalformed,
            IntegrityFailure::ManifestUnsupported,
            IntegrityFailure::EntryMissing,
            IntegrityFailure::DigestMismatch,
            IntegrityFailure::SourceNotEmbedded,
        ] {
            let text = failure.to_string();
            assert!(!text.is_empty());
            assert!(!text.contains('/'), "{text}");
        }
    }
}
