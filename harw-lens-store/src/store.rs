//! Kernimplementierung von [`LensStore`]: Ablage, Auffinden und
//! Nachprüfung von Chunks und Indizes.
//!
//! # Verantwortungsbereich
//! Dieses Modul besitzt die gesamte Dateisystem-Mechanik des Crates:
//! Fanout-Pfadbau für Chunks, Namensvalidierung für Indizes,
//! Symlink-Abwehr, atomares Schreiben über `tempfile` + `persist` und die
//! `fs4`-Advisory-Lock um schreibende Operationen. Vorbild ist
//! `harw-session-store/src/child_lease.rs` (Lock/Unlock,
//! `is_regular_file`/`path_exists`, `persist`-Reihenfolge) und dessen
//! `store.rs` (`sync_parent_directory`). Das Ablageformat selbst ist im
//! `//!`-Block von `crate` (`lib.rs`) dokumentiert.
//!
//! # Nebenläufigkeit
//! Siehe den `# Nebenläufigkeit`-Abschnitt in `crate`s Modul-Dokumentation:
//! schreibende Methoden ([`LensStore::put_chunk`], [`LensStore::put_index`])
//! sperren nicht-blockierend, lesende Methoden sperren nicht.
//!
//! # Fehler
//! Alle Methoden geben [`crate::LensStoreResult`] zurück; siehe
//! [`crate::LensStoreError`] für die vollständige Variantenliste.
//!
//! # Examples
//! Siehe den `# Examples`-Abschnitt in `crate`s Modul-Dokumentation.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs4::FileExt;
use harw_lens_types::{Chunk, ChunkDigest, IndexManifest};
use harw_types::ContentDigest;
use tempfile::NamedTempFile;

use crate::error::{LensStoreError, LensStoreResult};

/// Fanout-Länge in Hexzeichen (zwei Hexzeichen = ein Byte = 256
/// Unterverzeichnisse). Siehe `# Warum Fanout über zwei Hexzeichen` in
/// `crate`s Modul-Dokumentation.
const CHUNK_FANOUT_LEN: usize = 2;
/// Name des Chunk-Wurzelverzeichnisses unterhalb des Store-Root.
const CHUNKS_DIR_NAME: &str = "chunks";
/// Name des Index-Wurzelverzeichnisses unterhalb des Store-Root.
const INDEX_DIR_NAME: &str = "index";
/// Dateiname des Index-Manifests innerhalb eines Indexverzeichnisses.
const MANIFEST_FILE_NAME: &str = "manifest.json";
/// Dateiname der Index-Rohdaten innerhalb eines Indexverzeichnisses.
const DATA_FILE_NAME: &str = "data.bin";
/// Dateiname der Advisory-Lock-Datei am Store-Root.
const LOCK_FILE_NAME: &str = ".lock";

/// Der inhaltsadressierte Chunk- und Indexspeicher.
///
/// # Description
/// Hält ausschließlich den aufgelösten Root-Pfad (`<home>/lens_store`,
/// siehe [`harw_home::paths::lens_store_dir`]). Legt weder in
/// [`LensStore::open`] noch sonst irgendwo eager Verzeichnisse an — jede
/// schreibende Methode legt ihr eigenes Zielverzeichnis unmittelbar vor dem
/// Schreiben an (Konvention aus `harw-session-store`s `ChildLeaseStore`:
/// der Konstruktor ist billig, das Anlegen passiert lazy beim ersten
/// Schreibzugriff).
#[derive(Debug, Clone)]
pub struct LensStore {
    root: PathBuf,
}

impl LensStore {
    /// Öffnet den Lens-Store unterhalb von `home`.
    ///
    /// # Description
    /// Löst den Root-Pfad über [`harw_home::paths::lens_store_dir`] auf.
    /// Legt kein Verzeichnis an — existiert der Pfad noch nicht, merkt sich
    /// diese Methode ihn nur; das erste Schreiben legt ihn an. Existiert am
    /// Pfad bereits eine reguläre Datei statt eines Verzeichnisses, ist das
    /// ein Fehler: eine spätere schreibende Operation würde sonst mit einer
    /// verwirrenden Betriebssystem-Fehlermeldung scheitern statt mit einer
    /// benannten Ursache.
    ///
    /// # Arguments
    /// - `home` (`&Path`): der Root-Space (`harw_home::paths::home_dir()`
    ///   oder ein Test-Tempdir), unterhalb dessen `lens_store/` liegt.
    ///
    /// # Returns
    /// Ein `LensStore`-Handle auf `<home>/lens_store`.
    ///
    /// # Errors
    /// - [`LensStoreError::UnexpectedPathType`] — `<home>/lens_store`
    ///   existiert bereits, ist aber kein Verzeichnis (z. B. eine Datei).
    ///
    /// # Concurrency
    /// Tut kein schreibendes I/O; sicher aus jedem Thread und beliebig oft
    /// parallel aufrufbar.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_store::LensStore;
    ///
    /// let temp = tempfile::tempdir().expect("tempdir");
    /// let store = LensStore::open(temp.path()).expect("opens");
    /// // Noch nichts geschrieben: das Store-Verzeichnis existiert noch nicht.
    /// assert!(!store.root().exists());
    /// ```
    pub fn open(home: &Path) -> LensStoreResult<Self> {
        let root = harw_home::paths::lens_store_dir(home);
        if root.exists() && !root.is_dir() {
            return Err(LensStoreError::UnexpectedPathType {
                path: root.display().to_string(),
            });
        }
        Ok(Self { root })
    }

