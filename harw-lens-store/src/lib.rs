//! Inhaltsadressierter Chunk- und Indexspeicher für den Harwness-Lens-Layer.
//!
//! # Verantwortungsbereich
//! [`LensStore`] besitzt die gesamte Persistenzmechanik für zwei
//! Artefakttypen: [`harw_lens_types::Chunk`] (inhaltsadressiert über
//! [`harw_lens_types::ChunkDigest`]) und benannte Indizes (Manifest +
//! Rohdaten, adressiert über einen Namen). Dieses Crate interpretiert weder
//! Chunk-Text noch Indexinhalt — das Zerlegen von Text in Chunks lebt in
//! `harw-lens-chunk`, das Bauen/Lesen eines tatsächlichen Vektorindex in
//! `harw-lens-index`. `harw-lens-store` ist reine Speichermechanik: ablegen,
//! wiederfinden, nachprüfen.
//!
//! # Ablageform
//! ```text
//! <home>/lens_store/
//!   chunks/<zwei-hex>/<rest-hex>.json     # Fanout über die ersten zwei Hexzeichen des Digests
//!   index/<name>/manifest.json
//!   index/<name>/data.bin
//!   .lock                                  # Advisory-Lock-Datei für schreibende Operationen
//! ```
//!
//! `<home>` ist der von [`harw_home::paths::lens_store_dir`] aufgelöste
//! Pfad; dieses Crate legt ihn beim ersten Schreiben an, [`LensStore::open`]
//! selbst erzeugt kein Verzeichnis (Konvention aus `harw-session-store`s
//! `ChildLeaseStore`: der Konstruktor ist billig, das Anlegen passiert lazy
//! beim ersten Schreibzugriff).
//!
//! ## Warum Fanout über zwei Hexzeichen
//! Ein Chunk-Digest ist ein Blake3-Hash und daher gleichverteilt: die
//! ersten zwei Hexzeichen streuen jeden Chunk gleichmäßig über 256
//! Unterverzeichnisse (`00` bis `ff`). Ein einzelner Workspace-Index kann
//! niedrige Hunderttausend Chunks erreichen; ein einzelnes Verzeichnis mit
//! hunderttausend Einträgen ist auf vielen Dateisystemen (insbesondere
//! solchen mit linearer Verzeichnissuche) spürbar langsam bei jedem Zugriff
//! — Erstellen, Lookup und Auflisten verlangsamen sich mit der
//! Verzeichnisgröße. Mit zweistelligem Fanout landen bei 200.000 Chunks im
//! Mittel unter 800 Einträge je Unterverzeichnis, ein Bereich, in dem
//! gängige Dateisysteme (ext4, APFS, NTFS) keine messbare Verzeichnisgröße
//! mehr spüren. Ein Indexname braucht diesen Fanout nicht: die Anzahl
//! gleichzeitig lebender Indizes liegt um Größenordnungen niedriger als die
//! Chunkzahl, deshalb liegt `index/<name>/` direkt unter `index/`.
//!
//! # Nebenläufigkeit
//! [`LensStore`] hält nur einen `PathBuf` (keine interne Veränderlichkeit)
//! und ist `Clone`; mehrere Handles auf denselben Root sind sicher, auch aus
//! verschiedenen Threads oder Prozessen heraus. Schreibende Operationen
//! ([`LensStore::put_chunk`], [`LensStore::put_index`]) sperren eine
//! `fs4`-Advisory-Lock-Datei am Store-Root nicht-blockierend (`try_lock`)
//! und liefern [`LensStoreError::LockContended`], statt zu warten. Lesende
//! Operationen ([`LensStore::get_chunk`], [`LensStore::has_chunk`],
//! [`LensStore::get_index_manifest`], [`LensStore::read_index_data`])
//! sperren nicht — sie lesen atomar ausgetauschte Dateien und sind mit
//! gleichzeitigen Schreibern sicher kombinierbar, weil jeder Schreibvorgang
//! über `tempfile` + `persist`/`persist_noclobber` läuft (atomares Rename):
//! ein Leser sieht entweder die alte oder die neue Datei vollständig, nie
//! einen halben Schreibvorgang.
//!
//! # Fehler
//! Alle Fehler dieses Crates sind in [`LensStoreError`] versammelt:
//! I/O-Fehler, JSON-Fehler, ungültige Indexnamen, unerwartete Pfadtypen
//! (Symlink-Abwehr) und — der wichtigste Fall — eine fehlgeschlagene
//! Digest-Nachprüfung beim Lesen eines Chunks.
//!
//! # Examples
//! ```rust,no_run
//! use harw_lens_store::LensStore;
//! use harw_lens_types::{ByteSpan, Chunk, ChunkDigest, SourceRef};
//! use harw_types::ContentDigest;
//! use std::path::Path;
//!
//! let store = LensStore::open(Path::new("/tmp/example-harw-home"))?;
//! let text = "hallo welt".to_owned();
//! let chunk = Chunk {
//!     digest: ChunkDigest(ContentDigest::of(text.as_bytes())),
//!     source: SourceRef::File { path: "a.txt".to_owned() },
//!     span: ByteSpan::new(0, text.len())?,
//!     text,
//! };
//! let digest = store.put_chunk(&chunk)?;
//! assert_eq!(store.get_chunk(&digest)?, Some(chunk));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod error;
mod store;

pub use error::{LensStoreError, LensStoreResult};
pub use store::LensStore;