    /// Der Root-Pfad dieses Stores (`<home>/lens_store`).
    ///
    /// # Returns
    /// Den bei [`LensStore::open`] aufgelösten Pfad, unverändert.
    ///
    /// # Concurrency
    /// Reine Feldabfrage; sicher aus jedem Thread aufrufbar.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Legt einen Chunk unter seinem Digest ab.
    ///
    /// # Description
    /// Idempotent: existiert am Zielpfad bereits eine reguläre Datei, kehrt
    /// diese Methode ohne erneutes Schreiben zurück — derselbe Chunk
    /// zweimal abgelegt erzeugt genau eine Datei, kein Fehler. Andernfalls
    /// schreibt sie über `tempfile::NamedTempFile` im selben
    /// Fanout-Verzeichnis, ruft `sync_all` (entspricht `fsync`/`sync_data`)
    /// auf der temporären Datei, benennt sie über `persist_noclobber`
    /// atomar auf den Zielpfad um (nie direktes Schreiben an den Zielort:
    /// ein abgebrochener Schreibvorgang hinterlässt keine halbe Datei, die
    /// ein Leser für vollständig hält) und synct danach das
    /// Fanout-Verzeichnis selbst — ohne diesen zweiten `fsync` ist der
    /// Rename nach einem Stromausfall nicht garantiert sichtbar. Ein
    /// `AlreadyExists` aus `persist_noclobber` (ein anderer Schreiber war
    /// schneller) ist kein Fehler: beide Schreiber legen unter demselben
    /// Digest content-adressiert identischen Inhalt ab.
    ///
    /// # Arguments
    /// - `chunk` (`&Chunk`): der abzulegende Chunk. Der Zielpfad entsteht
    ///   aus `chunk.digest`, nicht aus einer hier neu berechneten
    ///   Prüfsumme — die Korrektheit von `chunk.digest` relativ zu
    ///   `chunk.text` ist Sache des Aufrufers (`harw-lens-chunk`); dieser
    ///   Store prüft sie beim Schreiben nicht, sondern erst beim Lesen
    ///   (siehe [`LensStore::get_chunk`]).
    ///
    /// # Returns
    /// `chunk.digest`, unverändert zurückgegeben als Bequemlichkeit für den
    /// Aufrufer.
    ///
    /// # Errors
    /// - [`LensStoreError::UnexpectedPathType`]: der Zielpfad ist bereits
    ///   von einer Nicht-Datei belegt (Symlink-Abwehr).
    /// - [`LensStoreError::Io`] / [`LensStoreError::Serde`]: I/O- bzw.
    ///   Serialisierungsfehler beim Schreiben.
    /// - [`LensStoreError::LockContended`]: die Advisory-Lock-Datei ist
    ///   durch einen anderen Prozess belegt.
    ///
    /// # Concurrency
    /// Sperrt die `fs4`-Advisory-Lock-Datei am Store-Root
    /// nicht-blockierend für die Dauer des Schreibvorgangs.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_lens_store::LensStore;
    /// use harw_lens_types::{ByteSpan, Chunk, ChunkDigest, SourceRef};
    /// use harw_types::ContentDigest;
    ///
    /// let temp = tempfile::tempdir().expect("tempdir");
    /// let store = LensStore::open(temp.path()).expect("opens");
    /// let text = "hallo".to_owned();
    /// let chunk = Chunk {
    ///     digest: ChunkDigest(ContentDigest::of(text.as_bytes())),
    ///     source: SourceRef::File { path: "a.txt".to_owned() },
    ///     span: ByteSpan::new(0, text.len()).expect("valid span"),
    ///     text,
    /// };
    /// let digest = store.put_chunk(&chunk).expect("stores");
    /// assert_eq!(digest, chunk.digest);
    /// // Zweites Schreiben desselben Inhalts ist kein Fehler.
    /// assert!(store.put_chunk(&chunk).is_ok());
    /// ```
    pub fn put_chunk(&self, chunk: &Chunk) -> LensStoreResult<ChunkDigest> {
        let (dir, path) = self.chunk_paths(&chunk.digest);
        std::fs::create_dir_all(&dir)?;
        let lock = self.lock()?;
        let result = (|| {
            if is_regular_file(&path)? {
                return Ok(chunk.digest);
            }
            if path_exists(&path)? {
                return Err(LensStoreError::UnexpectedPathType {
                    path: path.display().to_string(),
                });
            }
            let mut temp = NamedTempFile::new_in(&dir)?;
            serde_json::to_writer(temp.as_file_mut(), chunk)?;
            temp.as_file().sync_all()?;
            match temp.persist_noclobber(&path) {
                Ok(_file) => {}
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Wettlauf: ein anderer Schreiber hat denselben Digest
                    // inzwischen bereits abgelegt. Gleicher Digest heißt
                    // gleicher Inhalt (content-addressed) — kein Fehler.
                }
                Err(error) => return Err(LensStoreError::Io(error.error)),
            }
            sync_parent_directory(&dir)?;
            Ok(chunk.digest)
        })();
        unlock(lock, result)
    }

    /// Prüft, ob ein Chunk unter `digest` abgelegt ist.
    ///
    /// # Description
    /// Prüft nur Existenz und Dateityp am Fanout-Pfad, liest den Inhalt
    /// nicht und prüft daher auch keinen Digest nach — dafür ist
    /// [`LensStore::get_chunk`] da.
    ///
    /// # Arguments
    /// - `digest` (`&ChunkDigest`): der gesuchte Digest.
    ///
    /// # Returns
    /// `true`, wenn am Fanout-Pfad eine reguläre Datei liegt, sonst
    /// `false` (auch dann, wenn dort ein Symlink oder eine andere
    /// Nicht-Datei liegt — das ist Sache von [`LensStore::get_chunk`],
    /// nicht dieser Existenzprüfung).
    ///
    /// # Errors
    /// - [`LensStoreError::Io`]: die Existenzprüfung selbst schlägt fehl
    ///   (z. B. ein Berechtigungsfehler auf einem Elternverzeichnis).
    ///
    /// # Concurrency
    /// Sperrt nicht; sicher gleichzeitig mit jeder anderen Methode
    /// aufrufbar.
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_store::LensStore;
    /// use harw_lens_types::ChunkDigest;
    /// use harw_types::ContentDigest;
    ///
    /// let temp = tempfile::tempdir().expect("tempdir");
    /// let store = LensStore::open(temp.path()).expect("opens");
    /// let digest = ChunkDigest(ContentDigest::of(b"unbekannt"));
    /// assert!(!store.has_chunk(&digest).expect("checks"));
    /// ```
    pub fn has_chunk(&self, digest: &ChunkDigest) -> LensStoreResult<bool> {
        let (_, path) = self.chunk_paths(digest);
        is_regular_file(&path)
    }

    /// Liest einen Chunk und prüft seinen Digest nach.
    ///
    /// # Description
    /// Die wichtigste Zusage dieses Crates: liest die Datei am Fanout-Pfad,
    /// deserialisiert sie zu [`Chunk`] und berechnet
    /// `ContentDigest::of(chunk.text.as_bytes())` über den tatsächlich
    /// gelesenen Text neu. Stimmt dieser Wert nicht mit dem angefragten
    /// `digest` überein, ist das ein Fehler — nie ein stilles Zurückgeben
    /// des (möglicherweise verfälschten) Inhalts. Vor dem Lesen wird
    /// geprüft, dass der Pfad eine reguläre Datei ist, damit ein an dieser
    /// Stelle abgelegter Symlink nicht befolgt wird.
    ///
    /// # Arguments
    /// - `digest` (`&ChunkDigest`): der angefragte Digest.
    ///
    /// # Returns
    /// `Some(Chunk)`, wenn ein Chunk unter `digest` abgelegt ist und seinen
    /// Digest bestätigt; `None`, wenn am Fanout-Pfad nichts liegt — ein
    /// fehlender Chunk ist ein normaler Zustand, kein Fehlerfall.
    ///
    /// # Errors
    /// - [`LensStoreError::UnexpectedPathType`]: der Pfad ist von einer
    ///   Nicht-Datei belegt (Symlink-Abwehr).
    /// - [`LensStoreError::Serde`]: die Datei ist kein valides
    ///   `Chunk`-JSON.
    /// - [`LensStoreError::ChunkDigestMismatch`]: der gelesene Inhalt
    ///   hasht nicht auf `digest`.
    /// - [`LensStoreError::Io`]: sonstiger Lesefehler.
    ///
    /// # Concurrency
    /// Sperrt nicht. Sicher gleichzeitig mit [`LensStore::put_chunk`],
    /// weil jeder Schreibvorgang über ein atomares Rename läuft: diese
    /// Methode sieht entweder die alte oder die neue Datei vollständig.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_lens_store::LensStore;
    /// use harw_lens_types::{ByteSpan, Chunk, ChunkDigest, SourceRef};
    /// use harw_types::ContentDigest;
    ///
    /// let temp = tempfile::tempdir().expect("tempdir");
    /// let store = LensStore::open(temp.path()).expect("opens");
    /// let text = "hallo".to_owned();
    /// let chunk = Chunk {
    ///     digest: ChunkDigest(ContentDigest::of(text.as_bytes())),
    ///     source: SourceRef::File { path: "a.txt".to_owned() },
    ///     span: ByteSpan::new(0, text.len()).expect("valid span"),
    ///     text,
    /// };
    /// let digest = store.put_chunk(&chunk).expect("stores");
    /// assert_eq!(store.get_chunk(&digest).expect("reads"), Some(chunk));
    /// ```
    pub fn get_chunk(&self, digest: &ChunkDigest) -> LensStoreResult<Option<Chunk>> {
        let (_, path) = self.chunk_paths(digest);
        if !path_exists(&path)? {
            return Ok(None);
        }
        if !is_regular_file(&path)? {
            return Err(LensStoreError::UnexpectedPathType {
                path: path.display().to_string(),
            });
        }
        let bytes = std::fs::read(&path)?;
        let chunk: Chunk = serde_json::from_slice(&bytes)?;
        let actual = ContentDigest::of(chunk.text.as_bytes());
        if actual != digest.0 {
            return Err(LensStoreError::ChunkDigestMismatch {
                requested: digest.0,
                actual,
            });
        }
        Ok(Some(chunk))
    }

    /// Legt Manifest und Rohdaten eines benannten Index ab.
    ///
    /// # Description
    /// Validiert `name` zuerst ([`validate_index_name`]) — ein Name aus
    /// einer Konfigurationsdatei darf nicht aus dem Store hinausführen.
    /// Schreibt `data.bin` **vor** `manifest.json`: ein Leser, der ein
    /// neues Manifest sieht, sieht damit garantiert auch schon die
    /// zugehörigen Rohdaten — nie ein neues Manifest neben veralteten oder
    /// fehlenden Daten. Beide Dateien werden einzeln über
    /// `tempfile::NamedTempFile` + `persist` atomar ausgetauscht; anders
    /// als bei Chunks erlaubt `persist` (nicht `persist_noclobber`) ein
    /// Überschreiben — ein Index ist nicht inhaltsadressiert und wird beim
    /// Neu-Indizieren bewusst ersetzt. Am Ende wird das Indexverzeichnis
    /// einmal gesynct, was beide vorangegangenen Renames durchsetzt.
    ///
    /// # Arguments
    /// - `name` (`&str`): der Indexname; siehe [`validate_index_name`] für
    ///   die erlaubte Zeichenmenge.
    /// - `manifest` (`&IndexManifest`): das abzulegende Manifest.
    /// - `data` (`&[u8]`): die Rohdaten des Index (Format ist Sache von
    ///   `harw-lens-index`; dieser Store interpretiert sie nicht).
    ///
    /// # Returns
    /// `Ok(())` nach durablem Schreiben beider Dateien.
    ///
    /// # Errors
    /// - [`LensStoreError::InvalidIndexName`]: `name` verstößt gegen die
    ///   Namensregel.
    /// - [`LensStoreError::Io`] / [`LensStoreError::Serde`]: I/O- bzw.
    ///   Serialisierungsfehler beim Schreiben.
    /// - [`LensStoreError::LockContended`]: die Advisory-Lock-Datei ist
    ///   durch einen anderen Prozess belegt.
    ///
    /// # Concurrency
    /// Sperrt die `fs4`-Advisory-Lock-Datei am Store-Root
    /// nicht-blockierend für die Dauer beider Schreibvorgänge.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_lens_store::LensStore;
    /// use harw_lens_types::{IndexManifest, Locality, Metric};
    /// use harw_types::ContentDigest;
    ///
    /// let temp = tempfile::tempdir().expect("tempdir");
    /// let store = LensStore::open(temp.path()).expect("opens");
    /// let manifest = IndexManifest {
    ///     model: "text-embed-3".to_owned(),
    ///     locality: Locality::Local,
    ///     chunker_version: 1,
    ///     visibility: "workspace".to_owned(),
    ///     metric: Metric::Cosine,
    ///     source_set_digest: ContentDigest::of(b"sources"),
    /// };
    /// store
    ///     .put_index("my-index", &manifest, b"raw-index-bytes")
    ///     .expect("stores");
    /// assert_eq!(
    ///     store.get_index_manifest("my-index").expect("reads"),
    ///     Some(manifest)
    /// );
    /// ```
    pub fn put_index(
        &self,
        name: &str,
        manifest: &IndexManifest,
        data: &[u8],
    ) -> LensStoreResult<()> {
        let dir = self.index_dir(name)?;
        std::fs::create_dir_all(&dir)?;
        let manifest_path = dir.join(MANIFEST_FILE_NAME);
        let data_path = dir.join(DATA_FILE_NAME);
        let lock = self.lock()?;
        let result = (|| {
            let mut data_temp = NamedTempFile::new_in(&dir)?;
            data_temp.write_all(data)?;
            data_temp.as_file().sync_all()?;
            data_temp
                .persist(&data_path)
                .map_err(|error| LensStoreError::Io(error.error))?;

            let mut manifest_temp = NamedTempFile::new_in(&dir)?;
            serde_json::to_writer(manifest_temp.as_file_mut(), manifest)?;
            manifest_temp.as_file().sync_all()?;
            manifest_temp
                .persist(&manifest_path)
                .map_err(|error| LensStoreError::Io(error.error))?;

            sync_parent_directory(&dir)?;
            Ok(())
        })();
        unlock(lock, result)
    }

    /// Liest das Manifest eines benannten Index.
    ///
    /// # Arguments
    /// - `name` (`&str`): der Indexname; siehe [`validate_index_name`].
    ///
    /// # Returns
    /// `Some(IndexManifest)`, wenn ein Manifest unter `name` abgelegt ist;
    /// `None`, wenn nicht — ein fehlender Index ist ein normaler Zustand.
    ///
    /// # Errors
    /// - [`LensStoreError::InvalidIndexName`]: `name` verstößt gegen die
    ///   Namensregel.
    /// - [`LensStoreError::UnexpectedPathType`]: der Pfad ist von einer
    ///   Nicht-Datei belegt (Symlink-Abwehr).
    /// - [`LensStoreError::Serde`]: die Datei ist kein valides
    ///   `IndexManifest`-JSON.
    /// - [`LensStoreError::Io`]: sonstiger Lesefehler.
    ///
    /// # Concurrency
    /// Sperrt nicht.
    ///
    /// # Examples
    /// Siehe [`LensStore::put_index`].
    pub fn get_index_manifest(&self, name: &str) -> LensStoreResult<Option<IndexManifest>> {
        let path = self.index_dir(name)?.join(MANIFEST_FILE_NAME);
        if !path_exists(&path)? {
            return Ok(None);
        }
        if !is_regular_file(&path)? {
            return Err(LensStoreError::UnexpectedPathType {
                path: path.display().to_string(),
            });
        }
        let bytes = std::fs::read(&path)?;
        Ok(Some(serde_json::from_slice(&bytes)?))
    }

    /// Modifikationszeitpunkt des Manifests eines benannten Index
    /// (Dateisystem-`mtime` von `manifest.json`).
    ///
    /// # Description
    /// [`harw_lens_types::IndexManifest`] trägt selbst kein `built_at`-Feld
    /// (siehe dessen Moduldokumentation). Ein Aufrufer, der eine grobe
    /// Auskunft über das Alter eines Index braucht (z. B. `harw lens
    /// status`), bekommt sie deshalb hier über die Dateisystem-Zeitmarke von
    /// `manifest.json` — diese Datei wird bei jedem [`LensStore::put_index`]
    /// atomar neu geschrieben (siehe dessen Dokumentation), ihre `mtime`
    /// entspricht also exakt dem letzten erfolgreichen Bauzeitpunkt dieses
    /// Index. Das ist ein Signal, kein Ersatz für eine im Manifest
    /// mitgeführte Zeitmarke — es weicht ab, sollte je jemand außerhalb
    /// dieses Crates die Datei berühren, ohne den Inhalt zu ändern.
    ///
    /// # Arguments
    /// - `name` (`&str`): der Indexname; siehe [`validate_index_name`].
    ///
    /// # Returns
    /// `Some(SystemTime)`, wenn ein Manifest unter `name` existiert; `None`,
    /// wenn nicht — ein fehlender Index ist ein normaler Zustand, wie bei
    /// [`LensStore::get_index_manifest`].
    ///
    /// # Errors
    /// - [`LensStoreError::InvalidIndexName`]: `name` verstößt gegen die
    ///   Namensregel.
    /// - [`LensStoreError::UnexpectedPathType`]: der Pfad ist von einer
    ///   Nicht-Datei belegt (Symlink-Abwehr).
    /// - [`LensStoreError::Io`]: sonstiger Lesefehler, einschließlich eines
    ///   Dateisystems, das keine Modifikationszeit unterstützt.
    ///
    /// # Concurrency
    /// Sperrt nicht, wie [`LensStore::get_index_manifest`].
    ///
    /// # Examples
    /// ```rust
    /// use harw_lens_store::LensStore;
    /// use harw_lens_types::{IndexManifest, Locality, Metric};
    /// use harw_types::ContentDigest;
    ///
    /// let temp = tempfile::tempdir().expect("tempdir");
    /// let store = LensStore::open(temp.path()).expect("opens");
    /// assert_eq!(
    ///     store.index_manifest_modified("missing").expect("no error for a missing index"),
    ///     None
    /// );
    ///
    /// let manifest = IndexManifest {
    ///     model: "m".to_owned(),
    ///     locality: Locality::Local,
    ///     chunker_version: 1,
    ///     visibility: "workspace".to_owned(),
    ///     metric: Metric::Cosine,
    ///     source_set_digest: ContentDigest::of(b"s"),
    /// };
    /// store.put_index("my-index", &manifest, b"raw").expect("stores");
    /// assert!(store.index_manifest_modified("my-index").expect("reads").is_some());
    /// ```
    pub fn index_manifest_modified(&self, name: &str) -> LensStoreResult<Option<std::time::SystemTime>> {
        let path = self.index_dir(name)?.join(MANIFEST_FILE_NAME);
        if !path_exists(&path)? {
            return Ok(None);
        }
        if !is_regular_file(&path)? {
            return Err(LensStoreError::UnexpectedPathType {
                path: path.display().to_string(),
            });
        }
        let metadata = std::fs::metadata(&path)?;
        Ok(Some(metadata.modified()?))
    }

    /// Liest die Rohdaten eines benannten Index.
    ///
    /// # Arguments
    /// - `name` (`&str`): der Indexname; siehe [`validate_index_name`].
    ///
    /// # Returns
    /// `Some(Vec<u8>)` mit den unveränderten Rohdaten, wenn abgelegt;
    /// `None`, wenn nicht.
    ///
    /// # Errors
    /// - [`LensStoreError::InvalidIndexName`]: `name` verstößt gegen die
    ///   Namensregel.
    /// - [`LensStoreError::UnexpectedPathType`]: der Pfad ist von einer
    ///   Nicht-Datei belegt (Symlink-Abwehr).
    /// - [`LensStoreError::Io`]: sonstiger Lesefehler.
    ///
    /// # Concurrency
    /// Sperrt nicht.
    ///
    /// # Examples
    /// Siehe [`LensStore::put_index`].
    pub fn read_index_data(&self, name: &str) -> LensStoreResult<Option<Vec<u8>>> {
        let path = self.index_dir(name)?.join(DATA_FILE_NAME);
        if !path_exists(&path)? {
            return Ok(None);
        }
        if !is_regular_file(&path)? {
            return Err(LensStoreError::UnexpectedPathType {
                path: path.display().to_string(),
            });
        }
        Ok(Some(std::fs::read(&path)?))
    }

    /// Baut Fanout-Verzeichnis und Dateipfad eines Chunks aus seinem
    /// Digest.
    ///
    /// Privater Pfadbau, kein I/O. Der Fanout nutzt die ersten zwei
    /// Hexzeichen des Digests (siehe `# Ablageform` in `crate`s
    /// Modul-Dokumentation).
    fn chunk_paths(&self, digest: &ChunkDigest) -> (PathBuf, PathBuf) {
        let hex = digest.0.to_string();
        let (fanout, rest) = hex.split_at(CHUNK_FANOUT_LEN);
        let dir = self.root.join(CHUNKS_DIR_NAME).join(fanout);
        let path = dir.join(format!("{rest}.json"));
        (dir, path)
    }

    /// Baut das Verzeichnis eines benannten Index, nach Namensvalidierung.
    ///
    /// Privater Pfadbau, kein I/O außer der reinen Zeichenprüfung in
    /// [`validate_index_name`].
    fn index_dir(&self, name: &str) -> LensStoreResult<PathBuf> {
        validate_index_name(name)?;
        Ok(self.root.join(INDEX_DIR_NAME).join(name))
    }

    /// Erwirbt die Advisory-Lock-Datei am Store-Root nicht-blockierend.
    ///
    /// Vorbild: `harw-session-store/src/child_lease.rs`s `lock()`. Setzt
    /// voraus, dass `self.root` bereits existiert — jeder Aufrufer legt
    /// sein Zielverzeichnis unterhalb von `self.root` per
    /// `create_dir_all` an, bevor er sperrt.
    fn lock(&self) -> LensStoreResult<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join(LOCK_FILE_NAME))?;
        FileExt::try_lock(&file).map_err(|error| match error {
            fs4::TryLockError::WouldBlock => LensStoreError::LockContended,
            fs4::TryLockError::Error(error) => LensStoreError::Io(error),
        })?;
        Ok(file)
    }
}

/// Gibt die Lock-Datei frei und kombiniert einen Entsperrfehler mit dem
/// Ergebnis der gesperrten Operation, ohne einen Entsperrfehler
/// stillschweigend zu verschlucken.
///
/// Vorbild: `harw-session-store/src/child_lease.rs`s `unlock()`.
fn unlock<T>(lock: File, result: LensStoreResult<T>) -> LensStoreResult<T> {
    let unlock = FileExt::unlock(&lock).map_err(LensStoreError::Io);
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

/// Synct ein Verzeichnis nach einem oder mehreren Renames hinein, damit die
/// neuen Verzeichniseinträge einen Stromausfall garantiert überleben.
///
/// Vorbild: `harw-session-store/src/store.rs`s `sync_parent_directory`. Ein
/// `sync_data`/`sync_all` auf der Datei allein reicht nicht: es
/// garantiert nur, dass der Dateiinhalt persistent ist, nicht, dass der
/// neue Verzeichniseintrag (das Rename-Ziel) es ebenfalls ist.
fn sync_parent_directory(dir: &Path) -> LensStoreResult<()> {
    let directory = File::open(dir)?;
    directory.sync_all()?;
    Ok(())
}

/// Prüft, ob an `path` überhaupt etwas liegt (Datei, Verzeichnis oder
/// Symlink), ohne einem Symlink zu folgen.
///
/// Vorbild: `harw-session-store/src/child_lease.rs`s `path_exists`.
fn path_exists(path: &Path) -> LensStoreResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(LensStoreError::Io(error)),
    }
}

/// Prüft, ob an `path` eine reguläre Datei liegt, ohne einem Symlink zu
/// folgen (Symlink-Abwehr).
///
/// Vorbild: `harw-session-store/src/child_lease.rs`s `is_regular_file`.
fn is_regular_file(path: &Path) -> LensStoreResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(LensStoreError::Io(error)),
    }
}

/// Validiert einen Indexnamen: nur `[A-Za-z0-9_.-]`, nicht leer, kein `..`
/// als Teilzeichenkette.
///
/// # Description
/// Ein Indexname kommt aus Konfiguration und darf niemals als Pfadsegment
/// aus dem Store hinausführen. Die erlaubte Zeichenmenge schließt `/` und
/// `\` bereits aus; die zusätzliche `..`-Prüfung fängt auch scheinbar
/// harmlose Namen wie `a..b` ab, die auf manchen Dateisystemen als
/// Traversal-Trick missbraucht werden könnten.
fn validate_index_name(name: &str) -> LensStoreResult<()> {
    let chars_ok = !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
    if chars_ok && !name.contains("..") {
        Ok(())
    } else {
        Err(LensStoreError::InvalidIndexName {
            name: name.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_lens_types::{ByteSpan, Locality, Metric, SourceRef};

    fn sample_chunk(text: &str) -> Chunk {
        Chunk {
            digest: ChunkDigest(ContentDigest::of(text.as_bytes())),
            source: SourceRef::File {
                path: "a.txt".to_owned(),
            },
            span: ByteSpan::new(0, text.len()).expect("valid span"),
            text: text.to_owned(),
        }
    }

    fn sample_manifest() -> IndexManifest {
        IndexManifest {
            model: "text-embed-3".to_owned(),
            locality: Locality::Local,
            chunker_version: 1,
            visibility: "workspace".to_owned(),
            metric: Metric::Cosine,
            source_set_digest: ContentDigest::of(b"sources"),
        }
    }

    #[test]
    fn test_put_chunk_then_get_chunk_returns_same_chunk() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LensStore::open(temp.path()).expect("opens");
        let chunk = sample_chunk("hallo welt");

        let digest = store.put_chunk(&chunk).expect("stores");
        let loaded = store.get_chunk(&digest).expect("reads");

        assert_eq!(loaded, Some(chunk));
    }

    #[test]
    fn test_put_chunk_twice_same_content_creates_one_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LensStore::open(temp.path()).expect("opens");
        let chunk = sample_chunk("wiederholter inhalt");

        let first = store.put_chunk(&chunk).expect("stores first");
        let second = store.put_chunk(&chunk).expect("stores second");
        assert_eq!(first, second);

        let (dir, _) = store.chunk_paths(&chunk.digest);
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .expect("reads fanout dir")
            .collect::<std::io::Result<Vec<_>>>()
            .expect("valid entries");
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_get_chunk_unknown_digest_returns_none() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LensStore::open(temp.path()).expect("opens");
        let digest = ChunkDigest(ContentDigest::of(b"nie geschrieben"));

        assert_eq!(store.get_chunk(&digest).expect("reads"), None);
    }

    #[test]
    fn test_has_chunk_reflects_presence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LensStore::open(temp.path()).expect("opens");
        let chunk = sample_chunk("vorhanden");

        assert!(!store.has_chunk(&chunk.digest).expect("checks"));
        store.put_chunk(&chunk).expect("stores");
        assert!(store.has_chunk(&chunk.digest).expect("checks"));
    }

    #[test]
    fn test_get_chunk_tampered_file_reports_digest_mismatch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LensStore::open(temp.path()).expect("opens");
        let chunk = sample_chunk("unverfälschter inhalt");
        let digest = store.put_chunk(&chunk).expect("stores");

        let (_, path) = store.chunk_paths(&digest);
        let mut tampered = chunk.clone();
        tampered.text = "verfälschter inhalt".to_owned();
        std::fs::write(&path, serde_json::to_vec(&tampered).expect("serializes"))
            .expect("overwrites on disk");

        let result = store.get_chunk(&digest);
        assert!(matches!(
            result,
            Err(LensStoreError::ChunkDigestMismatch { .. })
        ));
    }

    #[test]
    fn test_put_index_get_manifest_and_read_data_roundtrip() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LensStore::open(temp.path()).expect("opens");
        let manifest = sample_manifest();
        let data = b"raw-index-bytes".to_vec();

        store
            .put_index("my-index", &manifest, &data)
            .expect("stores");

        assert_eq!(
            store.get_index_manifest("my-index").expect("reads"),
            Some(manifest)
        );
        assert_eq!(
            store.read_index_data("my-index").expect("reads"),
            Some(data)
        );
    }

    #[test]
    fn test_get_index_manifest_unknown_name_returns_none() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LensStore::open(temp.path()).expect("opens");

        assert_eq!(store.get_index_manifest("never-written").unwrap(), None);
        assert_eq!(store.read_index_data("never-written").unwrap(), None);
    }

    #[test]
    fn test_index_manifest_modified_unknown_name_returns_none() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LensStore::open(temp.path()).expect("opens");

        assert_eq!(
            store.index_manifest_modified("never-written").unwrap(),
            None
        );
    }

    #[test]
    fn test_index_manifest_modified_returns_some_after_put_index() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LensStore::open(temp.path()).expect("opens");
        let manifest = sample_manifest();

        store
            .put_index("my-index", &manifest, b"data")
            .expect("stores");

        assert!(
            store
                .index_manifest_modified("my-index")
                .expect("reads")
                .is_some()
        );
    }

    #[test]
    fn test_index_manifest_modified_rejects_path_separator_in_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LensStore::open(temp.path()).expect("opens");

        let result = store.index_manifest_modified("a/b");
        assert!(matches!(
            result,
            Err(LensStoreError::InvalidIndexName { .. })
        ));
    }

    #[test]
    fn test_put_index_rejects_path_separator_in_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LensStore::open(temp.path()).expect("opens");
        let manifest = sample_manifest();

        let result = store.put_index("a/b", &manifest, b"data");
        assert!(matches!(
            result,
            Err(LensStoreError::InvalidIndexName { .. })
        ));
    }

    #[test]
    fn test_put_index_rejects_dot_dot_traversal_in_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LensStore::open(temp.path()).expect("opens");
        let manifest = sample_manifest();

        let result = store.put_index("../escape", &manifest, b"data");
        assert!(matches!(
            result,
            Err(LensStoreError::InvalidIndexName { .. })
        ));
    }

    #[test]
    fn test_put_chunk_leaves_no_tmp_file_in_fanout_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = LensStore::open(temp.path()).expect("opens");
        let chunk = sample_chunk("aufgeräumter persist-pfad");

        store.put_chunk(&chunk).expect("stores");

        let (dir, path) = store.chunk_paths(&chunk.digest);
        let entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("reads fanout dir")
            .map(|entry| entry.expect("valid entry").path())
            .collect();

        assert_eq!(entries, vec![path]);
    }

    #[test]
    fn test_open_rejects_lens_store_path_occupied_by_a_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("lens_store"), b"not a directory")
            .expect("writes blocking file");

        let result = LensStore::open(temp.path());
        assert!(matches!(
            result,
            Err(LensStoreError::UnexpectedPathType { .. })
        ));
    }
}
