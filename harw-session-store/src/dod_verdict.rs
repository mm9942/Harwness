//! `VerdictStore`: die verkettete, segmentierte Ablage der Modell-Verdicts
//! über DoD-Befunde (PL-100 WP 2.3).
//!
//! # Zweck
//! Ein Triage-Kind gibt ein `harwness.security-verdict/v1` zurück.
//! `accept_security_verdict` (in `harw-core-bridge`) prüft, bindet und
//! bereinigt es und legt es hier ab. Der Store hält die **Meinung des
//! Modells** fest, mehr nicht:
//!
//! - Verdicts annotieren, sie entscheiden nie (Prinzip I3). Nichts in diesem
//!   Modul unterdrückt einen Hinweis oder autorisiert einen Eingriff.
//! - Jeder Datensatz trägt `model_authored = true`. Der Store setzt das Feld
//!   selbst; es gibt keinen Weg, über diese API einen Operator-Entscheid
//!   abzulegen (Prinzip I1, Bedrohung B5). Ein Datensatz mit `false` gilt beim
//!   Prüfen als Bruch der Kette.
//! - Die Begründung ist Modelltext und damit untrusted. Sie wird beim Anhängen
//!   mit [`sanitize_rationale`] auf höchstens [`MAX_RATIONALE_BYTES`] Bytes
//!   gekappt und über `harw_untrusted_text` escapt — idempotent, sodass ein
//!   Aufrufer, der schon bereinigt hat, denselben Text zurückbekommt.
//!
//! # Ablage
//! `<state_dir>/dod/verdicts/` (Verzeichnis `0700`), darin:
//! - `verdicts-<erste seq, 20 Stellen>.jsonl`: Segmente, je Zeile ein
//!   [`VerdictRecord`] als JSON, Dateirechte `0600`;
//! - `anchor.json` (`0600`): der Anker der Kette nach einer Retention
//!   (geschrieben über die Tempdatei `anchor.json.tmp`);
//! - `.lock` (`0600`): die `fs4`-Sperre für Schreiber. Sie gilt nur, solange
//!   der Name auf die gesperrte Inode zeigt (siehe „Nebenläufigkeit“).
//!
//! Ein neues Segment beginnt, sobald das aktive Segment mit dem nächsten
//! Datensatz [`VERDICT_SEGMENT_BYTES`] (4 MiB) überschreiten würde **oder**
//! sein erster Datensatz [`VERDICT_SEGMENT_SPAN`] (1 Tag) oder länger vor dem
//! neuen liegt (`created_at`). Die Rotation nach Alter hält jedes Segment
//! auf etwa einen Tag; so löscht die Retention auch auf Hosts mit wenigen
//! Verdicts (die die 4 MiB nie erreichen) im 90-Tage-Takt statt nie. Eine
//! Zeile ist höchstens [`MAX_VERDICT_RECORD_BYTES`] (8 KiB) lang.
//!
//! # Verzeichnisgrenze (Symlinks)
//! `state_dir` gibt der Aufrufer vor; es gilt als vertraut und darf selbst
//! ein Symlink sein (etwa `~/.harw` → `/data/harw`). Darunter öffnet jeder
//! öffentliche Aufruf den Store **Glied für Glied**, jeweils relativ zum
//! Deskriptor des Elternverzeichnisses: `dod` unter `state_dir`, `verdicts`
//! unter `dod`, beide mit `O_DIRECTORY | O_NOFOLLOW`
//! (`harw_fsutil::open_dir_at`; ein FIFO oder Gerät an ihrer Stelle wird gar
//! nicht erst geöffnet). `state_dir` selbst wird als `<state_dir>/.` geöffnet,
//! damit auch dort nur ein Verzeichnis in Frage kommt. Fehlt
//! ein Glied, legt nur [`VerdictStore::append`] es per `mkdirat` an; ein
//! vorhandener Eintrag, auch ein Symlink, wird dabei nie verfolgt, und am
//! Ziel eines Symlinks entsteht nichts. Jedes Glied muss ein Verzeichnis
//! sein, der effektiven uid gehören und darf weder für die Gruppe noch für
//! Andere beschreibbar sein, sonst [`VerdictStoreError::UnsafeDirectory`];
//! ein zu offenes Verzeichnis wird nicht stillschweigend repariert (nur das
//! Lese-/Durchsuchrecht von `verdicts` nimmt ein Schreiber per `fchmod` auf
//! `0700` zurück).
//!
//! Danach geschieht **alles** relativ zum Deskriptor von `verdicts`: Sperre,
//! Anker (lesen; schreiben über eine Tempdatei und `renameat`), Segmente
//! (öffnen, anlegen, anhängen, abschneiden), Auflisten (`readdir` auf einer
//! Kopie des Deskriptors, Typen per `fstatat` ohne Symlink-Folge), Löschen
//! in der Retention (`unlinkat`) und der Verzeichnis-`fsync`. Der Pfad
//! [`VerdictStore::root`] wird dafür nie wieder aufgelöst. Wird das geprüfte
//! Verzeichnis während eines Aufrufs umbenannt und durch einen Symlink oder
//! ein anderes Verzeichnis ersetzt, sperrt, schreibt und löscht der Aufruf
//! weiter im ursprünglichen Verzeichnis; der nächste Aufruf steigt neu ab
//! und verweigert einen Symlink.
//!
//! # Kette
//! Jeder Datensatz trägt `seq` (lückenlos, ab [`FIRST_VERDICT_SEQ`]),
//! `prev_hash` (der `record_hash` des Vorgängers) und `record_hash` (BLAKE3
//! über eine kanonische, längenpräfixierte Binärkodierung aller übrigen
//! Felder mit eigenem Domänen-String). Der erste `prev_hash` ist
//! [`genesis_prev_hash`], abgeleitet aus einem Domänen-String — nie Nullen.
//! [`VerdictStore::verify_chain`] rechnet jede Verknüpfung und jeden Hash nach
//! und erkennt so jede Änderung, Löschung oder Umordnung, die nicht den
//! gesamten Rest der Kette neu schreibt.
//!
//! Angehängt wird nur hinter eine Kette, die **als Ganzes** stimmt:
//! [`VerdictStore::append`] prüft unter der Schreibsperre die gesamte
//! lebende Kette vorwärts (Anker → jedes Segment → letzter Datensatz) mit
//! derselben Logik wie [`VerdictStore::verify_chain`] und leitet `seq` und
//! `prev_hash` des neuen Datensatzes ausschließlich aus dieser Prüfung ab.
//! Eine Verletzung an beliebiger Stelle (auch weit vor dem Ende, etwa ein neu
//! versiegelter Datensatz in einem älteren Segment, dessen Nachfolger dann
//! nicht mehr verknüpft) liefert denselben Fehler wie `verify_chain`; kein
//! Segment wird dann geändert, nichts angehängt. Die Prüfung läuft, bevor
//! irgendein Segment angefasst wird (kein `fchmod`, kein Abschneiden).
//!
//! Kosten: jedes Anhängen liest und hasht alle lebenden Segmente, also
//! O(Größe der lebenden Kette), dasselbe, was jeder Leser ohnehin bezahlt.
//! Die Größe begrenzt die Retention auf etwa 91 Tage plus höchstens ein
//! Segment (Rotation täglich bzw. bei 4 MiB), und zwar ohne eigenen Aufruf:
//! [`VerdictStore::append`] lässt sie bei jedem neuen Segment selbst
//! mitlaufen (siehe „Retention“). Wie groß die Kette werden darf, bevor
//! gleichzeitige Schreiber an der Sperre scheitern, steht unter
//! „Nebenläufigkeit“. Ein zwischengespeichertes,
//! „schon geprüftes“ Ende gibt es bewusst nicht: es müsste bei jeder
//! Änderung irgendeines Segments verworfen werden, und jeder Fehler in dieser
//! Invalidierung öffnete genau die Lücke, die die Vorwärtsprüfung schließt.
//!
//! # Wiederherstellung nach einem Kettenbruch
//! Eine Verletzung der Kette ([`VerdictStoreError::ChainBroken`],
//! [`VerdictStoreError::CorruptRecord`], [`VerdictStoreError::SegmentTooLarge`],
//! [`VerdictStoreError::AnchorCorrupt`]) heilt nicht von selbst: Leser
//! verweigern den Stand, [`VerdictStore::append`] verweigert jedes weitere
//! Verdict, und die Retention endet vor dem betroffenen Segment, entfernt es
//! also nie. Das ist Absicht (ein Bruch ist von einer Manipulation nicht zu
//! unterscheiden und bleibt sichtbar), heißt aber: bis jemand eingreift,
//! speichert der Host keine Verdicts mehr (`harw-core-bridge` gibt den
//! Fehler von `persist` an den Aufrufer weiter). Ursachen sind Manipulation,
//! ein Werkzeug, das Dateien im Store entfernt (siehe „Retention“), oder
//! selten ein Absturz, nach dem eine nie quittierte letzte Zeile zwar mit
//! `\n` endet, aber nicht lesbar ist (ungeordnetes Zurückschreiben von
//! Seiten).
//!
//! Wiederherstellen, ohne Beweise zu verlieren:
//! 1. Schreiber anhalten (oder hinnehmen, dass ihre Verdicts in dieser Zeit
//!    scheitern).
//! 2. `<state_dir>/dod/verdicts` als Ganzes beiseitelegen, etwa in
//!    `<state_dir>/dod/verdicts.broken-<Datum>` umbenennen, nicht löschen:
//!    dort lässt sich die Stelle des Bruchs (Segment und `seq` aus dem
//!    Fehler) später untersuchen.
//! 3. Das nächste [`VerdictStore::append`] legt `verdicts/` neu an und
//!    beginnt eine neue Kette ab Genesis (`seq` [`FIRST_VERDICT_SEQ`]).
//!
//! Ein [`VerdictRef::verdict_digest`] aus der alten Kette lässt sich danach
//! nur noch gegen die beiseitegelegte Kopie prüfen. Der Store selbst legt nie
//! etwas beiseite: das ist eine Operator-Entscheidung, kein automatischer
//! Weg, Beweise zu verwerfen.
//!
//! # Retention
//! [`VerdictStore::apply_retention`] löscht nur **ganze** Segmente vom Anfang
//! der Kette, deren jüngster Datensatz älter als [`VERDICT_RETENTION`]
//! (90 Tage) ist. Das gilt auch für das aktive (letzte) Segment: ist alles
//! abgelaufen, bleibt nur der Anker, und das nächste Anhängen beginnt ein
//! neues Segment mit `seq` und `prev_hash` aus dem Anker. Vorher prüft sie die
//! Kette; gelöscht und verankert wird nur innerhalb des **geprüften
//! Präfixes**. Findet die Prüfung eine Verletzung (Kettenbruch, unlesbare
//! Zeile, übergroßes Segment), endet die Retention vor dem betroffenen
//! Segment, meldet es in [`VerdictRetentionReport::fault_segment`] und auf
//! `warn`; die Verletzung selbst wird nie neu verankert und bleibt für
//! [`VerdictStore::verify_chain`] sichtbar. So hält eine einzelne kaputte
//! Zeile die 90-Tage-Frist für alles davor nicht auf. Den Anker
//! (`first_seq`, `prev_hash` = `record_hash` des letzten gelöschten
//! Datensatzes) schreibt sie atomar, **bevor** sie löscht. Danach prüft
//! [`VerdictStore::verify_chain`] die verbleibende Kette gegen den Anker. Ein
//! Absturz zwischen Anker und Löschen ist harmlos: Segmente vor dem Anker
//! gelten als „ausgemustert“, werden beim Prüfen übersprungen und vom
//! nächsten Lauf entfernt.
//!
//! **Automatisch beim Anhängen:** beginnt [`VerdictStore::append`] ein neues
//! Segment (Rotation nach Größe oder Zeitspanne, oder das erste Segment eines
//! leeren bzw. verankerten Stores), läuft nach dem quittierten Schreiben unter
//! derselben Sperre dieselbe Retention, mit `created_at` des neuen Datensatzes
//! als Jetzt-Zeit und auf der Prüfung, die das Anhängen ohnehin gemacht hat
//! (kein zweiter Durchlauf). In Frage kommen nur Segmente **vor dem bisher
//! aktiven** (dem, das den bisher letzten Datensatz hält), nie dieses und nie
//! das gerade beschriebene; das bisher aktive Segment geht erst beim nächsten
//! Segmentwechsel oder in einem expliziten Lauf. Grund: die Jetzt-Zeit stammt
//! hier vom Aufrufer, und ein einzelnes Verdict mit weit vorgestellter Uhr
//! (RTC-Fehler, NTP-Sprung) soll nicht den gesamten bisherigen Stand löschen;
//! es entfernt höchstens die Segmente vor dem jüngsten. Weil Segmente
//! höchstens etwa einen Tag umfassen, bleibt die lebende Kette so auf etwa 91
//! Tage plus dieses eine Segment begrenzt, solange angehängt wird, auch wenn
//! niemand [`VerdictStore::apply_retention`] aufruft. Scheitert diese
//! Retention, bleibt der Datensatz quittiert; der Fehler erscheint
//! inhaltsfrei auf `warn`, und der nächste Segmentwechsel (oder ein
//! expliziter Lauf) holt sie nach. [`VerdictStore::apply_retention`] bleibt
//! für explizite Läufe, etwa auf einem Host, der nicht mehr anhängt; sie
//! kennt keine solche Grenze.
//!
//! **Probelauf:** [`VerdictStore::plan_retention`] liefert den Bericht, den
//! [`VerdictStore::apply_retention`] zur selben Jetzt-Zeit liefern würde,
//! und ändert nichts (keine Sperre, kein Anlegen, kein `fchmod`, kein Anker,
//! kein Löschen). Beide rechnen die Auswahl mit derselben reinen Funktion
//! auf einem Durchlauf der Kette; auf einem unveränderten Store stimmen ihre
//! Berichte deshalb überein. Der Plan ist eine Momentaufnahme ohne Sperre:
//! ein Schreiber dazwischen kann den Stand eines späteren Laufs ändern.
//!
//! Nur die Retention des Stores ([`VerdictStore::apply_retention`] und die
//! beim Anhängen) darf Segmente, den Anker oder `.lock` entfernen;
//! [`VerdictStore::plan_retention`] entfernt nichts. Ein
//! Werkzeug, das das Verzeichnis dateiweise nach Alter oder Größe ausdünnt
//! (etwa ein generischer Retention-Sweep), löscht Segmente ohne neuen Anker
//! oder `anchor.json` selbst; das ist von einer Manipulation nicht zu
//! unterscheiden, und danach verweigern [`VerdictStore::verify_chain`] und
//! [`VerdictStore::append`] die Kette dauerhaft (siehe „Wiederherstellung nach
//! einem Kettenbruch“). Entfernt es `.lock` (deren mtime sich nie ändert),
//! können zwei Schreiber verschiedene Inodes sperren; davor schützt der Store
//! sich nur teilweise (siehe „Nebenläufigkeit“). Solche Werkzeuge müssen
//! `dod/verdicts` auslassen oder [`VerdictStore::apply_retention`] aufrufen.
//!
//! # Dauerhaftigkeit
//! Ein Anhängen schreibt die vollständige Zeile mit einem `write_all` auf
//! einen `O_APPEND`-Deskriptor und ruft danach `sync_data`; jedes vor dem
//! Schreiben leere Ziel-Segment (neu angelegt, oder ein leerer Rest eines
//! gescheiterten Anhängens, fortgesetzt oder als Rotationsziel
//! wiederverwendet) bekommt zusätzlich einen `fsync` des Verzeichnisses,
//! bevor das Anhängen quittiert wird. Legt der Store fehlende Glieder von
//! `state_dir` oder `dod/`, `dod/verdicts/`
//! neu an, synchronisiert er das Elternverzeichnis jedes neuen Glieds (für
//! `dod` und `verdicts` über den Deskriptor des Elternverzeichnisses), bevor
//! er schreibt. Den Anker schreibt
//! die Retention vollständig in `anchor.json.tmp` (`write_all`, `fsync`),
//! benennt ihn per `renameat` um und synchronisiert danach das Verzeichnis.
//! Eine durch einen Absturz abgerissene letzte Zeile wurde nie
//! quittiert: Leser überspringen sie, und das nächste Anhängen schneidet sie
//! unter der Sperre ab.
//!
//! # Nebenläufigkeit
//! [`VerdictStore`] hält keinen inneren Zustand (`Send + Sync`). Schreiber
//! ([`VerdictStore::append`], [`VerdictStore::apply_retention`]) nehmen die
//! exklusive `fs4`-Sperre `.lock`; ist sie belegt, versuchen sie es bis zu
//! etwa einer Sekunde lang erneut und melden dann
//! [`VerdictStoreError::LockContended`]. Die Sperre zählt nur, solange `.lock`
//! auf die gesperrte Inode zeigt: nach dem `flock` vergleicht der Schreiber
//! das per `fstatat` (ohne Symlink-Folge); wurde `.lock` inzwischen entfernt
//! oder ersetzt, öffnet und sperrt er die aktuelle Datei neu. Direkt vor
//! seiner ersten Änderung prüft er das noch einmal und bricht sonst mit
//! `LockContended` ab, ohne etwas geändert zu haben. Ganz ausschließen lässt
//! sich ein zweiter Schreiber nur, wenn niemand außer dieser API `.lock`
//! entfernt (siehe „Retention“).
//!
//! Sperr-Budget und Kettengröße: der Halter der Sperre prüft beim Anhängen
//! die ganze lebende Kette, ein zweiter Schreiber wartet also mindestens so
//! lange. Dauert die Prüfung länger als das Budget von etwa einer Sekunde,
//! bekommt er `LockContended`, und sein Verdict wird nicht gespeichert
//! (Verdicts sind Annotation, kein Audit; der Aufrufer darf es erneut
//! versuchen). Je Datensatz kostet die Prüfung eine JSON-Dekodierung, BLAKE3
//! über höchstens 8 KiB und die Escape-Prüfung der Begründung, also wenige
//! Mikrosekunden. Nach dieser groben, nicht gemessenen Schätzung reicht das
//! Budget für eine lebende Kette in der Größenordnung von 100 000 Datensätzen,
//! gut 1 000 Verdicts pro Tag über die 90 Tage der Retention. Ein einzelner
//! Schreiber ist davon nicht betroffen; nur gleichzeitige Schreiber auf einer
//! deutlich größeren Kette brauchten ein mit der Kette wachsendes Budget.
//!
//! Leser nehmen keine Sperre; eine halb geschriebene letzte Zeile wird
//! übersprungen. Leser **listen zuerst
//! die Segmente und lesen danach den Anker**. Weil die Retention den Anker
//! schreibt, bevor sie löscht, ist der gelesene Anker damit mindestens so
//! neu wie jede Löschung, die die Liste schon zeigt: ein Leser sieht nie
//! einen alten Anker zu einer bereits ausgedünnten Liste (das wäre ein
//! falscher Kettenbruch, also ein falscher Manipulationsalarm). Höchstens
//! stehen Segmente vor dem neuen Anker noch in der Liste (ausgemustert,
//! übersprungen), oder eine spätere Retention (auch die beim Segmentwechsel
//! in [`VerdictStore::append`]) löscht ein gelistetes Segment, bevor der Leser
//! es öffnet. Dann verwirft der Leser alles bisher Gesammelte und läuft die
//! Kette neu ab (neue Liste, neuer Anker), insgesamt höchstens dreimal; erst
//! danach meldet er [`VerdictStoreError::Io`] (`NotFound`). Alle Aufrufe sind
//! blockierende Datei-I/O und gehören in async-Kontexten in `spawn_blocking`.
//!
//! Jeder öffentliche Aufruf steigt neu bis `verdicts` ab (siehe
//! „Verzeichnisgrenze“) und hält dessen Deskriptor bis zu seiner Rückkehr;
//! der Handle selbst hält keinen Deskriptor. Ein Schreiber sperrt `.lock` in
//! genau dem Verzeichnis, in das er schreibt und aus dem er löscht.
//! Restrisiko: ersetzt jemand das Verzeichnis **zwischen** zwei Aufrufen
//! durch ein anderes echtes Verzeichnis, arbeiten Aufrufe davor und danach
//! auf verschiedenen Verzeichnissen (und Schreiber sperren nicht mehr
//! gegeneinander). Dafür braucht er Schreibrecht auf `dod/`, das der
//! eigenen uid gehört und für Gruppe und Andere nicht beschreibbar ist; das
//! liegt innerhalb der „Grenzen“ unten.
//!
//! # Fehler
//! [`VerdictStoreError`]; alle Meldungen sind inhaltsfrei (keine Begründung,
//! keine Pfade), damit sie auch auf `warn`-Ebene geloggt werden dürfen. Was
//! nach einer Verletzung der Kette zu tun ist, steht unter
//! „Wiederherstellung nach einem Kettenbruch“.
//!
//! # Grenzen
//! Die Kette hat keinen Schlüssel. Wer den Schreibbereich der harw-uid
//! beherrscht, kann den gesamten Rest einer Kette neu berechnen. Verdicts
//! gelten deshalb als Bequemlichkeit, nicht als Audit (THREAT-MODEL §1); wer
//! einen bestimmten Stand festhalten will, merkt sich den
//! [`VerdictRef::verdict_digest`] außerhalb dieses Stores.
//!
//! # Plattform
//! Nur Unix. Das Modul benutzt `std::os::unix` und `std::os::fd` ohne
//! `cfg`-Gate, weil die Crate ohnehin unbedingt von `harw-fsutil` abhängt, das
//! selbst Unix-only ist; ein Gate nur dieses Moduls machte die Crate nicht
//! auf anderen Plattformen baubar.
//!
//! # Examples
//! ```rust,no_run
//! use harw_session_store::dod_verdict::{
//!     NewVerdict, StoredClassification, StoredResponse, StoredSeverity, VerdictStore,
//! };
//! use harw_types::{ContentDigest, FindingId};
//!
//! # fn demo(state_dir: &std::path::Path, now: jiff::Timestamp)
//! #     -> harw_session_store::dod_verdict::VerdictStoreResult<()> {
//! let store = VerdictStore::new(state_dir);
//! let finding = FindingId::from_str("finding-1");
//! let verdict_ref = store.append(&NewVerdict {
//!     finding_id: finding.clone(),
//!     record_digest: ContentDigest::of(b"exported finding record"),
//!     classification: StoredClassification::Suspicious,
//!     severity: StoredSeverity::Medium,
//!     suggested_response: Some(StoredResponse::Monitor),
//!     confidence: None,
//!     rationale: "Verbindung zu einem unbekannten Ziel".to_owned(),
//!     model: Some("local/triage-model".to_owned()),
//!     created_at: now,
//! })?;
//! let summary = store.verify_chain()?;
//! assert_eq!(summary.last, Some(verdict_ref));
//! assert!(store.latest_for(&finding)?.is_some());
//! # Ok(())
//! # }
//! ```

use std::borrow::Cow;
use std::fmt;
use std::fs::{DirBuilder, File, Permissions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use fs4::FileExt;
use harw_fsutil::{
    EntryType, OpenMode, ensure_owned_directory, ensure_private_regular, entry_type_at,
    is_symlink_loop, mkdir_at, open_beneath, open_dir_at, read_dir_names, rename_at, same_file_at,
    unlink_at,
};
use harw_macros::HarwError;
use harw_types::{Confidence, ContentDigest, FindingId};
use harw_untrusted_text::{TRUNCATION_MARKER, escape_char, escape_line, is_clean, needs_escape};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

// Das eine Literal der Ketten-Domäne: `VERDICT_CHAIN_DOMAIN` und
// `GENESIS_DOMAIN` entstehen beide daraus und können nicht auseinanderlaufen.
macro_rules! verdict_chain_domain {
    () => {
        "harwness.dod.verdict-chain/v1"
    };
}

/// Domänen-String der Verdict-Kette; Grundlage von [`genesis_prev_hash`]
/// (der Genesis-Wert ist BLAKE3 über `"<VERDICT_CHAIN_DOMAIN>/genesis"`).
pub const VERDICT_CHAIN_DOMAIN: &str = verdict_chain_domain!();

/// Höchstlänge der gespeicherten Begründung in Bytes (2 KiB), Kürzungsmarke
/// und Ersatzformen eingerechnet.
pub const MAX_RATIONALE_BYTES: usize = 2 * 1024;

/// Höchstlänge einer kodierten JSONL-Zeile in Bytes (8 KiB), `\n`
/// eingerechnet.
pub const MAX_VERDICT_RECORD_BYTES: usize = 8 * 1024;

/// Größe, ab der ein neues Segment beginnt (4 MiB).
pub const VERDICT_SEGMENT_BYTES: u64 = 4 * 1024 * 1024;

/// Zeitspanne eines Segments (1 Tag): liegt der erste Datensatz des aktiven
/// Segments mindestens so lange vor dem neuen (`created_at`), beginnt ein
/// neues Segment.
///
/// # Description
/// Die Retention löscht nur ganze Segmente. Ohne Rotation nach Alter bliebe
/// auf einem Host mit wenigen Verdicts alles in einem Segment, dessen
/// jüngster Datensatz nie [`VERDICT_RETENTION`] alt wird. Mit dieser Spanne
/// ist kein gelöschter Datensatz jünger als 90 Tage und, nach einem
/// expliziten [`VerdictStore::apply_retention`], kein verbliebener älter als
/// etwa 91 Tage (bei monotoner Uhr). Die Retention beim Anhängen lässt
/// zusätzlich das bisher aktive Segment bis zum nächsten Segmentwechsel
/// stehen. Läuft die Uhr zurück, rotiert nur die Größe.
pub const VERDICT_SEGMENT_SPAN: SignedDuration = SignedDuration::from_secs(86_400);

/// Aufbewahrungsdauer: ganze Segmente, deren jüngster Datensatz älter ist,
/// entfernt [`VerdictStore::apply_retention`] und, beim Beginn jedes neuen
/// Segments und ohne das bisher aktive Segment, [`VerdictStore::append`]
/// selbst (90 Tage).
pub const VERDICT_RETENTION: SignedDuration = SignedDuration::from_secs(90 * 86_400);

/// Höchstlänge des Modellbezeichners in Zeichen (nach dem Escapen).
pub const MAX_MODEL_CHARS: usize = 128;

/// Höchstlänge einer `finding_id` in Bytes.
pub const MAX_FINDING_ID_BYTES: usize = 256;

/// Die erste Sequenznummer einer neuen Kette.
pub const FIRST_VERDICT_SEQ: u64 = 1;

// Eigener Domänen-String des Genesis-Werts (nie Nullen, nie ein echter
// Record-Hash), abgeleitet aus `VERDICT_CHAIN_DOMAIN`.
const GENESIS_DOMAIN: &[u8] = concat!(verdict_chain_domain!(), "/genesis").as_bytes();

// Domänen-String der kanonischen Kodierung, über die `record_hash` läuft.
const RECORD_HASH_DOMAIN: &[u8] = b"harwness.dod.verdict-record/v1";

// Rechte des Store-Verzeichnisses und aller Dateien darin.
const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

// Unterverzeichnisse unterhalb von `state_dir`.
const DOD_DIR: &str = "dod";
const VERDICTS_DIR: &str = "verdicts";

// Dateinamen innerhalb des Store-Verzeichnisses.
const LOCK_FILE: &str = ".lock";
const ANCHOR_FILE: &str = "anchor.json";
// Tempdatei für das atomare Schreiben von `anchor.json` (nur unter der Sperre).
const ANCHOR_TEMP_FILE: &str = "anchor.json.tmp";
const SEGMENT_PREFIX: &str = "verdicts-";
const SEGMENT_SUFFIX: &str = ".jsonl";
const SEGMENT_DIGITS: usize = 20;

// Formatversion von `anchor.json`.
const ANCHOR_VERSION: u32 = 1;

// Leselimit für `anchor.json`; die Datei ist wenige hundert Bytes groß.
const MAX_ANCHOR_BYTES: u64 = 4 * 1024;

// Leselimit eines Segments: Rotationsgrenze plus Spielraum für eine Zeile.
const MAX_SEGMENT_READ_BYTES: u64 = VERDICT_SEGMENT_BYTES.saturating_add(16 * 1024);

// Begrenztes Warten auf die Schreibsperre: 200 Versuche im Abstand von 5 ms
// (Budget gegen die Kettengröße: Moduldoku „Nebenläufigkeit“).
const LOCK_ATTEMPTS: u32 = 200;
const LOCK_BACKOFF: Duration = Duration::from_millis(5);

// Durchläufe eines Lesers, wenn ein gelistetes Segment vor dem Öffnen
// verschwindet (`retry_vanished`). Eine Retention läuft höchstens bei jedem
// Segmentwechsel, zwei Durchläufe reichen also praktisch immer.
const READ_ATTEMPTS: u32 = 3;

// Feste, inhaltsfreie Gründe für `UnsafeDirectory` (keine Pfade).
const NOT_A_REAL_DIRECTORY: &str = "store directory is not a real directory";
const NOT_A_PRIVATE_DIRECTORY: &str =
    "store directory is not owned by the current user or is writable by group or others";
const STATE_DIR_NOT_A_DIRECTORY: &str = "state directory is not a directory";

/// Fehler des [`VerdictStore`] (erzeugt `VerdictStoreResult<T>`).
///
/// # Description
/// Alle Meldungen sind inhaltsfrei: weder Begründung noch Modelltext noch
/// Pfade erscheinen darin.
#[derive(Debug, HarwError)]
pub enum VerdictStoreError {
    /// Dateisystemfehler; `Display`/`source()` kommen vom inneren Fehler.
    #[from]
    Io(std::io::Error),

    /// JSON-Kodierung eines Datensatzes oder des Ankers schlug fehl.
    #[from]
    Serde(serde_json::Error),

    /// Ein anderer Schreiber hielt die Sperre länger als etwa eine Sekunde.
    #[msg("verdict store lock is contended")]
    LockContended,

    /// Ein Glied unterhalb von `state_dir` (`dod`, `verdicts`) ist kein
    /// echtes Verzeichnis (z. B. ein Symlink), gehört nicht der effektiven
    /// uid oder ist für Gruppe/Andere beschreibbar; oder `.lock` ist ein
    /// Symlink; oder `state_dir` selbst ist kein Verzeichnis.
    #[msg("verdict store directory is unsafe: {detail}")]
    UnsafeDirectory {
        /// Fester, inhaltsfreier Grund.
        detail: &'static str,
    },

    /// Ein Segment-Eintrag ist keine reguläre Datei (z. B. ein Symlink).
    #[msg("verdict segment entry is not a regular file")]
    UnsafeSegment,

    /// Die `finding_id` ist leer, zu lang oder enthält Zeichen, die escapt
    /// werden müssten.
    #[msg("verdict finding id is blank, too long or contains characters that must be escaped")]
    FindingIdInvalid,

    /// Die kodierte Zeile überschreitet [`MAX_VERDICT_RECORD_BYTES`].
    #[msg("encoded verdict record has {size} bytes (limit {limit})")]
    RecordTooLarge {
        /// Länge der kodierten Zeile in Bytes.
        size: usize,
        /// Das Limit in Bytes.
        limit: usize,
    },

    /// Ein Segment ist größer, als dieser Store es je schreibt.
    #[msg("verdict segment has at least {size} bytes (read limit {limit})")]
    SegmentTooLarge {
        /// Gelesene Bytes (mindestens).
        size: u64,
        /// Das Leselimit in Bytes.
        limit: u64,
    },

    /// Die Sequenznummern sind erschöpft (`u64::MAX`).
    #[msg("verdict sequence number space is exhausted")]
    SequenceExhausted,

    /// Eine Zeile ließ sich nicht als [`VerdictRecord`] lesen.
    #[msg("corrupt verdict record in segment {segment_seq} at line {line}")]
    CorruptRecord {
        /// Erste Sequenznummer (Name) des Segments.
        segment_seq: u64,
        /// Zeilennummer ab 1; `0`, wenn die Stelle am Segmentende liegt.
        line: u64,
    },

    /// `anchor.json` ist nicht lesbar oder widersprüchlich.
    #[msg("verdict chain anchor is corrupt")]
    AnchorCorrupt,

    /// Die Kette ist an `seq` gebrochen.
    #[msg("verdict chain broken at seq {seq}: {reason}")]
    ChainBroken {
        /// Sequenznummer des Datensatzes, an dem die Prüfung scheiterte.
        seq: u64,
        /// Welche Prüfung scheiterte.
        reason: ChainBreak,
    },
}

/// Welche Prüfung der Kette gescheitert ist.
///
/// # Description
/// Teil von [`VerdictStoreError::ChainBroken`]. Inhaltsfrei.
///
/// # Examples
/// ```rust
/// use harw_session_store::dod_verdict::ChainBreak;
///
/// assert_eq!(ChainBreak::RecordHashMismatch.to_string(), "record hash mismatch");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainBreak {
    /// `seq` ist nicht die erwartete nächste Nummer (Lücke, Umordnung).
    SequenceMismatch {
        /// Die erwartete Sequenznummer.
        expected: u64,
    },
    /// `prev_hash` verweist nicht auf den Vorgänger bzw. den Anker.
    PrevHashMismatch,
    /// `record_hash` passt nicht zum Inhalt des Datensatzes.
    RecordHashMismatch,
    /// Der Datensatz ist nicht als vom Modell verfasst markiert.
    NotModelAuthored,
    /// Ein Feld verletzt die Invarianten des Stores (Länge, Escaping).
    InvariantViolated,
    /// Der Segmentname stimmt nicht mit dem ersten Datensatz überein.
    SegmentNameMismatch,
}

impl fmt::Display for ChainBreak {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SequenceMismatch { expected } => write!(f, "expected seq {expected}"),
            Self::PrevHashMismatch => f.write_str("prev hash does not link"),
            Self::RecordHashMismatch => f.write_str("record hash mismatch"),
            Self::NotModelAuthored => f.write_str("record is not model-authored"),
            Self::InvariantViolated => f.write_str("record violates store invariants"),
            Self::SegmentNameMismatch => {
                f.write_str("segment name does not match its first record")
            }
        }
    }
}

/// Einstufung eines Verdicts, wie sie im Store liegt.
///
/// # Description
/// Spiegelt `harw_dod_signals::VerdictClassification` (gleiche Varianten,
/// gleiche kebab-case-Form). `harw-session-store` (Schicht I) darf die
/// DoD-Crate (Schicht D) nicht kennen; die Abbildung macht der Aufrufer.
/// `Confirmed` ist die **Meinung des Modells**, nie ein Operator-Entscheid.
///
/// # Examples
/// ```rust
/// use harw_session_store::dod_verdict::StoredClassification;
///
/// assert_eq!(StoredClassification::Confirmed.as_str(), "confirmed");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoredClassification {
    /// Harmlos erklärt.
    Benign,
    /// Auffällig, nicht eindeutig.
    Suspicious,
    /// Vom Modell als Vorfall eingestuft (nur Annotation).
    Confirmed,
}

impl StoredClassification {
    /// Die feste Drahtform (`benign`, `suspicious`, `confirmed`).
    ///
    /// # Returns
    /// Die kebab-case-Bezeichnung; dieselbe Form wie in JSON.
    ///
    /// # Examples
    /// ```rust
    /// use harw_session_store::dod_verdict::StoredClassification;
    ///
    /// assert_eq!(StoredClassification::Benign.as_str(), "benign");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Benign => "benign",
            Self::Suspicious => "suspicious",
            Self::Confirmed => "confirmed",
        }
    }
}

/// Vorgeschlagene Reaktion eines Verdicts, wie sie im Store liegt.
///
/// # Description
/// Spiegelt `harw_dod_signals::SuggestedResponse`. Kein Auftrag und ohne
/// Ziel: eine Kategorie, die nichts auslöst (Prinzip I2).
///
/// # Examples
/// ```rust
/// use harw_session_store::dod_verdict::StoredResponse;
///
/// assert_eq!(StoredResponse::Contain.as_str(), "contain");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoredResponse {
    /// Weiter beobachten.
    Monitor,
    /// Eskalation vorschlagen.
    Escalate,
    /// Eindämmung vorschlagen.
    Contain,
}

impl StoredResponse {
    /// Die feste Drahtform (`monitor`, `escalate`, `contain`).
    ///
    /// # Returns
    /// Die kebab-case-Bezeichnung; dieselbe Form wie in JSON.
    ///
    /// # Examples
    /// ```rust
    /// use harw_session_store::dod_verdict::StoredResponse;
    ///
    /// assert_eq!(StoredResponse::Escalate.as_str(), "escalate");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Monitor => "monitor",
            Self::Escalate => "escalate",
            Self::Contain => "contain",
        }
    }
}

/// Gewicht der Einstufung eines Verdicts, wie es im Store liegt.
///
/// # Description
/// Spiegelt `harw_dod_signals::Severity` (gleiche Varianten, gleiche
/// kebab-case-Form); die Abbildung macht der Aufrufer, weil Schicht I die
/// DoD-Crate nicht kennen darf. Gespeichert, damit der I4-Rückkanal
/// `{finding_id, classification, severity, suggested_response, verdict_ref}`
/// und die Oberflächen (WP 2.4) aus einem Datensatz rekonstruierbar sind. Die
/// Einschätzung des Modells, nicht die des Sensors.
///
/// # Examples
/// ```rust
/// use harw_session_store::dod_verdict::StoredSeverity;
///
/// assert_eq!(StoredSeverity::Critical.as_str(), "critical");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoredSeverity {
    /// Rein informativ.
    Info,
    /// Geringes Gewicht.
    Low,
    /// Mittleres Gewicht.
    Medium,
    /// Hohes Gewicht.
    High,
    /// Kritisch.
    Critical,
}

impl StoredSeverity {
    /// Die feste Drahtform (`info`, `low`, `medium`, `high`, `critical`).
    ///
    /// # Returns
    /// Die kebab-case-Bezeichnung; dieselbe Form wie in JSON.
    ///
    /// # Examples
    /// ```rust
    /// use harw_session_store::dod_verdict::StoredSeverity;
    ///
    /// assert_eq!(StoredSeverity::Info.as_str(), "info");
    /// ```
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

/// Eingabe für [`VerdictStore::append`].
///
/// # Description
/// Die bereits geprüften und gebundenen Felder eines Verdicts. `seq`,
/// `prev_hash`, `record_hash` und `model_authored` vergibt der Store selbst.
///
/// - `rationale` darf roh oder schon mit [`sanitize_rationale`] bereinigt
///   sein; der Store bereinigt ohnehin (idempotent).
/// - `model` wird einzeilig escapt und auf [`MAX_MODEL_CHARS`] gekürzt; ein
///   leerer Bezeichner wird zu `None`.
/// - `finding_id` muss nicht leer, höchstens [`MAX_FINDING_ID_BYTES`] lang
///   und sauber im Sinne von `harw_untrusted_text::is_clean` sein.
/// - Das vom Modell behauptete `issued_at` des Verdicts wird bewusst nicht
///   gespeichert: es ist unverifiziert, und die maßgebliche Zeit ist
///   `created_at` aus der `Clock` des Aufrufers.
///
/// # Examples
/// ```rust
/// use harw_session_store::dod_verdict::{NewVerdict, StoredClassification, StoredSeverity};
/// use harw_types::{ContentDigest, FindingId};
///
/// let verdict = NewVerdict {
///     finding_id: FindingId::from_str("finding-1"),
///     record_digest: ContentDigest::of(b"record"),
///     classification: StoredClassification::Benign,
///     severity: StoredSeverity::Low,
///     suggested_response: None,
///     confidence: None,
///     rationale: "bekannter Dienst".to_owned(),
///     model: None,
///     created_at: jiff::Timestamp::UNIX_EPOCH,
/// };
/// assert_eq!(verdict.classification, StoredClassification::Benign);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewVerdict {
    /// Der Befund, an den das Verdict gebunden wurde.
    pub finding_id: FindingId,
    /// Der vom Aufrufer selbst ermittelte `record_digest` des exportierten
    /// Befunds (nie der vom Modell behauptete Wert).
    pub record_digest: ContentDigest,
    /// Einstufung des Modells.
    pub classification: StoredClassification,
    /// Gewicht der Einstufung laut Modell.
    pub severity: StoredSeverity,
    /// Vorgeschlagene Reaktion des Modells, falls angegeben.
    pub suggested_response: Option<StoredResponse>,
    /// Selbsteinschätzung des Modells; `None`, solange der Verdict-Vertrag
    /// `harwness.security-verdict/v1` kein Feld dafür hat.
    pub confidence: Option<Confidence>,
    /// Begründung des Modells (untrusted).
    pub rationale: String,
    /// Bezeichner des Modells, das das Verdict verfasst hat, falls bekannt.
    pub model: Option<String>,
    /// Zeitpunkt der Annahme (vom Aufrufer über seine `Clock` ermittelt, nie
    /// aus Modellausgabe). [`VerdictStore::append`] benutzt ihn auch als
    /// Jetzt-Zeit der Retention beim Segmentwechsel: ein Wert weit in der
    /// Zukunft ließe alles verfallen, was mehr als 90 Tage davor liegt.
    pub created_at: Timestamp,
}

/// Ein gespeicherter, verketteter Verdict-Datensatz.
///
/// # Description
/// Eine Zeile eines Segments. Die Felder sind privat und nur über Methoden
/// lesbar: ein Datensatz entsteht ausschließlich in
/// [`VerdictStore::append`] oder beim Lesen eines Segments. `rationale` ist
/// escapt und höchstens [`MAX_RATIONALE_BYTES`] lang; Oberflächen zeigen sie
/// eingeklappt und als „vom Modell verfasst“ an.
///
/// # Wire-Format
/// JSON mit `deny_unknown_fields`; Digests als 64-stelliges Hex,
/// `created_at` als RFC-3339-Zeitstempel.
///
/// # Examples
/// ```rust,no_run
/// use harw_session_store::dod_verdict::VerdictStore;
/// use harw_types::FindingId;
///
/// # fn demo(store: &VerdictStore) -> harw_session_store::dod_verdict::VerdictStoreResult<()> {
/// if let Some(record) = store.latest_for(&FindingId::from_str("finding-1"))? {
///     assert!(record.model_authored());
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerdictRecord {
    // Lückenlose Sequenznummer ab `FIRST_VERDICT_SEQ`.
    seq: u64,
    // `record_hash` des Vorgängers, Anker-Wert oder `genesis_prev_hash()`.
    prev_hash: ContentDigest,
    // BLAKE3 über die kanonische Kodierung aller übrigen Felder.
    record_hash: ContentDigest,
    // Gebundener Befund.
    finding_id: FindingId,
    // Gebundener `record_digest` des exportierten Befunds.
    record_digest: ContentDigest,
    // Einstufung des Modells.
    classification: StoredClassification,
    // Gewicht der Einstufung laut Modell.
    severity: StoredSeverity,
    // Vorgeschlagene Reaktion des Modells.
    suggested_response: Option<StoredResponse>,
    // Selbsteinschätzung des Modells.
    confidence: Option<Confidence>,
    // Escapte, gekappte Begründung.
    rationale: String,
    // Escapter Modellbezeichner.
    model: Option<String>,
    // Immer `true`; `false` bricht die Kette.
    model_authored: bool,
    // Zeitpunkt der Annahme.
    created_at: Timestamp,
}

impl VerdictRecord {
    /// Die Sequenznummer.
    ///
    /// # Returns
    /// `seq` des Datensatzes.
    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.seq
    }

    /// Der `record_hash` des Vorgängers (bzw. Anker- oder Genesis-Wert).
    ///
    /// # Returns
    /// `prev_hash` des Datensatzes.
    #[must_use]
    pub const fn prev_hash(&self) -> ContentDigest {
        self.prev_hash
    }

    /// Der Hash dieses Datensatzes.
    ///
    /// # Returns
    /// `record_hash`; zugleich [`VerdictRef::verdict_digest`].
    #[must_use]
    pub const fn record_hash(&self) -> ContentDigest {
        self.record_hash
    }

    /// Der gebundene Befund.
    ///
    /// # Returns
    /// Die `finding_id`.
    #[must_use]
    pub const fn finding_id(&self) -> &FindingId {
        &self.finding_id
    }

    /// Der gebundene `record_digest` des exportierten Befunds.
    ///
    /// # Returns
    /// Den Digest, gegen den das Verdict gebunden wurde.
    #[must_use]
    pub const fn record_digest(&self) -> ContentDigest {
        self.record_digest
    }

    /// Die Einstufung des Modells.
    ///
    /// # Returns
    /// Eine Annotation, nie ein Operator-Entscheid.
    #[must_use]
    pub const fn classification(&self) -> StoredClassification {
        self.classification
    }

    /// Das Gewicht der Einstufung laut Modell.
    ///
    /// # Returns
    /// Die Severity des Verdicts (Modellmeinung).
    #[must_use]
    pub const fn severity(&self) -> StoredSeverity {
        self.severity
    }

    /// Die vorgeschlagene Reaktion des Modells.
    ///
    /// # Returns
    /// `None`, wenn das Modell keine vorgeschlagen hat.
    #[must_use]
    pub const fn suggested_response(&self) -> Option<StoredResponse> {
        self.suggested_response
    }

    /// Die Selbsteinschätzung des Modells.
    ///
    /// # Returns
    /// `None`, wenn keine vorlag.
    #[must_use]
    pub const fn confidence(&self) -> Option<Confidence> {
        self.confidence
    }

    /// Die escapte, gekappte Begründung (vom Modell verfasst).
    ///
    /// # Returns
    /// Text, der `harw_untrusted_text::is_clean` erfüllt.
    #[must_use]
    pub fn rationale(&self) -> &str {
        &self.rationale
    }

    /// Der escapte Modellbezeichner.
    ///
    /// # Returns
    /// `None`, wenn der Aufrufer keinen kannte.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Ob der Datensatz vom Modell verfasst ist.
    ///
    /// # Returns
    /// Für jeden gültigen Datensatz `true`.
    #[must_use]
    pub const fn model_authored(&self) -> bool {
        self.model_authored
    }

    /// Der Zeitpunkt der Annahme.
    ///
    /// # Returns
    /// `created_at` des Datensatzes.
    #[must_use]
    pub const fn created_at(&self) -> Timestamp {
        self.created_at
    }

    /// Die Referenz auf diesen Datensatz.
    ///
    /// # Returns
    /// `VerdictRef { seq, verdict_digest: record_hash }`.
    #[must_use]
    pub const fn reference(&self) -> VerdictRef {
        VerdictRef {
            seq: self.seq,
            verdict_digest: self.record_hash,
        }
    }
}

/// Referenz auf einen gespeicherten Verdict-Datensatz.
///
/// # Description
/// Rückgabe von [`VerdictStore::append`]. Darf an Eltern-Agenten und in
/// IDs-und-Enums-Nachrichten (`TriageSubmission`) gehen: sie enthält keinen
/// Modelltext. `verdict_digest` ist der `record_hash` des Datensatzes.
///
/// # Examples
/// ```rust
/// use harw_session_store::dod_verdict::VerdictRef;
/// use harw_types::ContentDigest;
///
/// let verdict_ref = VerdictRef { seq: 1, verdict_digest: ContentDigest::of(b"record") };
/// assert_eq!(verdict_ref.seq, 1);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VerdictRef {
    /// Sequenznummer des Datensatzes.
    pub seq: u64,
    /// `record_hash` des Datensatzes.
    pub verdict_digest: ContentDigest,
}

/// Ergebnis einer vollständigen Prüfung der Kette.
///
/// # Examples
/// ```rust,no_run
/// # fn demo(store: &harw_session_store::dod_verdict::VerdictStore)
/// #     -> harw_session_store::dod_verdict::VerdictStoreResult<()> {
/// let summary = store.verify_chain()?;
/// assert!(summary.records == 0 || summary.last.is_some());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerdictChainSummary {
    /// Anzahl geprüfter (nicht ausgemusterter) Datensätze.
    pub records: u64,
    /// Erste Sequenznummer der Kette: [`FIRST_VERDICT_SEQ`] oder der Anker.
    pub first_seq: u64,
    /// Referenz auf den jüngsten Datensatz, falls es einen gibt.
    pub last: Option<VerdictRef>,
}

/// Ergebnis von [`VerdictStore::apply_retention`] bzw., als Vorhersage, von
/// [`VerdictStore::plan_retention`].
///
/// # Description
/// Die Retention, die [`VerdictStore::append`] beim Segmentwechsel selbst
/// ausführt, liefert keinen Bericht; sie meldet entfernte Segmente nur auf
/// `info`. Ein Plan ([`VerdictStore::plan_retention`]) trägt dieselben
/// Felder im Konjunktiv: was ein Lauf zur selben Jetzt-Zeit entfernen und
/// verankern würde.
///
/// # Examples
/// ```rust,no_run
/// # fn demo(store: &harw_session_store::dod_verdict::VerdictStore, now: jiff::Timestamp)
/// #     -> harw_session_store::dod_verdict::VerdictStoreResult<()> {
/// let report = store.apply_retention(now)?;
/// assert!(report.first_seq >= harw_session_store::dod_verdict::FIRST_VERDICT_SEQ);
/// if let Some(segment) = report.fault_segment {
///     // Die Kette ist ab diesem Segment verletzt; `verify_chain` meldet es.
///     assert!(segment >= report.first_seq);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerdictRetentionReport {
    /// Entfernte Segmente (abgelaufene und vorher ausgemusterte).
    pub removed_segments: usize,
    /// Datensätze in den entfernten, abgelaufenen Segmenten.
    pub removed_records: u64,
    /// Erste Sequenznummer der Kette nach der Retention.
    pub first_seq: u64,
    /// Erste Sequenznummer (Name) des Segments, an dem die Prüfung scheiterte
    /// (Kettenbruch, unlesbare Zeile, übergroßes Segment); `None` bei intakter
    /// Kette. Die Retention hat dann nur abgelaufene Segmente **davor**
    /// entfernt; [`VerdictStore::verify_chain`] meldet die Verletzung weiter.
    /// Aufrufer (Retention-Klasse, Doctor) zeigen das als Fehler an.
    pub fault_segment: Option<u64>,
}

/// Der Genesis-Wert für `prev_hash` des ersten Datensatzes.
///
/// # Description
/// BLAKE3 über einen eigenen Domänen-String. Nie Nullen, und kein Wert, den
/// ein echter Datensatz als `record_hash` haben kann (andere Domäne).
///
/// # Returns
/// Den festen Genesis-Digest.
///
/// # Concurrency
/// Rein.
///
/// # Examples
/// ```rust
/// use harw_session_store::dod_verdict::genesis_prev_hash;
///
/// assert_ne!(genesis_prev_hash().as_bytes(), &[0_u8; 32]);
/// ```
#[must_use]
pub fn genesis_prev_hash() -> ContentDigest {
    ContentDigest::of(GENESIS_DOMAIN)
}

/// Kappt eine Modell-Begründung auf [`MAX_RATIONALE_BYTES`] und escapt sie.
///
/// # Description
/// Die eine Stelle für „Begründung auf 2 KiB kappen und escapen“ (WP 2.3):
/// Jedes Zeichen aus `harw_untrusted_text::needs_escape` (Steuerzeichen,
/// Bidi, Nullbreiten, Tag-Block, …) wird zu seiner sichtbaren Ersatzform.
/// Passt die Ausgabe nicht in [`MAX_RATIONALE_BYTES`] Bytes, endet sie auf
/// `…`; die Marke zählt mit, gekürzt wird nur an Zeichen- und
/// Ersatzformgrenzen (nie ein halbes UTF-8-Zeichen, nie ein halbes
/// `\u{…}`). Die Funktion ist idempotent: bereinigter Text bleibt gleich.
///
/// # Arguments
/// - `raw` (`&str`): die rohe Begründung aus der Modellausgabe.
///
/// # Returns
/// Text mit höchstens [`MAX_RATIONALE_BYTES`] Bytes, der
/// `harw_untrusted_text::is_clean` erfüllt.
///
/// # Concurrency
/// Rein; die Arbeit ist durch [`MAX_RATIONALE_BYTES`] begrenzt.
///
/// # Examples
/// ```rust
/// use harw_session_store::dod_verdict::{MAX_RATIONALE_BYTES, sanitize_rationale};
///
/// assert_eq!(sanitize_rationale("ok\u{202E}txt"), "ok\\u{202e}txt");
/// let long = "ä".repeat(4_000);
/// let capped = sanitize_rationale(&long);
/// assert!(capped.len() <= MAX_RATIONALE_BYTES);
/// assert!(capped.ends_with('…'));
/// assert_eq!(sanitize_rationale(&capped), capped);
/// ```
#[must_use]
pub fn sanitize_rationale(raw: &str) -> String {
    let budget = MAX_RATIONALE_BYTES.saturating_sub(TRUNCATION_MARKER.len_utf8());
    let mut out = String::with_capacity(raw.len().min(MAX_RATIONALE_BYTES));
    // Länge der Ausgabe an der letzten Stückgrenze, an der noch `…` passt.
    let mut fits_with_marker = 0_usize;
    for ch in raw.chars() {
        let mut utf8 = [0_u8; 4];
        let piece: Cow<'_, str> = if needs_escape(ch) {
            Cow::Owned(escape_char(ch))
        } else {
            Cow::Borrowed(ch.encode_utf8(&mut utf8))
        };
        if out.len().saturating_add(piece.len()) > MAX_RATIONALE_BYTES {
            let mut capped = out.get(..fits_with_marker).unwrap_or_default().to_owned();
            capped.push(TRUNCATION_MARKER);
            return capped;
        }
        out.push_str(&piece);
        if out.len() <= budget {
            fits_with_marker = out.len();
        }
    }
    out
}

/// Append-only, verkettete Ablage der Modell-Verdicts.
///
/// # Description
/// Siehe Moduldoku für Format, Kette, Rotation, Retention, Rechte und die
/// Verzeichnisgrenze. Der Handle ist billig und zustandslos (er hält nur
/// Pfade, keinen Deskriptor); das Verzeichnis entsteht beim ersten Schreiben.
///
/// # Concurrency
/// `Send + Sync`; Schreiber serialisieren sich über `.lock`, Leser nehmen
/// keine Sperre. Jeder Aufruf öffnet das Store-Verzeichnis selbst und hält
/// den Deskriptor nur bis zu seiner Rückkehr (siehe Moduldoku).
///
/// # Examples
/// ```rust,no_run
/// use harw_session_store::dod_verdict::VerdictStore;
///
/// let store = VerdictStore::new(std::path::Path::new("/home/user/.harw"));
/// assert!(store.root().ends_with("dod/verdicts"));
/// ```
#[derive(Debug, Clone)]
pub struct VerdictStore {
    // Das vertraute Zustandsverzeichnis des Aufrufers: Ausgangspunkt jedes
    // Abstiegs (`open_root`/`create_root`).
    state_dir: PathBuf,
    // `<state_dir>/dod/verdicts`, nur für `root()`; der Store öffnet nie über
    // diesen Pfad.
    root: PathBuf,
    // Rotationsgrenze in Bytes; außerhalb von Tests immer `VERDICT_SEGMENT_BYTES`.
    segment_limit: u64,
    // Ob `append` beim Beginn eines neuen Segments die Retention mitlaufen
    // lässt; außerhalb von Tests immer `true` (Tests von `apply_retention`
    // bauen abgelaufene Segmente auf, ohne dass sie schon beim Anhängen gehen).
    auto_retention: bool,
}

impl VerdictStore {
    /// Erzeugt einen Store unter `<state_dir>/dod/verdicts`.
    ///
    /// # Description
    /// Greift nicht auf das Dateisystem zu. `state_dir` ist die harw-Heimat
    /// (`~/.harw` oder ein Override); dieselbe Wurzel, die die
    /// Retention-Resolver (`roots.home.join("dod")`) verwenden. Der Aufrufer
    /// verbürgt sich für `state_dir` (es darf ein Symlink sein); ab `dod`
    /// abwärts prüft der Store jedes Glied selbst.
    ///
    /// # Arguments
    /// - `state_dir` (`&Path`): das Zustandsverzeichnis.
    ///
    /// # Returns
    /// Einen Handle auf den Store.
    ///
    /// # Errors
    /// Keine.
    ///
    /// # Concurrency
    /// Rein.
    ///
    /// # Examples
    /// ```rust
    /// use harw_session_store::dod_verdict::VerdictStore;
    ///
    /// let store = VerdictStore::new(std::path::Path::new("/state"));
    /// assert_eq!(store.root(), std::path::Path::new("/state/dod/verdicts"));
    /// ```
    #[must_use]
    pub fn new(state_dir: &Path) -> Self {
        Self {
            state_dir: state_dir.to_path_buf(),
            root: state_dir.join(DOD_DIR).join(VERDICTS_DIR),
            segment_limit: VERDICT_SEGMENT_BYTES,
            auto_retention: true,
        }
    }

    /// Das Store-Verzeichnis (`<state_dir>/dod/verdicts`).
    ///
    /// # Description
    /// Nur zur Anzeige und für Werkzeuge: der Store selbst öffnet nie über
    /// diesen Pfad, sondern steigt je Aufruf Glied für Glied ab (siehe
    /// Moduldoku, „Verzeichnisgrenze“).
    ///
    /// # Returns
    /// Den Pfad, unter dem Segmente, Anker und Sperre liegen.
    ///
    /// # Examples
    /// ```rust
    /// use harw_session_store::dod_verdict::VerdictStore;
    ///
    /// let store = VerdictStore::new(std::path::Path::new("/state"));
    /// assert!(store.root().ends_with("verdicts"));
    /// ```
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    // Nur für Tests: kleine Rotationsgrenze statt 4-MiB-Fixtures.
    #[cfg(test)]
    fn with_segment_limit(mut self, segment_limit: u64) -> Self {
        self.segment_limit = segment_limit;
        self
    }

    // Nur für Tests: `append` lässt beim Segmentwechsel keine Retention
    // mitlaufen, damit Tests von `apply_retention` abgelaufene Segmente
    // anlegen können.
    #[cfg(test)]
    fn without_auto_retention(mut self) -> Self {
        self.auto_retention = false;
        self
    }

    /// Hängt ein Verdict an die Kette an.
    ///
    /// # Description
    /// Prüft und bereinigt die Eingabe (siehe [`NewVerdict`]) und nimmt die
    /// Sperre. Unter der Sperre prüft es zuerst die **gesamte** lebende Kette
    /// vorwärts wie [`VerdictStore::verify_chain`]: Anker, jedes Segment in
    /// Reihenfolge, lückenlose `seq`, jede `prev_hash`-Verknüpfung, jedes
    /// Siegel (`record_hash`, `model_authored`, Invarianten) und jeden
    /// Segmentnamen. Erst danach ändert es etwas: es schneidet eine
    /// abgerissene letzte Zeile ab, vergibt `seq` und `prev_hash` aus der
    /// Prüfung, setzt `model_authored = true`, berechnet `record_hash` und
    /// schreibt die Zeile mit einem `write_all` plus `sync_data` in das
    /// aktive oder ein neues Segment (`0600`, Verzeichnis `0700`). Ein neues
    /// Segment beginnt bei [`VERDICT_SEGMENT_BYTES`] oder nach
    /// [`VERDICT_SEGMENT_SPAN`] (gemessen am ersten Datensatz des aktiven
    /// Segments). An eine verletzte Kette wird nie angehängt, auch wenn die
    /// Verletzung weit vor dem Ende liegt und der letzte Datensatz für sich
    /// genommen intakt ist.
    ///
    /// Beginnt es ein neues Segment, läuft danach unter derselben Sperre die
    /// Retention wie bei [`VerdictStore::apply_retention`], mit `created_at`
    /// als Jetzt-Zeit und auf derselben Prüfung; nur Segmente vor dem bisher
    /// aktiven kommen in Frage (eine weit vorgestellte Uhr löscht so nie den
    /// jüngsten bisherigen Stand, siehe Moduldoku „Retention“). Ein Fehler
    /// dabei erscheint nur auf `warn`; der Datensatz ist dann schon quittiert
    /// und das Ergebnis bleibt `Ok`.
    ///
    /// # Arguments
    /// - `verdict` (`&NewVerdict`): das angenommene, gebundene Verdict.
    ///
    /// # Returns
    /// [`VerdictRef`] mit `seq` und `verdict_digest` (= `record_hash`).
    ///
    /// # Errors
    /// - [`VerdictStoreError::FindingIdInvalid`]: ungültige `finding_id`.
    /// - [`VerdictStoreError::RecordTooLarge`]: Zeile über 8 KiB.
    /// - [`VerdictStoreError::LockContended`]: Sperre länger belegt, oder
    ///   `.lock` wurde ersetzt, während sie gehalten war (nichts geändert;
    ///   ein erneuter Versuch sperrt die aktuelle Datei).
    /// - [`VerdictStoreError::ChainBroken`],
    ///   [`VerdictStoreError::CorruptRecord`],
    ///   [`VerdictStoreError::SegmentTooLarge`]: die Kette ist an
    ///   irgendeiner Stelle verletzt; derselbe Fehler, den
    ///   [`VerdictStore::verify_chain`] meldet. Kein Segment wurde geändert.
    ///   Der Zustand hält an, bis ein Operator den Store beiseitelegt
    ///   (Moduldoku „Wiederherstellung nach einem Kettenbruch“).
    /// - [`VerdictStoreError::AnchorCorrupt`]: `anchor.json` ist unlesbar
    ///   oder ein Symlink.
    /// - [`VerdictStoreError::UnsafeDirectory`]: `dod` oder `verdicts` ist ein
    ///   Symlink, kein Verzeichnis, nicht das eigene oder für Gruppe/Andere
    ///   beschreibbar, oder `.lock` ist ein Symlink. Am Ziel eines Symlinks
    ///   wird nichts angelegt.
    /// - [`VerdictStoreError::UnsafeSegment`]: ein Segment ist keine
    ///   reguläre Datei (z. B. ein Symlink).
    /// - [`VerdictStoreError::SequenceExhausted`],
    ///   [`VerdictStoreError::Io`], [`VerdictStoreError::Serde`].
    ///
    /// # Concurrency
    /// Blockierende Datei-I/O unter der exklusiven Sperre (siehe Moduldoku).
    /// Das Store-Verzeichnis wird zu Beginn Glied für Glied geöffnet; Sperre,
    /// Prüfung und Schreiben laufen danach nur relativ zu diesem Deskriptor.
    /// Kosten: O(Größe der lebenden Kette) je Aufruf, weil jedes Anhängen
    /// die ganze Kette liest und prüft; die Retention beim Segmentwechsel
    /// begrenzt diese Größe auf etwa 91 Tage plus höchstens ein Segment
    /// (Sperr-Budget gleichzeitiger Schreiber: Moduldoku „Nebenläufigkeit“).
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_session_store::dod_verdict::{
    ///     NewVerdict, StoredClassification, StoredSeverity, VerdictStore,
    /// };
    /// use harw_types::{ContentDigest, FindingId};
    ///
    /// # fn demo(store: &VerdictStore, now: jiff::Timestamp)
    /// #     -> harw_session_store::dod_verdict::VerdictStoreResult<()> {
    /// let verdict_ref = store.append(&NewVerdict {
    ///     finding_id: FindingId::from_str("finding-1"),
    ///     record_digest: ContentDigest::of(b"record"),
    ///     classification: StoredClassification::Benign,
    ///     severity: StoredSeverity::Info,
    ///     suggested_response: None,
    ///     confidence: None,
    ///     rationale: "bekannter Dienst".to_owned(),
    ///     model: None,
    ///     created_at: now,
    /// })?;
    /// assert!(verdict_ref.seq >= 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn append(&self, verdict: &NewVerdict) -> VerdictStoreResult<VerdictRef> {
        let prepared = prepare(verdict)?;
        let root = self.create_root()?;
        self.append_at(&root, prepared)
    }

    /// Prüft die gesamte Kette.
    ///
    /// # Description
    /// Liest den Anker (falls vorhanden) und alle nicht ausgemusterten
    /// Segmente in Reihenfolge und prüft je Datensatz: `seq` lückenlos,
    /// `prev_hash` verweist auf den Vorgänger (bzw. Anker oder Genesis),
    /// `record_hash` stimmt, `model_authored` ist gesetzt und die Felder
    /// erfüllen die Invarianten (Begründung escapt und ≤ 2 KiB). Der erste
    /// Datensatz eines Segments muss zu dessen Namen passen. Eine
    /// abgerissene letzte Zeile des letzten Segments gilt als nicht
    /// geschrieben und wird übersprungen.
    ///
    /// # Returns
    /// [`VerdictChainSummary`]; leer, wenn der Store noch nicht existiert
    /// (ein Leser legt nichts an).
    ///
    /// # Errors
    /// - [`VerdictStoreError::ChainBroken`]: eine Prüfung scheiterte.
    /// - [`VerdictStoreError::CorruptRecord`],
    ///   [`VerdictStoreError::AnchorCorrupt`],
    ///   [`VerdictStoreError::SegmentTooLarge`],
    ///   [`VerdictStoreError::UnsafeDirectory`],
    ///   [`VerdictStoreError::UnsafeSegment`], [`VerdictStoreError::Io`]
    ///   (`NotFound` erst, wenn auch der dritte Durchlauf ein gelistetes
    ///   Segment nicht mehr fand).
    ///
    /// # Concurrency
    /// Nur lesend, ohne Sperre. Entfernt eine gleichzeitige Retention ein
    /// gelistetes Segment, läuft die Kette mit neuer Liste und neuem Anker
    /// noch einmal ab (höchstens drei Durchläufe; siehe Moduldoku).
    ///
    /// # Examples
    /// ```rust,no_run
    /// # fn demo(store: &harw_session_store::dod_verdict::VerdictStore)
    /// #     -> harw_session_store::dod_verdict::VerdictStoreResult<()> {
    /// let summary = store.verify_chain()?;
    /// assert!(summary.first_seq >= 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn verify_chain(&self) -> VerdictStoreResult<VerdictChainSummary> {
        self.read_verified(|| (), |_, _| {})
            .map(|(summary, ())| summary)
    }

    /// Der jüngste Datensatz zu einem Befund.
    ///
    /// # Description
    /// Läuft die Kette wie [`VerdictStore::verify_chain`] ab und liefert den
    /// Datensatz mit der höchsten `seq` für `finding_id`. Eine gebrochene
    /// Kette liefert einen Fehler statt eines womöglich manipulierten
    /// Datensatzes.
    ///
    /// # Arguments
    /// - `finding_id` (`&FindingId`): der gesuchte Befund.
    ///
    /// # Returns
    /// `Some(record)` oder `None`, wenn es kein Verdict zu dem Befund gibt.
    ///
    /// # Errors
    /// Wie [`VerdictStore::verify_chain`].
    ///
    /// # Concurrency
    /// Nur lesend, ohne Sperre.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_types::FindingId;
    ///
    /// # fn demo(store: &harw_session_store::dod_verdict::VerdictStore)
    /// #     -> harw_session_store::dod_verdict::VerdictStoreResult<()> {
    /// let latest = store.latest_for(&FindingId::from_str("finding-1"))?;
    /// assert!(latest.is_none_or(|record| record.model_authored()));
    /// # Ok(())
    /// # }
    /// ```
    pub fn latest_for(&self, finding_id: &FindingId) -> VerdictStoreResult<Option<VerdictRecord>> {
        self.read_verified(
            || None::<VerdictRecord>,
            |latest, record| {
                if record.finding_id == *finding_id {
                    *latest = Some(record.clone());
                }
            },
        )
        .map(|(_, latest)| latest)
    }

    /// Alle geprüften Datensätze in Kettenreihenfolge.
    ///
    /// # Description
    /// Für Oberflächen wie `harw dod verdicts`. Läuft die Kette wie
    /// [`VerdictStore::verify_chain`] ab.
    ///
    /// # Returns
    /// Die Datensätze ab dem Anker, aufsteigend nach `seq`.
    ///
    /// # Errors
    /// Wie [`VerdictStore::verify_chain`].
    ///
    /// # Concurrency
    /// Nur lesend, ohne Sperre.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # fn demo(store: &harw_session_store::dod_verdict::VerdictStore)
    /// #     -> harw_session_store::dod_verdict::VerdictStoreResult<()> {
    /// for record in store.records()? {
    ///     assert!(record.model_authored());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn records(&self) -> VerdictStoreResult<Vec<VerdictRecord>> {
        self.read_verified(Vec::<VerdictRecord>::new, |records, record| {
            records.push(record.clone());
        })
        .map(|(_, records)| records)
    }

    /// Entfernt abgelaufene Segmente, ohne die Prüfbarkeit der Kette zu
    /// verlieren.
    ///
    /// # Description
    /// Unter der Sperre: prüft die Kette, bestimmt die längste Folge von
    /// vollständig geprüften Segmenten am Anfang (nie eines ab einer
    /// Verletzung; das letzte Segment nur, wenn auch es ganz abgelaufen ist),
    /// deren jüngster Datensatz mehr als
    /// [`VERDICT_RETENTION`] vor `now` liegt, schreibt den neuen Anker atomar
    /// (`0600`) und löscht danach diese und zuvor ausgemusterte Segmente; zum
    /// Schluss ein `fsync` des Verzeichnisses. Eine Verletzung der Kette
    /// stoppt die Retention nicht ganz: sie endet vor dem betroffenen Segment
    /// und meldet es in [`VerdictRetentionReport::fault_segment`] (und auf
    /// `warn`, inhaltsfrei). Die Verletzung wird nie neu verankert. Ist alles
    /// abgelaufen, bleibt nur der Anker: [`VerdictStore::verify_chain`]
    /// meldet dann `first_seq` = letzte `seq` + 1 und null Datensätze, und
    /// das nächste Anhängen setzt die Kette ab dem Anker fort.
    ///
    /// [`VerdictStore::append`] führt denselben Lauf beim Beginn jedes neuen
    /// Segments selbst aus (mit `created_at` als Jetzt-Zeit, ohne das bisher
    /// aktive Segment); diese Methode ist für explizite Läufe, etwa einen
    /// Wartungsjob oder einen Host, der nicht mehr anhängt, und kennt diese
    /// Grenze nicht. Sie ist der einzige zulässige Weg für ein Werkzeug, das
    /// den Store ausdünnen will (kein dateiweises Löschen, Moduldoku
    /// „Retention“). Was sie entfernen würde, sagt
    /// [`VerdictStore::plan_retention`] vorher, ohne etwas zu ändern (dieselbe
    /// Auswahl, dieselbe Berechnung).
    ///
    /// # Arguments
    /// - `now` (`Timestamp`): die Jetzt-Zeit aus der `Clock` des Aufrufers.
    ///
    /// # Returns
    /// [`VerdictRetentionReport`]; nichts entfernt, wenn der Store fehlt.
    ///
    /// # Errors
    /// [`VerdictStoreError::LockContended`] (wie bei
    /// [`VerdictStore::append`], auch für ein ersetztes `.lock`),
    /// [`VerdictStoreError::AnchorCorrupt`],
    /// [`VerdictStoreError::UnsafeDirectory`] (wie bei
    /// [`VerdictStore::append`]),
    /// [`VerdictStoreError::UnsafeSegment`], [`VerdictStoreError::Io`],
    /// [`VerdictStoreError::Serde`]. Eine Verletzung der Kette ist kein
    /// Fehler dieser Methode (siehe oben). Bei einem Fehler vor dem Anker
    /// wird nichts gelöscht.
    ///
    /// # Concurrency
    /// Blockierende Datei-I/O unter der exklusiven Sperre. Prüfen, Verankern
    /// und Löschen (`unlinkat`) laufen nur relativ zum zu Beginn geöffneten
    /// Store-Verzeichnis; ein Löschen kann also nie ein anderes Verzeichnis
    /// treffen, auch wenn der Pfad inzwischen woanders hinzeigt.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # fn demo(store: &harw_session_store::dod_verdict::VerdictStore, now: jiff::Timestamp)
    /// #     -> harw_session_store::dod_verdict::VerdictStoreResult<()> {
    /// let report = store.apply_retention(now)?;
    /// assert_eq!(store.verify_chain()?.first_seq, report.first_seq);
    /// # Ok(())
    /// # }
    /// ```
    pub fn apply_retention(&self, now: Timestamp) -> VerdictStoreResult<VerdictRetentionReport> {
        let Some(root) = self.open_root()? else {
            return Ok(VerdictRetentionReport {
                removed_segments: 0,
                removed_records: 0,
                first_seq: FIRST_VERDICT_SEQ,
                fault_segment: None,
            });
        };
        root.set_permissions(Permissions::from_mode(DIR_MODE))?;
        retention_at(&root, now)
    }

    /// Sagt vorher, was [`VerdictStore::apply_retention`] zur Jetzt-Zeit
    /// `now` entfernen würde, ohne etwas zu ändern (Probelauf).
    ///
    /// # Description
    /// Läuft die Kette ab wie ein Leser ([`VerdictStore::verify_chain`]) und
    /// rechnet auf diesem Durchlauf mit **derselben** reinen Funktion wie
    /// [`VerdictStore::apply_retention`] aus, was ein Lauf täte: vorher
    /// ausgemusterte Segmente plus die längste Folge vollständig geprüfter,
    /// ganz abgelaufener Segmente am Anfang der Kette (nie eines ab einer
    /// Verletzung), die Datensätze darin, `first_seq` nach dem neuen Anker
    /// und ein etwaiges [`VerdictRetentionReport::fault_segment`]. Eine
    /// Verletzung der Kette ist wie dort kein Fehler, sondern begrenzt den
    /// Plan auf das geprüfte Präfix; anders als dort erscheint sie nicht auf
    /// `warn` (der Aufrufer zeigt `fault_segment` an, ein Probelauf soll
    /// keine doppelten Warnungen erzeugen).
    ///
    /// Schreibt nichts: keine Sperre (`.lock` wird weder angelegt noch
    /// gesperrt), kein Anlegen fehlender Verzeichnisse, kein `fchmod`, kein
    /// Anker, kein Löschen, kein `fsync`. Fehlt der Store (oder ein Glied bis
    /// `verdicts`), ist der Plan leer, wie der Bericht von
    /// [`VerdictStore::apply_retention`] dann.
    ///
    /// # Arguments
    /// - `now` (`Timestamp`): die Jetzt-Zeit aus der `Clock` des Aufrufers;
    ///   für einen Vergleich mit einem späteren Lauf dieselbe wie dort.
    ///
    /// # Returns
    /// Den [`VerdictRetentionReport`], den `apply_retention(now)` auf dem
    /// gelesenen Stand liefern würde.
    ///
    /// # Errors
    /// [`VerdictStoreError::UnsafeDirectory`],
    /// [`VerdictStoreError::UnsafeSegment`],
    /// [`VerdictStoreError::AnchorCorrupt`],
    /// [`VerdictStoreError::SequenceExhausted`], [`VerdictStoreError::Io`]
    /// (auch `NotFound`, wenn gleichzeitige Retentionen in drei Durchläufen
    /// hintereinander ein gelistetes Segment entfernen). Eine Verletzung der
    /// Kette ist kein Fehler (siehe oben).
    ///
    /// # Concurrency
    /// Nur lesend, ohne Sperre, wie [`VerdictStore::verify_chain`]:
    /// verschwindet ein gelistetes Segment, läuft die Kette mit neuer Liste
    /// und neuem Anker noch einmal ab (höchstens drei Durchläufe). Der Plan
    /// ist eine Momentaufnahme: ein gleichzeitiges Anhängen (samt seiner
    /// Retention beim Segmentwechsel) oder eine andere Retention kann den
    /// Stand ändern, bevor ein späteres [`VerdictStore::apply_retention`]
    /// läuft, dessen Bericht dann abweicht. Auf einem unveränderten Store
    /// stimmen beide Berichte überein.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # fn demo(store: &harw_session_store::dod_verdict::VerdictStore, now: jiff::Timestamp)
    /// #     -> harw_session_store::dod_verdict::VerdictStoreResult<()> {
    /// let plan = store.plan_retention(now)?;
    /// let report = store.apply_retention(now)?;
    /// // Ohne Schreiber dazwischen derselbe Bericht.
    /// assert_eq!(plan, report);
    /// # Ok(())
    /// # }
    /// ```
    pub fn plan_retention(&self, now: Timestamp) -> VerdictStoreResult<VerdictRetentionReport> {
        retry_vanished(|| {
            let walk = self.walk(|_| {})?;
            let report = retention_plan(&walk, now, None)?.report;
            tracing::debug!(
                removed_segments = report.removed_segments,
                removed_records = report.removed_records,
                first_seq = report.first_seq,
                "verdict retention planned"
            );
            Ok(report)
        })
    }

    // Anhängen relativ zum schon geöffneten und geprüften Store-Verzeichnis
    // `root` (aus `create_root`): Sperre in `root`, dann `append_locked`.
    // Getrennt von `append`, damit Tests ein Verzeichnis zwischen Abstieg und
    // Schreiben austauschen können.
    fn append_at(&self, root: &File, prepared: NewVerdict) -> VerdictStoreResult<VerdictRef> {
        let lock = acquire_lock(root)?;
        let result = self.append_locked(prepared, root, &lock);
        release(lock, result)
    }

    // Kern von `append`; Vorbedingung: die Sperre `lock` in `root` ist
    // gehalten (`acquire_lock`).
    //
    // Reihenfolge (Review-Befund zu PL-100 WP 2.3): **erst** die gesamte
    // lebende Kette vorwärts prüfen (`walk_root`: Anker → jedes Segment →
    // Ende), **dann** etwas ändern. Früher wurde nur das Siegel des letzten
    // Datensatzes nachgerechnet; ein neu versiegelter Datensatz weiter vorn
    // bricht aber die Verknüpfung seines Nachfolgers, während der letzte
    // Datensatz für sich intakt bleibt. `seq` und `prev_hash` stammen deshalb
    // nur aus dem Cursor der Prüfung, und bei einer Verletzung irgendwo in
    // der Kette wird kein Segment zum Schreiben geöffnet, per `fchmod`
    // geändert, abgeschnitten oder angelegt. Weil die Schreibsperre gehalten
    // ist, ändert kein anderer Schreiber die Kette zwischen Prüfung und
    // Schreiben; `ensure_lock_held` stellt direkt vor der ersten Änderung
    // sicher, dass `.lock` noch die gesperrte Datei ist. Alles läuft relativ
    // zu `root` (Moduldoku „Verzeichnisgrenze“).
    //
    // Beginnt das Anhängen ein neues Segment, läuft danach unter derselben
    // Sperre die Retention (`prune_expired`) auf demselben Durchlauf, mit
    // `created_at` als Jetzt-Zeit (Moduldoku „Retention“), begrenzt durch
    // `auto_retention_limit`. Sie hält die lebende Kette und damit die Kosten
    // der Vorwärtsprüfung begrenzt, auch wenn nie jemand `apply_retention`
    // aufruft. Der Datensatz ist zu diesem Zeitpunkt schon quittiert; ein
    // Fehler der Retention wird nur gemeldet.
    fn append_locked(
        &self,
        prepared: NewVerdict,
        root: &File,
        lock: &File,
    ) -> VerdictStoreResult<VerdictRef> {
        let mut walk = walk_root(root, |_| {})?;
        if let Some((segment_seq, error)) = walk.fault.take() {
            tracing::warn!(
                segment = segment_seq,
                error = %error,
                "verdict chain fault: append refused until the store directory is moved aside"
            );
            return Err(error);
        }
        ensure_lock_held(root, lock)?;

        let seq = walk.next.seq;
        let created_at = prepared.created_at;
        let record = seal(seq, walk.next.prev, prepared);
        let line = encode_line(&record)?;
        let line_len = u64::try_from(line.len()).unwrap_or(u64::MAX);

        // Das aktive (letzte geprüfte) Segment: eine abgerissene letzte Zeile
        // (nie quittiert, von der Prüfung schon übersprungen) wird
        // abgeschnitten; danach Länge und die Frage, ob seine Zeitspanne
        // (ab dem ersten Datensatz) erschöpft ist. Ein leeres Segment hat
        // keine Spanne; läuft die Uhr zurück (negative Dauer), rotiert nichts.
        let active = match walk.live.last() {
            Some(stats) => {
                let mut file = open_segment_append(root, &stats.name)?;
                repair_torn_tail(&mut file, stats.first_seq)?;
                let len = file.metadata()?.len();
                let span_exhausted = stats
                    .first_created
                    .is_some_and(|first| created_at.duration_since(first) >= VERDICT_SEGMENT_SPAN);
                Some((stats.first_seq, file, len, span_exhausted))
            }
            None => None,
        };

        // Ein nicht leeres Segment wird fortgesetzt, solange Größe und
        // Zeitspanne reichen; ein leeres aktives Segment nur, wenn sein Name
        // zu `seq` passt. `new_segment`: das Ziel ist ein anderes Segment als
        // das aktive (Rotation, oder das erste Segment eines leeren bzw.
        // verankerten Stores); es wird angelegt oder, als leerer Rest eines
        // gescheiterten Anhängens, wiederverwendet.
        let (mut file, new_segment) = match active {
            Some((segment_seq, file, len, span_exhausted))
                if (len == 0 && segment_seq == seq)
                    || (len > 0
                        && !span_exhausted
                        && len.saturating_add(line_len) <= self.segment_limit) =>
            {
                (file, false)
            }
            _ => (open_segment_append(root, &segment_file_name(seq))?, true),
        };
        // `created`: der Verzeichniseintrag des Ziels ist womöglich noch nicht
        // dauerhaft. Das gilt für jedes **leere** Ziel, auf beiden Wegen: ein
        // gerade angelegtes Segment ebenso wie ein leerer Rest aus einem
        // früheren Anhängen, das nach dem Anlegen scheiterte
        // (`write_all`/`sync_data`) und seinen Verzeichnis-`fsync` nie bekam,
        // ob es nun als aktives Segment fortgesetzt oder als Rotationsziel
        // wiederverwendet wird. Ein nicht leeres Ziel stammt aus einem
        // quittierten Anhängen und ist schon dauerhaft eingetragen.
        let created = file.metadata()?.len() == 0;
        file.write_all(line.as_bytes())?;
        file.sync_data()?;
        if created {
            root.sync_all()?;
        }
        tracing::debug!(seq, "verdict record appended");

        // Retention beim Segmentwechsel, auf dem Durchlauf von oben (die
        // Sperre ist noch gehalten; geändert hat sich seither nur das
        // Ziel-Segment, und das liegt nie vor der Grenze aus
        // `auto_retention_limit`).
        if new_segment
            && self.auto_retention
            && let Err(error) = prune_expired(
                root,
                lock,
                &walk,
                created_at,
                Some(auto_retention_limit(&walk, seq)),
            )
        {
            tracing::warn!(
                seq,
                error = %error,
                "verdict retention after segment rotation failed; the record is stored"
            );
        }
        Ok(record.reference())
    }

    // Läuft die Kette eines Lesers ab; fehlt ein Glied bis `verdicts`, ist
    // die Kette leer (ab Genesis). Ein Leser legt nichts an und ändert nichts.
    fn walk(&self, visit: impl FnMut(&VerdictRecord)) -> VerdictStoreResult<Walk> {
        match self.open_root()? {
            Some(root) => walk_root(&root, visit),
            None => {
                let (first_seq, prev) = chain_start(None);
                Ok(Walk::empty(first_seq, prev))
            }
        }
    }

    // Die Kette für einen Leser: `visit` sammelt je gültigem Datensatz in
    // einen Zustand aus `fresh`. Eine Verletzung der Kette ist ein Fehler
    // (Leser bekommen nie einen womöglich manipulierten Stand). Verschwindet
    // ein gelistetes Segment, bevor der Leser es öffnet (eine gleichzeitige
    // Retention), beginnt der Durchlauf mit neuem Zustand von vorn
    // (`retry_vanished`); was der abgebrochene Versuch gesammelt hat, geht nie
    // in das Ergebnis ein.
    fn read_verified<T>(
        &self,
        mut fresh: impl FnMut() -> T,
        mut visit: impl FnMut(&mut T, &VerdictRecord),
    ) -> VerdictStoreResult<(VerdictChainSummary, T)> {
        retry_vanished(|| {
            let mut state = fresh();
            let mut walk = self.walk(|record| visit(&mut state, record))?;
            match walk.fault.take() {
                Some((_, error)) => Err(error),
                None => Ok((walk.summary, state)),
            }
        })
    }

    // Öffnet das vorhandene Store-Verzeichnis Glied für Glied (Moduldoku
    // „Verzeichnisgrenze“): `state_dir` (vertraut), darunter `dod` und
    // `verdicts`, je relativ zum Eltern-Deskriptor mit `O_DIRECTORY |
    // O_NOFOLLOW` geöffnet (`open_dir_at`) und geprüft. Legt nichts an und
    // ändert nichts; `None`, wenn ein Glied fehlt.
    fn open_root(&self) -> VerdictStoreResult<Option<File>> {
        let Some(state) = open_state_dir(&self.state_dir)? else {
            return Ok(None);
        };
        let Some(dod) = open_store_dir(&state, DOD_DIR)? else {
            return Ok(None);
        };
        open_store_dir(&dod, VERDICTS_DIR)
    }

    // Wie `open_root`, legt aber fehlende Glieder an (`state_dir` samt
    // Eltern über den vertrauten Pfad, `dod` und `verdicts` per `mkdirat`
    // relativ zum Eltern-Deskriptor; jedes neue Glied mit `fsync` seines
    // Elternverzeichnisses) und setzt `verdicts` per `fchmod` auf `0700`
    // (umask-unabhängig). Geliefert wird der Deskriptor von `verdicts`; ab
    // hier wird kein Pfad mehr aufgelöst. Dauerhaftigkeit: ohne den `fsync`
    // des Elternverzeichnisses könnte nach einem Stromausfall `verdicts/`
    // samt quittiertem Segment fehlen.
    fn create_root(&self) -> VerdictStoreResult<File> {
        let state = create_state_dir(&self.state_dir)?;
        let dod = create_store_dir(&state, DOD_DIR)?;
        let root = create_store_dir(&dod, VERDICTS_DIR)?;
        root.set_permissions(Permissions::from_mode(DIR_MODE))?;
        Ok(root)
    }
}

// Retention relativ zum schon geöffneten und geprüften Store-Verzeichnis
// `root`: Sperre in `root`, dann `retention_locked`. Getrennt von
// `apply_retention`, damit Tests ein Verzeichnis zwischen Abstieg und
// Löschen austauschen können.
fn retention_at(root: &File, now: Timestamp) -> VerdictStoreResult<VerdictRetentionReport> {
    let lock = acquire_lock(root)?;
    let result = retention_locked(root, &lock, now);
    release(lock, result)
}

// Kern von `apply_retention`; Vorbedingung: die Sperre `lock` in `root` ist
// gehalten. Prüfen, Verankern und Löschen laufen nur relativ zu `root`.
fn retention_locked(
    root: &File,
    lock: &File,
    now: Timestamp,
) -> VerdictStoreResult<VerdictRetentionReport> {
    // Erst die Kette prüfen. `walk.live` enthält nur vollständig geprüfte
    // Segmente vor einer etwaigen Verletzung; eine verletzte Stelle wird
    // nie neu verankert. Der neue Anker (`first_seq`, `prev_hash`) stammt
    // nur aus diesem vorwärts geprüften Präfix.
    let walk = walk_root(root, |_| {})?;
    if let Some((segment_seq, error)) = &walk.fault {
        tracing::warn!(
            segment = *segment_seq,
            error = %error,
            "verdict chain fault: retention limited to the verified prefix"
        );
    }
    prune_expired(root, lock, &walk, now, None)
}

// Der verankernde und löschende Teil der Retention, auf einem schon unter der
// Sperre gemachten Durchlauf `walk` von `root` (`retention_locked` und, beim
// Segmentwechsel, `append_locked`). Vorbedingung: die Sperre `lock` in `root`
// ist gehalten, und seit `walk` hat sich höchstens ein Segment ab `before_seq`
// geändert. `before_seq`: nur Segmente, deren erste `seq` davor liegt, kommen
// in Frage (`None`: alle in `walk.live`). Was entfernt und verankert wird,
// rechnet allein `retention_plan` (dieselbe Funktion, die
// `VerdictStore::plan_retention` für den Probelauf benutzt); hier wird der
// Plan nur ausgeführt, und der Bericht ist der des Plans. Anker und Löschen
// laufen nur relativ zu `root`, und erst nach `ensure_lock_held`.
fn prune_expired(
    root: &File,
    lock: &File,
    walk: &Walk,
    now: Timestamp,
    before_seq: Option<u64>,
) -> VerdictStoreResult<VerdictRetentionReport> {
    let plan = retention_plan(walk, now, before_seq)?;
    if plan.doomed.is_empty() {
        // Nichts auszumustern, also auch nichts zu verankern (ein neuer Anker
        // setzt ein abgelaufenes Segment mit Datensätzen voraus).
        return Ok(plan.report);
    }
    ensure_lock_held(root, lock)?;

    if let Some(next) = &plan.anchor {
        write_anchor(
            root,
            &VerdictAnchor {
                version: ANCHOR_VERSION,
                first_seq: next.seq,
                prev_hash: next.prev,
                anchored_at: now,
            },
        )?;
    }

    // `unlinkat` relativ zu `root`: auch wenn der Pfad des Store-Verzeichnisses
    // inzwischen auf etwas anderes zeigt, wird nur hier gelöscht. Ein schon
    // verschwundenes Segment zählt wie im Plan als entfernt.
    for name in plan.doomed.iter().copied() {
        match unlink_at(root.as_fd(), Path::new(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(VerdictStoreError::Io(error)),
        }
    }
    root.sync_all()?;
    tracing::info!(
        removed_segments = plan.report.removed_segments,
        removed_records = plan.report.removed_records,
        first_seq = plan.report.first_seq,
        "verdict retention applied"
    );
    Ok(plan.report)
}

// Was eine Retention auf einem Durchlauf täte, rein berechnet aus dem
// Durchlauf, der Jetzt-Zeit und der Grenze (`retention_plan`). `prune_expired`
// führt genau diesen Plan aus, `VerdictStore::plan_retention` meldet nur
// seinen Bericht; so können Probelauf und Lauf nicht auseinanderlaufen.
struct RetentionPlan<'walk> {
    // Zu löschende Segmente (Dateinamen) in Löschreihenfolge: erst die
    // ausgemusterten (vor dem Anker, aus einer unterbrochenen Retention),
    // dann die abgelaufenen am Anfang von `walk.live`.
    doomed: Vec<&'walk str>,
    // Der neue Anker: `seq` und `prev_hash` hinter dem letzten Datensatz der
    // abgelaufenen Segmente; `None`, wenn darin kein Datensatz liegt (der
    // bisherige Anker bzw. Genesis bleibt).
    anchor: Option<ChainCursor>,
    // Der Bericht nach Ausführung des Plans.
    report: VerdictRetentionReport,
}

// Berechnet den Plan der Retention auf `walk` zur Jetzt-Zeit `now`; liest und
// schreibt nichts. `before_seq` wie bei `prune_expired`.
//
// Kandidaten sind alle Segmente in `walk.live` vor `before_seq`, auch das
// letzte (aktive): ist es ganz abgelaufen, bleibt nur der Anker. Das ist
// sicher, weil ein Lauf die Sperre hält und `append_locked` eine leere
// `live`-Liste ab dem Anker fortsetzt (`seq = first_seq`, `prev = prev_hash`,
// neues Segment `first_seq`); `walk_listed` prüft eine leere Liste gegen den
// Anker. Abgelaufen ist ein Segment, dessen jüngster Datensatz mehr als
// `VERDICT_RETENTION` vor `now` liegt (ein leeres Segment immer); gezählt wird
// nur die Folge am Anfang. Bei einer Verletzung steht das verletzte Segment
// (und alles dahinter) gar nicht in `live`, wird also nie gezählt; es
// erscheint nur als `fault_segment`. Ausgemusterte Segmente (`walk.retired`)
// zählen immer mit, auch wenn sie bis zum Löschen schon verschwunden sind.
//
// Fehler: `SequenceExhausted`, wenn hinter dem letzten abgelaufenen
// Datensatz keine `seq` mehr frei ist.
fn retention_plan(
    walk: &Walk,
    now: Timestamp,
    before_seq: Option<u64>,
) -> VerdictStoreResult<RetentionPlan<'_>> {
    let removable = walk
        .live
        .iter()
        .take_while(|stats| {
            before_seq.is_none_or(|limit| stats.first_seq < limit)
                && stats
                    .newest
                    .is_none_or(|newest| now.duration_since(newest) > VERDICT_RETENTION)
        })
        .count();
    // Die abgelaufenen Segmente; `removable` zählt über `walk.live`, ist also
    // nie größer als dessen Länge.
    let expired = walk.live.get(..removable).unwrap_or_default();
    let removed_records = expired
        .iter()
        .fold(0_u64, |sum, stats| sum.saturating_add(stats.records));
    let anchor = match expired.iter().rev().find_map(|stats| stats.last) {
        Some(last_removed) => Some(ChainCursor {
            seq: last_removed
                .seq
                .checked_add(1)
                .ok_or(VerdictStoreError::SequenceExhausted)?,
            prev: last_removed.verdict_digest,
        }),
        None => None,
    };
    let doomed: Vec<&str> = walk
        .retired
        .iter()
        .map(String::as_str)
        .chain(expired.iter().map(|stats| stats.name.as_str()))
        .collect();
    let report = VerdictRetentionReport {
        removed_segments: doomed.len(),
        removed_records,
        first_seq: anchor
            .as_ref()
            .map_or(walk.summary.first_seq, |next| next.seq),
        fault_segment: walk.fault.as_ref().map(|(segment_seq, _)| *segment_seq),
    };
    Ok(RetentionPlan {
        doomed,
        anchor,
        report,
    })
}

// Grenze (`before_seq`) der Retention, die `append_locked` beim Segmentwechsel
// mitlaufen lässt: nur Segmente **vor** dem, das den bisher letzten Datensatz
// der Kette hält, kommen in Frage, und nie das Ziel `seq` des neuen
// Datensatzes (auch nicht als leerer Rest in `walk.live`). Das bisher aktive
// Segment bleibt also stehen, bis der nächste Segmentwechsel oder ein
// expliziter Lauf es entfernt.
//
// Grund: die Jetzt-Zeit ist hier das `created_at` des Aufrufers. Ein einzelner
// Datensatz mit weit vorgestellter Uhr (RTC-Fehler, NTP-Sprung) entfernt so
// höchstens die Segmente davor, nie den jüngsten bisherigen Stand, und eine
// Kette ohne Datensatz verliert gar nichts. Preis: die lebende Kette umfasst
// etwa 91 Tage plus höchstens dieses eine Segment.
fn auto_retention_limit(walk: &Walk, seq: u64) -> u64 {
    walk.live
        .iter()
        .rev()
        .find(|stats| stats.last.is_some())
        .map_or(seq, |stats| stats.first_seq.min(seq))
}

// Läuft die Kette des Store-Verzeichnisses `root` vorwärts ab (siehe
// `walk_listed`); die eine Prüflogik für Leser (`VerdictStore::walk`),
// `append_locked` und `retention_locked`. Nimmt selbst keine Sperre:
// Schreiber rufen sie unter ihrer schon gehaltenen Sperre auf, Leser ohne.
// Reihenfolge: **erst die Segmente listen, dann den Anker lesen.** Die
// Retention schreibt den Anker, bevor sie löscht; der hier gelesene Anker ist
// damit mindestens so neu wie jede Löschung, die die Liste schon zeigt.
// Umgekehrt (alter Anker, ausgedünnte Liste) sähe ein Leser ohne Sperre
// einen falschen Kettenbruch (Test
// `stale_anchor_with_a_fresh_listing_would_look_broken`).
fn walk_root(root: &File, visit: impl FnMut(&VerdictRecord)) -> VerdictStoreResult<Walk> {
    let segments = list_segments(root)?;
    let anchor = read_anchor(root)?;
    walk_listed(root, segments, anchor.as_ref(), visit)
}

// Führt den Leser-Durchlauf `attempt` aus und wiederholt ihn (insgesamt
// höchstens `READ_ATTEMPTS` Durchläufe), solange er mit `Io(NotFound)`
// scheitert. Leser nehmen keine Sperre: zwischen dem Auflisten und dem Öffnen
// eines Segments kann eine Retention (auch die beim Segmentwechsel in
// `append`) es entfernt haben, nachdem sie den Anker geschrieben hat. Unterhalb
// des geöffneten Store-Verzeichnisses entsteht `NotFound` nur beim Öffnen
// eines gelisteten Segments (fehlender Anker und verschwundene Listeneinträge
// gelten als „nicht da“). Jeder neue Durchlauf listet neu und liest den Anker
// neu, sieht also den neuen Stand; ein ohne Anker entferntes Segment meldet er
// als Kettenbruch. Nur für Leser: unter der Schreibsperre verschwindet kein
// Segment, ein `NotFound` dort ist ein echter Fehler.
fn retry_vanished<T>(mut attempt: impl FnMut() -> VerdictStoreResult<T>) -> VerdictStoreResult<T> {
    let mut tries = 1_u32;
    loop {
        match attempt() {
            Err(VerdictStoreError::Io(error))
                if error.kind() == io::ErrorKind::NotFound && tries < READ_ATTEMPTS =>
            {
                tracing::debug!(
                    attempt = tries,
                    "verdict segment vanished during a read; walking again"
                );
                tries = tries.saturating_add(1);
            }
            result => return result,
        }
    }
}

// Nimmt die exklusive Schreibsperre `.lock` in `root`; wartet begrenzt (siehe
// `LOCK_ATTEMPTS`). Ein Symlink an der Stelle von `.lock` wird nie verfolgt
// (`UnsafeDirectory`).
//
// Die Sperre gilt nur, wenn `.lock` nach dem `flock` noch genau die gesperrte
// Inode ist (`lock_is_current`). Hat jemand die Datei inzwischen entfernt oder
// ersetzt (etwa ein dateiweiser Retention-Sweep, der sie nach ihrer mtime für
// alt hält; `fchmod` ändert nur die ctime), hält der Aufrufer sonst eine
// Sperre auf einer verwaisten Inode, während ein anderer Schreiber die neue
// Datei sperrt, und beide hängen dieselbe `seq` an. Dann öffnet
// `acquire_lock` den aktuellen Namen neu und sperrt dort (zählt als Versuch).
// Restrisiko: wird `.lock` entfernt, **während** ein Schreiber sie hält, kann
// ein zweiter die neue Datei sperren; davor schützt `ensure_lock_held` vor
// jeder Änderung (der erste bricht dann ab), ganz ausschließen lässt es sich
// nur, indem niemand außer dieser API Dateien im Store entfernt (Moduldoku
// „Retention“).
fn acquire_lock(root: &File) -> VerdictStoreResult<File> {
    let mut lock = open_lock_file(root)?;
    for attempt in 0..LOCK_ATTEMPTS {
        match FileExt::try_lock(&lock) {
            Ok(()) => {
                if lock_is_current(root, &lock)? {
                    return Ok(lock);
                }
                // Die alte Datei wird beim Ersetzen geschlossen; das gibt
                // ihren `flock` frei.
                tracing::debug!(
                    attempt,
                    "verdict store lock file was replaced; locking it again"
                );
                lock = open_lock_file(root)?;
            }
            Err(fs4::TryLockError::WouldBlock) => {
                if attempt.saturating_add(1) < LOCK_ATTEMPTS {
                    std::thread::sleep(LOCK_BACKOFF);
                }
            }
            Err(fs4::TryLockError::Error(error)) => return Err(VerdictStoreError::Io(error)),
        }
    }
    Err(VerdictStoreError::LockContended)
}

// Ob `.lock` in `root` (ohne Symlink-Folge) noch die gesperrte Datei `lock`
// ist; `false`, wenn der Name fehlt oder auf etwas anderes zeigt.
fn lock_is_current(root: &File, lock: &File) -> VerdictStoreResult<bool> {
    same_file_at(root.as_fd(), Path::new(LOCK_FILE), lock).map_err(VerdictStoreError::Io)
}

// Vor der ersten Änderung eines Schreibers (Segment, Anker, Löschen): ist
// `.lock` seit dem Sperren entfernt oder ersetzt worden, kann ein anderer
// Schreiber inzwischen die neue Datei gesperrt haben. Dann bricht dieser
// Schreiber ab (`LockContended`, nichts geändert); der Aufrufer darf es erneut
// versuchen und sperrt dann die aktuelle Datei. Vorbedingung: `lock` ist von
// `acquire_lock`.
fn ensure_lock_held(root: &File, lock: &File) -> VerdictStoreResult<()> {
    if lock_is_current(root, lock)? {
        return Ok(());
    }
    tracing::warn!("verdict store lock file was replaced while held; write aborted");
    Err(VerdictStoreError::LockContended)
}

// Öffnet (bzw. legt an) `.lock` in `root` ohne Symlink-Folge und setzt die
// Rechte per `fchmod` auf `0600`.
fn open_lock_file(root: &File) -> VerdictStoreResult<File> {
    let lock = open_beneath(
        root.as_fd(),
        Path::new(LOCK_FILE),
        OpenMode {
            read: false,
            write: true,
            create: true,
            create_new: false,
            truncate: false,
            append: false,
            mode: FILE_MODE,
        },
    )
    .map_err(|error| {
        if is_symlink_loop(&error) {
            VerdictStoreError::UnsafeDirectory {
                detail: "lock file is a symbolic link",
            }
        } else {
            VerdictStoreError::Io(error)
        }
    })?;
    lock.set_permissions(Permissions::from_mode(FILE_MODE))?;
    Ok(lock)
}

// Liest `anchor.json` aus `root`; `None`, solange keine Retention gelöscht
// hat. Ein Symlink an seiner Stelle ist `AnchorCorrupt`.
fn read_anchor(root: &File) -> VerdictStoreResult<Option<VerdictAnchor>> {
    let file = match open_beneath(root.as_fd(), Path::new(ANCHOR_FILE), OpenMode::read_only()) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) if is_symlink_loop(&error) => {
            return Err(VerdictStoreError::AnchorCorrupt);
        }
        Err(error) => return Err(VerdictStoreError::Io(error)),
    };
    ensure_private_regular(&file)?;
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_ANCHOR_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_ANCHOR_BYTES {
        return Err(VerdictStoreError::AnchorCorrupt);
    }
    let anchor: VerdictAnchor =
        serde_json::from_slice(&bytes).map_err(|_| VerdictStoreError::AnchorCorrupt)?;
    if anchor.version != ANCHOR_VERSION || anchor.first_seq < FIRST_VERDICT_SEQ {
        return Err(VerdictStoreError::AnchorCorrupt);
    }
    Ok(Some(anchor))
}

// Schreibt `anchor.json` in `root` atomar: Tempdatei `anchor.json.tmp`
// (`O_CREAT | O_EXCL | O_NOFOLLOW`, `0600` per `fchmod`), `write_all`,
// `fsync`, dann `renameat` auf den endgültigen Namen und `fsync` des
// Verzeichnisses. Ein Rest der Tempdatei aus einem abgebrochenen Lauf (auch
// ein Symlink) wird vorher entfernt, nie verfolgt; das ist sicher, weil nur
// Schreiber unter der Sperre diesen Namen benutzen. `renameat` ersetzt ein
// vorhandenes `anchor.json` (auch einen Symlink) selbst, nie dessen Ziel.
// Vorbedingung: die Sperre in `root` ist gehalten.
fn write_anchor(root: &File, anchor: &VerdictAnchor) -> VerdictStoreResult<()> {
    let bytes = serde_json::to_vec(anchor)?;
    let temp = Path::new(ANCHOR_TEMP_FILE);
    match unlink_at(root.as_fd(), temp) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(VerdictStoreError::Io(error)),
    }
    let written = open_beneath(root.as_fd(), temp, OpenMode::write_create_new(FILE_MODE))
        .and_then(|mut file| {
            file.set_permissions(Permissions::from_mode(FILE_MODE))?;
            file.write_all(&bytes)?;
            file.sync_all()
        })
        .and_then(|()| rename_at(root.as_fd(), temp, Path::new(ANCHOR_FILE)));
    if let Err(error) = written {
        // Aufräumen nach bestem Bemühen: der ursprüngliche Fehler zählt, und
        // der nächste Schreiber entfernt einen Rest ohnehin vorher.
        if let Err(cleanup) = unlink_at(root.as_fd(), temp)
            && cleanup.kind() != io::ErrorKind::NotFound
        {
            tracing::debug!(error = %cleanup, "verdict anchor temp file not removed");
        }
        return Err(VerdictStoreError::Io(error));
    }
    root.sync_all()?;
    Ok(())
}

// Alle Segmente in `root`, aufsteigend nach erster `seq`. Fremde Namen
// werden ignoriert; ein Segmentname auf etwas anderem als einer regulären
// Datei (Symlink, Verzeichnis, FIFO …) ist ein Fehler. Gelistet wird per
// `readdir` auf einer Kopie des Deskriptors, geprüft per `fstatat` ohne
// Symlink-Folge relativ zu `root`. Ein zwischen Auflisten und Prüfen
// verschwundener Eintrag (eine gleichzeitige Retention) gilt als nie
// gelistet.
fn list_segments(root: &File) -> VerdictStoreResult<Vec<Segment>> {
    let mut segments = Vec::new();
    for entry in read_dir_names(root.as_fd())? {
        let Some(name) = entry.to_str() else {
            continue;
        };
        let Some(first_seq) = parse_segment_name(name) else {
            continue;
        };
        match entry_type_at(root.as_fd(), Path::new(name))? {
            Some(EntryType::File) => {}
            None => continue,
            Some(_) => return Err(VerdictStoreError::UnsafeSegment),
        }
        segments.push(Segment {
            first_seq,
            name: name.to_owned(),
        });
    }
    segments.sort_by_key(|segment| segment.first_seq);
    Ok(segments)
}

// Öffnet `state_dir`. Es ist vom Aufrufer vorgegeben und vertraut und darf
// ein Symlink sein (etwa `~/.harw` → `/data/harw`); die Symlink-Grenze
// beginnt darunter. Geöffnet wird `<state_dir>/.`: die Auflösung von `.`
// verlangt ein Verzeichnis, ein FIFO oder Gerät an dieser Stelle scheitert
// also mit `ENOTDIR`, bevor es geöffnet wird (kein Blockieren, kein
// Wettlauf zwischen einer Typprüfung und dem Öffnen); Symlinks im vertrauten
// Pfad werden dabei wie gewollt aufgelöst. Danach prüft `fstat` den Typ am
// Deskriptor noch einmal. `None`, wenn es fehlt (auch für einen leeren Pfad,
// der sonst als `.` das Arbeitsverzeichnis öffnete).
fn open_state_dir(state_dir: &Path) -> VerdictStoreResult<Option<File>> {
    if state_dir.as_os_str().is_empty() {
        return Ok(None);
    }
    let state = match File::open(state_dir.join(".")) {
        Ok(state) => state,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotADirectory => {
            return Err(VerdictStoreError::UnsafeDirectory {
                detail: STATE_DIR_NOT_A_DIRECTORY,
            });
        }
        Err(error) => return Err(VerdictStoreError::Io(error)),
    };
    if !state.metadata()?.is_dir() {
        return Err(VerdictStoreError::UnsafeDirectory {
            detail: STATE_DIR_NOT_A_DIRECTORY,
        });
    }
    Ok(Some(state))
}

// Wie `open_state_dir`, legt `state_dir` aber an, falls es fehlt (fehlende
// Glieder `0700`; das Elternverzeichnis jedes neuen Glieds wird
// synchronisiert, bevor etwas quittiert wird). Legt ein anderer Prozess die
// Glieder gleichzeitig an, synchronisiert er sie selbst.
fn create_state_dir(state_dir: &Path) -> VerdictStoreResult<File> {
    let missing = missing_directories(state_dir)?;
    if !missing.is_empty() {
        DirBuilder::new()
            .recursive(true)
            .mode(DIR_MODE)
            .create(state_dir)?;
        for created in &missing {
            sync_directory(parent_or_current(created))?;
        }
    }
    open_state_dir(state_dir)?
        .ok_or_else(|| VerdictStoreError::Io(io::Error::from(io::ErrorKind::NotFound)))
}

// Öffnet das Glied `name` direkt unter `parent` ohne Symlink-Folge
// (`open_dir_at`: `O_DIRECTORY | O_NOFOLLOW`, ein Nicht-Verzeichnis wie ein
// FIFO oder Gerät wird gar nicht erst geöffnet) und prüft es
// (`verified_store_dir`). `None`, wenn es fehlt.
fn open_store_dir(parent: &File, name: &str) -> VerdictStoreResult<Option<File>> {
    match open_dir_at(parent.as_fd(), Path::new(name)) {
        Ok(dir) => verified_store_dir(dir).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(map_dir_open_error(error)),
    }
}

// Legt `name` direkt unter `parent` an, falls dort noch kein Eintrag liegt
// (`mkdirat`, `0700`; ein vorhandener Eintrag, auch ein Symlink, bleibt
// unberührt und wird nicht verfolgt, am Ziel eines Symlinks entsteht also
// nichts), synchronisiert bei einem neuen Glied `parent` und öffnet und
// prüft das Glied wie `open_store_dir`.
fn create_store_dir(parent: &File, name: &str) -> VerdictStoreResult<File> {
    if mkdir_at(parent.as_fd(), Path::new(name), DIR_MODE)? {
        parent.sync_all()?;
    }
    let dir = open_dir_at(parent.as_fd(), Path::new(name)).map_err(map_dir_open_error)?;
    verified_store_dir(dir)
}

// Ein Glied der Grenze muss ein Verzeichnis sein, der effektiven uid gehören
// und darf für Gruppe/Andere nicht beschreibbar sein: sonst könnte ein
// Dritter darin Einträge vertauschen, etwa ein anderes eigenes Verzeichnis
// an die Stelle von `verdicts` setzen. Ein zu offenes Verzeichnis wird nicht
// repariert, sondern abgelehnt.
fn verified_store_dir(dir: File) -> VerdictStoreResult<File> {
    match ensure_owned_directory(&dir) {
        Ok(()) => Ok(dir),
        Err(error) => Err(match error.kind() {
            io::ErrorKind::NotADirectory => VerdictStoreError::UnsafeDirectory {
                detail: NOT_A_REAL_DIRECTORY,
            },
            io::ErrorKind::PermissionDenied => VerdictStoreError::UnsafeDirectory {
                detail: NOT_A_PRIVATE_DIRECTORY,
            },
            _ => VerdictStoreError::Io(error),
        }),
    }
}

// Fehler beim Öffnen eines Glieds (`open_dir_at`): ein Symlink (`ELOOP` bzw.
// `ENOTDIR`, je nach Kernel) oder ein Nicht-Verzeichnis (`ENOTDIR`, auch ein
// FIFO oder Gerät) an seiner Stelle ist `UnsafeDirectory`, alles andere I/O.
fn map_dir_open_error(error: io::Error) -> VerdictStoreError {
    if is_symlink_loop(&error) || error.kind() == io::ErrorKind::NotADirectory {
        VerdictStoreError::UnsafeDirectory {
            detail: NOT_A_REAL_DIRECTORY,
        }
    } else {
        VerdictStoreError::Io(error)
    }
}

// Ein Segment auf der Platte.
struct Segment {
    // Erste `seq`, aus dem Dateinamen.
    first_seq: u64,
    // Dateiname im Store-Verzeichnis; geöffnet wird nur relativ zu dessen
    // Deskriptor.
    name: String,
}

// Kennzahlen eines geprüften Segments für die Retention und das Anhängen.
struct SegmentStats {
    // Erste `seq`, aus dem Dateinamen.
    first_seq: u64,
    // Dateiname im Store-Verzeichnis.
    name: String,
    // Anzahl Datensätze.
    records: u64,
    // `created_at` des ersten Datensatzes (Rotation nach Alter).
    first_created: Option<Timestamp>,
    // Jüngstes `created_at` im Segment.
    newest: Option<Timestamp>,
    // Referenz auf den letzten Datensatz.
    last: Option<VerdictRef>,
}

// Ergebnis eines Kettendurchlaufs.
struct Walk {
    // Zusammenfassung der geprüften Kette (bis zu einer etwaigen Verletzung).
    summary: VerdictChainSummary,
    // Vollständig geprüfte Segmente in Reihenfolge; endet vor `fault`.
    live: Vec<SegmentStats>,
    // Dateinamen der Segmente vor dem Anker (aus einer unterbrochenen
    // Retention).
    retired: Vec<String>,
    // Erste Verletzung: erste `seq` (Name) des betroffenen Segments und der
    // inhaltsfreie Fehler. Der Durchlauf endete dort.
    fault: Option<(u64, VerdictStoreError)>,
    // Die Position hinter dem letzten geprüften Datensatz (bzw. Anker oder
    // Genesis): `seq` und `prev_hash` des nächsten Datensatzes. Nur ohne
    // `fault` die Fortsetzung der Kette; `append_locked` schreibt nur dann.
    next: ChainCursor,
}

impl Walk {
    // Ein leerer Durchlauf, dessen Kette bei `first_seq` mit `prev` beginnt.
    fn empty(first_seq: u64, prev: ContentDigest) -> Self {
        Self {
            summary: VerdictChainSummary {
                records: 0,
                first_seq,
                last: None,
            },
            live: Vec::new(),
            retired: Vec::new(),
            fault: None,
            next: ChainCursor {
                seq: first_seq,
                prev,
            },
        }
    }
}

// Position in der Kette: die als Nächstes erwartete `seq` und `prev_hash`.
struct ChainCursor {
    // Erwartete `seq` des nächsten Datensatzes.
    seq: u64,
    // Erwarteter `prev_hash` des nächsten Datensatzes.
    prev: ContentDigest,
}

// Prüft `segments` (aufsteigend nach erster `seq`) gegen `anchor` und ruft
// `visit` für jeden gültigen Datensatz auf. Segmente vor dem Anker werden nur
// als ausgemustert gesammelt, nie gelesen. Ein inhaltlicher Fehler eines
// Segments (`is_chain_fault`) beendet den Durchlauf und steht in
// `Walk::fault`; `Walk::live` enthält dann nur die vollständig geprüften
// Segmente davor. Andere Fehler (I/O, Rechte, Symlinks) brechen ab.
// Ohne Verletzung steht `Walk::next` danach hinter dem letzten Datensatz.
// Segmente werden nur relativ zu `root` geöffnet. Getrennt von `walk_root`,
// damit Tests eine Liste und einen Anker aus verschiedenen Zeitpunkten
// kombinieren können.
fn walk_listed(
    root: &File,
    segments: Vec<Segment>,
    anchor: Option<&VerdictAnchor>,
    mut visit: impl FnMut(&VerdictRecord),
) -> VerdictStoreResult<Walk> {
    let (first_seq, prev) = chain_start(anchor);
    let mut walk = Walk::empty(first_seq, prev);
    let last_position = segments.len().checked_sub(1);
    for (position, segment) in segments.into_iter().enumerate() {
        if segment.first_seq < first_seq {
            walk.retired.push(segment.name);
            continue;
        }
        let segment_seq = segment.first_seq;
        let is_last = Some(position) == last_position;
        match verify_segment(
            root,
            segment,
            is_last,
            &mut walk.next,
            &mut walk.summary,
            &mut visit,
        ) {
            Ok(stats) => walk.live.push(stats),
            Err(error) if is_chain_fault(&error) => {
                walk.fault = Some((segment_seq, error));
                break;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(walk)
}

// Prüft ein Segment (geöffnet relativ zu `root`) ab der Position `cursor`,
// schreibt `cursor` und `summary` fort und liefert die Kennzahlen des
// Segments.
fn verify_segment(
    root: &File,
    segment: Segment,
    is_last: bool,
    cursor: &mut ChainCursor,
    summary: &mut VerdictChainSummary,
    visit: &mut impl FnMut(&VerdictRecord),
) -> VerdictStoreResult<SegmentStats> {
    let records = read_segment(root, &segment.name, segment.first_seq, is_last)?;
    if let Some(first) = records.first()
        && first.seq != segment.first_seq
    {
        return Err(VerdictStoreError::ChainBroken {
            seq: first.seq,
            reason: ChainBreak::SegmentNameMismatch,
        });
    }
    let mut stats = SegmentStats {
        first_seq: segment.first_seq,
        name: segment.name,
        records: 0,
        first_created: records.first().map(|first| first.created_at),
        newest: None,
        last: None,
    };
    for record in &records {
        check_record(record, cursor.seq, cursor.prev)?;
        visit(record);
        cursor.prev = record.record_hash;
        cursor.seq = record
            .seq
            .checked_add(1)
            .ok_or(VerdictStoreError::SequenceExhausted)?;
        stats.records = stats.records.saturating_add(1);
        stats.newest = Some(
            stats
                .newest
                .map_or(record.created_at, |newest| newest.max(record.created_at)),
        );
        stats.last = Some(record.reference());
        summary.records = summary.records.saturating_add(1);
        summary.last = Some(record.reference());
    }
    Ok(stats)
}

// Inhaltliche Fehler eines Segments: sie verorten eine Verletzung der Kette
// (Manipulation, Bitfehler, fremde Datei) und stoppen die Retention nur ab
// diesem Segment. Alles andere (I/O, Rechte, Symlinks) ist kein Befund über
// die Kette und bricht jeden Durchlauf ab.
const fn is_chain_fault(error: &VerdictStoreError) -> bool {
    matches!(
        error,
        VerdictStoreError::ChainBroken { .. }
            | VerdictStoreError::CorruptRecord { .. }
            | VerdictStoreError::SegmentTooLarge { .. }
    )
}

// Die Glieder des vertrauten `state_dir` aufwärts, die noch nicht
// existieren, das oberste zuerst (nur für `create_state_dir`; unterhalb von
// `state_dir` legt der Store per `mkdirat` an). Ein Eintrag beliebiger Art
// (auch ein Symlink) gilt als vorhanden; `open_state_dir` prüft den Typ
// danach selbst.
fn missing_directories(state_dir: &Path) -> VerdictStoreResult<Vec<PathBuf>> {
    let mut missing = Vec::new();
    let mut current = Some(state_dir);
    while let Some(path) = current {
        if path.as_os_str().is_empty() || entry_exists(path)? {
            break;
        }
        missing.push(path.to_path_buf());
        current = path.parent();
    }
    missing.reverse();
    Ok(missing)
}

// Das Elternverzeichnis von `path`; `.` für ein relatives Glied ohne Eltern.
fn parent_or_current(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

// Synchronisiert die Einträge von `directory` über die eine Stelle der
// Crate dafür (`crate::durability`).
fn sync_directory(directory: &Path) -> VerdictStoreResult<()> {
    crate::durability::sync_parent_directory(directory).map_err(|error| match error {
        crate::error::SessionStoreError::Io(error) => VerdictStoreError::Io(error),
        other => VerdictStoreError::Io(io::Error::other(other.to_string())),
    })
}

// Inhalt von `anchor.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerdictAnchor {
    // Formatversion (`ANCHOR_VERSION`).
    version: u32,
    // Erste `seq` der verbleibenden Kette.
    first_seq: u64,
    // `record_hash` des letzten entfernten Datensatzes.
    prev_hash: ContentDigest,
    // Zeitpunkt der Retention.
    anchored_at: Timestamp,
}

// Start der Kette: Anker oder Genesis.
fn chain_start(anchor: Option<&VerdictAnchor>) -> (u64, ContentDigest) {
    anchor.map_or((FIRST_VERDICT_SEQ, genesis_prev_hash()), |anchor| {
        (anchor.first_seq, anchor.prev_hash)
    })
}

// Gibt die Sperre frei; der erste Fehler gewinnt.
fn release<T>(lock: File, result: VerdictStoreResult<T>) -> VerdictStoreResult<T> {
    let unlocked = FileExt::unlock(&lock).map_err(VerdictStoreError::Io);
    match (result, unlocked) {
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

// Prüft und bereinigt die Eingabe, bevor eine Sperre genommen wird.
fn prepare(verdict: &NewVerdict) -> VerdictStoreResult<NewVerdict> {
    if !finding_id_valid(&verdict.finding_id) {
        return Err(VerdictStoreError::FindingIdInvalid);
    }
    Ok(NewVerdict {
        finding_id: verdict.finding_id.clone(),
        record_digest: verdict.record_digest,
        classification: verdict.classification,
        severity: verdict.severity,
        suggested_response: verdict.suggested_response,
        confidence: verdict.confidence,
        rationale: sanitize_rationale(&verdict.rationale),
        model: verdict.model.as_deref().and_then(sanitize_model),
        created_at: verdict.created_at,
    })
}

// Baut den Datensatz und berechnet `record_hash`.
fn seal(seq: u64, prev_hash: ContentDigest, verdict: NewVerdict) -> VerdictRecord {
    let mut record = VerdictRecord {
        seq,
        prev_hash,
        // Platzhalter: `compute_record_hash` liest dieses Feld nicht.
        record_hash: prev_hash,
        finding_id: verdict.finding_id,
        record_digest: verdict.record_digest,
        classification: verdict.classification,
        severity: verdict.severity,
        suggested_response: verdict.suggested_response,
        confidence: verdict.confidence,
        rationale: verdict.rationale,
        model: verdict.model,
        model_authored: true,
        created_at: verdict.created_at,
    };
    record.record_hash = compute_record_hash(&record);
    record
}

// Kodiert einen Datensatz als JSONL-Zeile und prüft das Zeilenlimit.
fn encode_line(record: &VerdictRecord) -> VerdictStoreResult<String> {
    let mut line = serde_json::to_string(record)?;
    line.push('\n');
    if line.len() > MAX_VERDICT_RECORD_BYTES {
        return Err(VerdictStoreError::RecordTooLarge {
            size: line.len(),
            limit: MAX_VERDICT_RECORD_BYTES,
        });
    }
    Ok(line)
}

// BLAKE3 über die kanonische Kodierung: Domäne, dann jedes Feld außer
// `record_hash` in fester Reihenfolge, variable Längen mit u64-BE-Präfix,
// optionale Felder mit Kennbyte 0/1.
fn compute_record_hash(record: &VerdictRecord) -> ContentDigest {
    let mut buf = Vec::with_capacity(512_usize.saturating_add(record.rationale.len()));
    put_bytes(&mut buf, RECORD_HASH_DOMAIN);
    buf.extend_from_slice(&record.seq.to_be_bytes());
    buf.extend_from_slice(record.prev_hash.as_bytes());
    put_bytes(&mut buf, record.finding_id.as_str().as_bytes());
    buf.extend_from_slice(record.record_digest.as_bytes());
    put_bytes(&mut buf, record.classification.as_str().as_bytes());
    put_bytes(&mut buf, record.severity.as_str().as_bytes());
    put_optional(
        &mut buf,
        record
            .suggested_response
            .map(|response| response.as_str().as_bytes()),
    );
    put_optional(
        &mut buf,
        record
            .confidence
            .map(|confidence| confidence_tag(confidence).as_bytes()),
    );
    put_bytes(&mut buf, record.rationale.as_bytes());
    put_optional(&mut buf, record.model.as_deref().map(str::as_bytes));
    buf.push(u8::from(record.model_authored));
    buf.extend_from_slice(&record.created_at.as_nanosecond().to_be_bytes());
    ContentDigest::of(&buf)
}

// Hängt `bytes` mit u64-BE-Längenpräfix an.
fn put_bytes(buf: &mut Vec<u8>, bytes: &[u8]) {
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(bytes);
}

// Hängt ein optionales Feld an: `0` für `None`, sonst `1` und `put_bytes`.
fn put_optional(buf: &mut Vec<u8>, bytes: Option<&[u8]>) {
    match bytes {
        None => buf.push(0),
        Some(bytes) => {
            buf.push(1);
            put_bytes(buf, bytes);
        }
    }
}

// Feste Kennung je `Confidence`-Stufe (dieselbe Form wie deren JSON).
const fn confidence_tag(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::VeryLow => "very_low",
        Confidence::Low => "low",
        Confidence::Medium => "medium",
        Confidence::High => "high",
        Confidence::VeryHigh => "very_high",
    }
}

// Escapt den Modellbezeichner einzeilig und kürzt ihn; leer wird zu `None`.
fn sanitize_model(model: &str) -> Option<String> {
    let escaped = escape_line(model.trim(), MAX_MODEL_CHARS);
    (!escaped.is_empty()).then_some(escaped)
}

// Eine `finding_id` ist nicht leer, begrenzt und sauber.
fn finding_id_valid(finding_id: &FindingId) -> bool {
    let text = finding_id.as_str();
    !text.trim().is_empty() && text.len() <= MAX_FINDING_ID_BYTES && is_clean(text)
}

// Die Invarianten, die `append` für jeden Datensatz herstellt.
fn record_invariants_hold(record: &VerdictRecord) -> bool {
    finding_id_valid(&record.finding_id)
        && record.seq >= FIRST_VERDICT_SEQ
        && record.rationale.len() <= MAX_RATIONALE_BYTES
        && is_clean(&record.rationale)
        && record.model.as_deref().is_none_or(|model| {
            !model.is_empty() && model.chars().count() <= MAX_MODEL_CHARS && is_clean(model)
        })
}

// Prüft einen Datensatz gegen die erwartete Position in der Kette.
fn check_record(
    record: &VerdictRecord,
    expected_seq: u64,
    expected_prev: ContentDigest,
) -> VerdictStoreResult<()> {
    let reason = if record.seq != expected_seq {
        Some(ChainBreak::SequenceMismatch {
            expected: expected_seq,
        })
    } else if record.prev_hash != expected_prev {
        Some(ChainBreak::PrevHashMismatch)
    } else {
        seal_violation(record)
    };
    match reason {
        Some(reason) => Err(VerdictStoreError::ChainBroken {
            seq: record.seq,
            reason,
        }),
        None => Ok(()),
    }
}

// Was an einem Datensatz selbst (ohne Nachbarn) nicht stimmt.
fn seal_violation(record: &VerdictRecord) -> Option<ChainBreak> {
    if compute_record_hash(record) != record.record_hash {
        Some(ChainBreak::RecordHashMismatch)
    } else if !record.model_authored {
        Some(ChainBreak::NotModelAuthored)
    } else if !record_invariants_hold(record) {
        Some(ChainBreak::InvariantViolated)
    } else {
        None
    }
}

// `verdicts-<seq, 20 Stellen>.jsonl`.
fn segment_file_name(first_seq: u64) -> String {
    format!(
        "{SEGMENT_PREFIX}{first_seq:0width$}{SEGMENT_SUFFIX}",
        width = SEGMENT_DIGITS
    )
}

// Umkehrung von `segment_file_name`; `None` für fremde Namen.
fn parse_segment_name(name: &str) -> Option<u64> {
    let digits = name
        .strip_prefix(SEGMENT_PREFIX)?
        .strip_suffix(SEGMENT_SUFFIX)?;
    if digits.len() != SEGMENT_DIGITS || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits
        .parse::<u64>()
        .ok()
        .filter(|seq| *seq >= FIRST_VERDICT_SEQ)
}

// `true`, wenn an `path` irgendein Eintrag liegt (auch ein Symlink). Nur für
// den vertrauten Teil oberhalb der Grenze (`missing_directories`); darunter
// gilt `entry_type_at` relativ zum Deskriptor.
fn entry_exists(path: &Path) -> VerdictStoreResult<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(VerdictStoreError::Io(error)),
    }
}

// Ein Symlink als letztes Pfadglied wird zu `UnsafeSegment`.
fn map_open_error(error: io::Error) -> VerdictStoreError {
    if is_symlink_loop(&error) {
        VerdictStoreError::UnsafeSegment
    } else {
        VerdictStoreError::Io(error)
    }
}

// Öffnet das Segment `name` in `root` lesend und anhängend (anlegen mit
// `0600`, ohne Symlink-Folge), setzt die Rechte per `fchmod` auf `0600` und
// prüft Eigentümer und Typ.
fn open_segment_append(root: &File, name: &str) -> VerdictStoreResult<File> {
    let file = open_beneath(
        root.as_fd(),
        Path::new(name),
        OpenMode {
            read: true,
            ..OpenMode::append_create(FILE_MODE)
        },
    )
    .map_err(map_open_error)?;
    file.set_permissions(Permissions::from_mode(FILE_MODE))?;
    ensure_private_regular(&file)?;
    Ok(file)
}

// Öffnet das Segment `name` in `root` nur lesend (ohne Symlink-Folge) und
// prüft `0600`, Eigentümer und Typ.
fn open_segment_read(root: &File, name: &str) -> VerdictStoreResult<File> {
    let file = open_beneath(root.as_fd(), Path::new(name), OpenMode::read_only())
        .map_err(map_open_error)?;
    ensure_private_regular(&file)?;
    Ok(file)
}

// Liest das Segment `name` aus `root` ganz (begrenzt) und dekodiert alle
// vollständigen Zeilen.
fn read_segment(
    root: &File,
    name: &str,
    segment_seq: u64,
    allow_torn_tail: bool,
) -> VerdictStoreResult<Vec<VerdictRecord>> {
    let file = open_segment_read(root, name)?;
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_SEGMENT_READ_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if size > MAX_SEGMENT_READ_BYTES {
        return Err(VerdictStoreError::SegmentTooLarge {
            size,
            limit: MAX_SEGMENT_READ_BYTES,
        });
    }
    parse_segment(&bytes, segment_seq, allow_torn_tail)
}

// Zerlegt Segmentbytes in Datensätze. Eine Zeile ohne abschließendes `\n`
// ist nur im letzten Segment zulässig (nie quittiert) und wird übersprungen.
fn parse_segment(
    bytes: &[u8],
    segment_seq: u64,
    allow_torn_tail: bool,
) -> VerdictStoreResult<Vec<VerdictRecord>> {
    let corrupt_tail = VerdictStoreError::CorruptRecord {
        segment_seq,
        line: 0,
    };
    let complete: &[u8] = match bytes.iter().rposition(|byte| *byte == b'\n') {
        Some(end) => {
            let (complete, torn) = bytes.split_at_checked(end.saturating_add(1)).ok_or(
                VerdictStoreError::CorruptRecord {
                    segment_seq,
                    line: 0,
                },
            )?;
            if !torn.is_empty() && !allow_torn_tail {
                return Err(corrupt_tail);
            }
            complete
        }
        None => {
            if !bytes.is_empty() && !allow_torn_tail {
                return Err(corrupt_tail);
            }
            &[]
        }
    };
    let mut records = Vec::new();
    let Some(body) = complete.strip_suffix(b"\n") else {
        return Ok(records);
    };
    for (index, line) in body.split(|byte| *byte == b'\n').enumerate() {
        let line_no = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
        records.push(decode_line(line, segment_seq, line_no)?);
    }
    Ok(records)
}

// Dekodiert eine Zeile (ohne `\n`); Fehler bleiben inhaltsfrei.
fn decode_line(line: &[u8], segment_seq: u64, line_no: u64) -> VerdictStoreResult<VerdictRecord> {
    if line.len().saturating_add(1) > MAX_VERDICT_RECORD_BYTES {
        return Err(VerdictStoreError::CorruptRecord {
            segment_seq,
            line: line_no,
        });
    }
    serde_json::from_slice(line).map_err(|_| VerdictStoreError::CorruptRecord {
        segment_seq,
        line: line_no,
    })
}

// Größtes Fenster, das eine vollständige Zeile samt vorangehendem `\n` fasst.
fn record_window() -> u64 {
    u64::try_from(MAX_VERDICT_RECORD_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1)
}

// Liest die letzten `window` Bytes von `file` ab `start`.
fn read_window(file: &mut File, start: u64, window: u64) -> VerdictStoreResult<Vec<u8>> {
    let size = usize::try_from(window).map_err(|_| {
        VerdictStoreError::Io(io::Error::other("verdict read window exceeds usize"))
    })?;
    let mut buffer = vec![0_u8; size];
    file.seek(SeekFrom::Start(start))?;
    file.read_exact(&mut buffer)?;
    Ok(buffer)
}

// Schneidet eine abgerissene letzte Zeile ab (Absturz mitten im Schreiben;
// nie quittiert). Vorbedingung: die Schreibsperre ist gehalten.
fn repair_torn_tail(file: &mut File, segment_seq: u64) -> VerdictStoreResult<()> {
    let len = file.metadata()?.len();
    let Some(last_index) = len.checked_sub(1) else {
        return Ok(());
    };
    if read_window(file, last_index, 1)? == [b'\n'] {
        return Ok(());
    }
    let window = len.min(record_window());
    let start = len.saturating_sub(window);
    let buffer = read_window(file, start, window)?;
    let keep = match buffer.iter().rposition(|byte| *byte == b'\n') {
        Some(index) => start
            .saturating_add(u64::try_from(index).unwrap_or(u64::MAX))
            .saturating_add(1),
        None if start == 0 => 0,
        // Länger als jede Zeile, die dieser Store schreibt: nicht von uns.
        None => {
            return Err(VerdictStoreError::CorruptRecord {
                segment_seq,
                line: 0,
            });
        }
    };
    file.set_len(keep)?;
    file.sync_all()?;
    tracing::warn!(
        segment = segment_seq,
        removed_bytes = len.saturating_sub(keep),
        "torn verdict segment tail removed before append"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::fs::OpenOptions;
    use std::os::unix::fs::{MetadataExt, symlink};
    use std::time::SystemTime;

    // Feste Basiszeit (2030-03-17), unabhängig von der Wanduhr.
    const BASE_SECOND: i64 = 1_900_000_000;

    fn at(second: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(second).map_err(ctx("timestamp"))
    }

    fn days_before(now: Timestamp, days: i64) -> TestResult<Timestamp> {
        now.checked_sub(SignedDuration::from_secs(days.saturating_mul(86_400)))
            .map_err(ctx("timestamp arithmetic"))
    }

    fn finding(name: &str) -> TestResult<FindingId> {
        FindingId::try_from_str(name).map_err(ctx("finding id"))
    }

    fn verdict(finding_id: &FindingId, rationale: &str, created_at: Timestamp) -> NewVerdict {
        NewVerdict {
            finding_id: finding_id.clone(),
            record_digest: ContentDigest::of(finding_id.as_str().as_bytes()),
            classification: StoredClassification::Suspicious,
            severity: StoredSeverity::High,
            suggested_response: Some(StoredResponse::Monitor),
            confidence: Some(Confidence::Medium),
            rationale: rationale.to_owned(),
            model: Some("local/triage-model".to_owned()),
            created_at,
        }
    }

    // Store mit `count` Ein-Datensatz-Segmenten; `old` davon 100 Tage alt,
    // der Rest einen Tag alt. Ohne Retention beim Anhängen, damit die alten
    // Segmente für `apply_retention` liegen bleiben.
    fn store_with_segments(
        temp: &Path,
        old: usize,
        count: usize,
        now: Timestamp,
    ) -> TestResult<VerdictStore> {
        let store = VerdictStore::new(temp)
            .with_segment_limit(1)
            .without_auto_retention();
        let finding_a = finding("finding-a")?;
        let old_at = days_before(now, 100)?;
        let recent_at = days_before(now, 1)?;
        for index in 0..count {
            let created_at = if index < old { old_at } else { recent_at };
            store
                .append(&verdict(
                    &finding_a,
                    &format!("Begründung {index}"),
                    created_at,
                ))
                .map_err(ctx("append"))?;
        }
        Ok(store)
    }

    // Überschreibt die Begründung im (einzigen) Datensatz eines Segments,
    // ohne neu zu versiegeln: der `record_hash` passt danach nicht mehr.
    fn tamper_segment(path: &Path, from: &str, to: &str) -> TestResult {
        let text = std::fs::read_to_string(path)?;
        let tampered = text.replacen(from, to, 1);
        if tampered == text {
            return Err(TestError::Missing("text to tamper with"));
        }
        std::fs::write(path, tampered)?;
        Ok(())
    }

    // Das Store-Verzeichnis als Deskriptor, so wie jeder öffentliche Aufruf
    // es öffnet (Glied für Glied, ohne Symlink-Folge).
    fn root_of(store: &VerdictStore) -> TestResult<File> {
        store
            .open_root()
            .map_err(ctx("open store root"))?
            .ok_or(TestError::Missing("store root"))
    }

    // Die Segmente als (erste `seq`, Pfad), aufsteigend; leer ohne Store.
    fn listed_segments(store: &VerdictStore) -> TestResult<Vec<(u64, PathBuf)>> {
        let Some(root) = store.open_root().map_err(ctx("open store root"))? else {
            return Ok(Vec::new());
        };
        Ok(list_segments(&root)
            .map_err(ctx("list segments"))?
            .into_iter()
            .map(|segment| (segment.first_seq, store.root().join(segment.name)))
            .collect())
    }

    fn segment_paths(store: &VerdictStore) -> TestResult<Vec<PathBuf>> {
        Ok(listed_segments(store)?
            .into_iter()
            .map(|(_, path)| path)
            .collect())
    }

    // Legt `<state_dir>/dod/verdicts` vorab an, wie es ein früherer Lauf
    // getan hätte (`0700` abzüglich umask, also nie für Gruppe/Andere
    // beschreibbar).
    fn precreate_root(store: &VerdictStore) -> TestResult {
        DirBuilder::new()
            .recursive(true)
            .mode(DIR_MODE)
            .create(store.root())?;
        Ok(())
    }

    // Erwartet `UnsafeDirectory` (beliebiger Grund).
    fn expect_unsafe_directory<T: fmt::Debug>(result: VerdictStoreResult<T>) -> TestResult {
        match result {
            Err(VerdictStoreError::UnsafeDirectory { .. }) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    // Anzahl der Einträge eines Verzeichnisses (per Pfad, nur im Test).
    fn entry_count(dir: &Path) -> TestResult<usize> {
        Ok(std::fs::read_dir(dir)?.count())
    }

    fn only_segment(store: &VerdictStore) -> TestResult<PathBuf> {
        let paths = segment_paths(store)?;
        if paths.len() != 1 {
            return Err(TestError::Unexpected(format!(
                "expected exactly one segment, found {}",
                paths.len()
            )));
        }
        paths
            .into_iter()
            .next()
            .ok_or(TestError::Missing("segment"))
    }

    fn mode_of(path: &Path) -> TestResult<u32> {
        Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
    }

    fn parse_lines(text: &str) -> TestResult<Vec<VerdictRecord>> {
        text.lines()
            .map(|line| serde_json::from_str::<VerdictRecord>(line).map_err(TestError::from))
            .collect()
    }

    fn write_lines(path: &Path, records: &[VerdictRecord]) -> TestResult {
        let mut text = String::new();
        for record in records {
            text.push_str(&serde_json::to_string(record)?);
            text.push('\n');
        }
        std::fs::write(path, text)?;
        Ok(())
    }

    // Inhalt jeder Datei in `paths`, um „nichts geändert“ zu prüfen.
    fn snapshot(paths: &[PathBuf]) -> TestResult<Vec<Vec<u8>>> {
        paths
            .iter()
            .map(|path| std::fs::read(path).map_err(TestError::from))
            .collect()
    }

    // Name, mtime, Rechte und Inhalt (nur reguläre Dateien) jedes Eintrags
    // von `dir`, nach Name sortiert, und davor `dir` selbst (leerer Name,
    // mtime, Rechte). Gleich vor und nach einem Aufruf heißt: er hat in `dir`
    // nichts angelegt, entfernt, umbenannt, beschrieben oder per `fchmod`
    // geändert.
    fn directory_state(dir: &Path) -> TestResult<Vec<(String, SystemTime, u32, Vec<u8>)>> {
        let own = std::fs::metadata(dir)?;
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let meta = std::fs::symlink_metadata(entry.path())?;
            let bytes = if meta.is_file() {
                std::fs::read(entry.path())?
            } else {
                Vec::new()
            };
            entries.push((
                entry.file_name().to_string_lossy().into_owned(),
                meta.modified()?,
                meta.mode() & 0o7777,
                bytes,
            ));
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        let mut state = vec![(
            String::new(),
            own.modified()?,
            own.mode() & 0o7777,
            Vec::new(),
        )];
        state.extend(entries);
        Ok(state)
    }

    // Erwartet `ChainBroken { seq, reason }` aus `verify_chain` oder `append`.
    fn expect_chain_broken<T: fmt::Debug>(
        result: VerdictStoreResult<T>,
        seq: u64,
        reason: ChainBreak,
    ) -> TestResult {
        match result {
            Err(VerdictStoreError::ChainBroken {
                seq: actual_seq,
                reason: actual_reason,
            }) if actual_seq == seq && actual_reason == reason => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn genesis_prev_hash_is_derived_and_never_zero() -> TestResult {
        let genesis = genesis_prev_hash();
        assert_ne!(genesis.as_bytes(), &[0_u8; 32]);
        assert_eq!(genesis, ContentDigest::of(GENESIS_DOMAIN));
        assert_ne!(genesis, ContentDigest::of(VERDICT_CHAIN_DOMAIN.as_bytes()));
        // Beide Domänen entstehen aus demselben Literal.
        assert_eq!(
            GENESIS_DOMAIN,
            format!("{VERDICT_CHAIN_DOMAIN}/genesis").as_bytes()
        );
        assert_eq!(
            genesis,
            ContentDigest::of(format!("{VERDICT_CHAIN_DOMAIN}/genesis").as_bytes())
        );
        Ok(())
    }

    #[test]
    fn valid_verdict_is_appended_and_read_back_after_reopen() -> TestResult {
        let temp = tempfile::tempdir()?;
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        let first = VerdictStore::new(temp.path())
            .append(&verdict(&finding_a, "erste Begründung", now))
            .map_err(ctx("append first"))?;

        // Ein neuer Handle (wie nach einem Neustart) setzt die Kette fort.
        let store = VerdictStore::new(temp.path());
        let second = store
            .append(&verdict(&finding_a, "zweite Begründung", now))
            .map_err(ctx("append second"))?;
        assert_eq!(first.seq, FIRST_VERDICT_SEQ);
        assert_eq!(second.seq, 2);

        let summary = store.verify_chain().map_err(ctx("verify"))?;
        assert_eq!(summary.records, 2);
        assert_eq!(summary.first_seq, FIRST_VERDICT_SEQ);
        assert_eq!(summary.last, Some(second));

        let records = store.records().map_err(ctx("records"))?;
        let stored_first = records.first().ok_or(TestError::Missing("first record"))?;
        assert_eq!(stored_first.prev_hash(), genesis_prev_hash());
        assert_eq!(stored_first.reference(), first);
        assert!(stored_first.model_authored());
        assert_eq!(stored_first.rationale(), "erste Begründung");
        assert_eq!(stored_first.model(), Some("local/triage-model"));
        assert_eq!(stored_first.confidence(), Some(Confidence::Medium));
        assert_eq!(stored_first.severity(), StoredSeverity::High);
        assert_eq!(
            stored_first.record_digest(),
            ContentDigest::of(finding_a.as_str().as_bytes())
        );
        let stored_second = records.get(1).ok_or(TestError::Missing("second record"))?;
        assert_eq!(stored_second.prev_hash(), first.verdict_digest);
        assert_eq!(stored_second.record_hash(), second.verdict_digest);
        Ok(())
    }

    #[test]
    fn confirmed_is_stored_as_model_opinion_only() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let mut input = verdict(&finding_a, "eindeutig", at(BASE_SECOND)?);
        input.classification = StoredClassification::Confirmed;
        input.suggested_response = Some(StoredResponse::Contain);
        store.append(&input).map_err(ctx("append"))?;

        let record = store
            .latest_for(&finding_a)
            .map_err(ctx("latest"))?
            .ok_or(TestError::Missing("record"))?;
        assert_eq!(record.classification(), StoredClassification::Confirmed);
        assert_eq!(record.suggested_response(), Some(StoredResponse::Contain));
        assert!(record.model_authored());
        Ok(())
    }

    #[test]
    fn latest_for_returns_the_newest_record_of_that_finding() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let finding_b = finding("finding-b")?;
        let missing = finding("finding-c")?;
        let now = at(BASE_SECOND)?;
        store
            .append(&verdict(&finding_a, "a eins", now))
            .map_err(ctx("append a1"))?;
        let b = store
            .append(&verdict(&finding_b, "b eins", now))
            .map_err(ctx("append b1"))?;
        let a2 = store
            .append(&verdict(&finding_a, "a zwei", now))
            .map_err(ctx("append a2"))?;

        let latest_a = store
            .latest_for(&finding_a)
            .map_err(ctx("latest a"))?
            .ok_or(TestError::Missing("latest a"))?;
        assert_eq!(latest_a.reference(), a2);
        assert_eq!(latest_a.rationale(), "a zwei");
        let latest_b = store
            .latest_for(&finding_b)
            .map_err(ctx("latest b"))?
            .ok_or(TestError::Missing("latest b"))?;
        assert_eq!(latest_b.reference(), b);
        assert!(
            store
                .latest_for(&missing)
                .map_err(ctx("latest c"))?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn empty_store_verifies_and_has_no_records() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let summary = store.verify_chain().map_err(ctx("verify"))?;
        assert_eq!(summary.records, 0);
        assert_eq!(summary.first_seq, FIRST_VERDICT_SEQ);
        assert_eq!(summary.last, None);
        assert!(!store.root().exists());
        Ok(())
    }

    #[test]
    fn modified_record_is_detected() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        for rationale in ["erste Begründung", "zweite Begründung", "dritte Begründung"] {
            store
                .append(&verdict(&finding_a, rationale, now))
                .map_err(ctx("append"))?;
        }
        let path = only_segment(&store)?;
        let text = std::fs::read_to_string(&path)?;
        let tampered = text.replacen("zweite", "gefälschte", 1);
        assert_ne!(tampered, text);
        std::fs::write(&path, tampered)?;

        expect_chain_broken(store.verify_chain(), 2, ChainBreak::RecordHashMismatch)?;
        assert!(matches!(
            store.latest_for(&finding_a),
            Err(VerdictStoreError::ChainBroken { .. })
        ));
        Ok(())
    }

    #[test]
    fn resealed_modification_breaks_the_next_link() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        for rationale in ["eins", "zwei", "drei"] {
            store
                .append(&verdict(&finding_a, rationale, now))
                .map_err(ctx("append"))?;
        }
        let path = only_segment(&store)?;
        let mut records = parse_lines(&std::fs::read_to_string(&path)?)?;
        let second = records.get_mut(1).ok_or(TestError::Missing("second"))?;
        second.classification = StoredClassification::Benign;
        let resealed = compute_record_hash(second);
        second.record_hash = resealed;
        write_lines(&path, &records)?;

        expect_chain_broken(store.verify_chain(), 3, ChainBreak::PrevHashMismatch)
    }

    #[test]
    fn deleted_record_is_detected() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        for rationale in ["eins", "zwei", "drei"] {
            store
                .append(&verdict(&finding_a, rationale, now))
                .map_err(ctx("append"))?;
        }
        let path = only_segment(&store)?;
        let records = parse_lines(&std::fs::read_to_string(&path)?)?;
        let kept: Vec<VerdictRecord> = records
            .into_iter()
            .filter(|record| record.seq() != 2)
            .collect();
        write_lines(&path, &kept)?;

        expect_chain_broken(
            store.verify_chain(),
            3,
            ChainBreak::SequenceMismatch { expected: 2 },
        )
    }

    #[test]
    fn record_without_model_authored_flag_breaks_the_chain() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        store
            .append(&verdict(&finding_a, "eins", at(BASE_SECOND)?))
            .map_err(ctx("append"))?;
        let path = only_segment(&store)?;
        let mut records = parse_lines(&std::fs::read_to_string(&path)?)?;
        let first = records.first_mut().ok_or(TestError::Missing("first"))?;
        first.model_authored = false;
        let resealed = compute_record_hash(first);
        first.record_hash = resealed;
        write_lines(&path, &records)?;

        expect_chain_broken(store.verify_chain(), 1, ChainBreak::NotModelAuthored)?;
        // Auch das Anhängen verweigert einen solchen Vorgänger.
        assert!(matches!(
            store.append(&verdict(&finding_a, "zwei", at(BASE_SECOND)?)),
            Err(VerdictStoreError::ChainBroken {
                seq: 1,
                reason: ChainBreak::NotModelAuthored
            })
        ));
        Ok(())
    }

    #[test]
    fn append_refuses_to_extend_a_tampered_predecessor() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        store
            .append(&verdict(&finding_a, "original", now))
            .map_err(ctx("append"))?;
        let path = only_segment(&store)?;
        let text = std::fs::read_to_string(&path)?;
        std::fs::write(&path, text.replacen("original", "geändert", 1))?;

        assert!(matches!(
            store.append(&verdict(&finding_a, "neu", now)),
            Err(VerdictStoreError::ChainBroken {
                seq: 1,
                reason: ChainBreak::RecordHashMismatch
            })
        ));
        Ok(())
    }

    #[test]
    fn oversize_rationale_is_capped_on_a_char_boundary() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let long = "ä".repeat(3_000);
        store
            .append(&verdict(&finding_a, &long, at(BASE_SECOND)?))
            .map_err(ctx("append"))?;

        let record = store
            .latest_for(&finding_a)
            .map_err(ctx("latest"))?
            .ok_or(TestError::Missing("record"))?;
        let rationale = record.rationale();
        assert!(rationale.len() <= MAX_RATIONALE_BYTES);
        assert!(rationale.ends_with(TRUNCATION_MARKER));
        let body = rationale
            .strip_suffix(TRUNCATION_MARKER)
            .ok_or(TestError::Missing("truncation marker"))?;
        assert!(!body.is_empty());
        assert!(body.chars().all(|ch| ch == 'ä'));
        Ok(())
    }

    #[test]
    fn control_and_bidi_characters_in_the_rationale_are_escaped() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let hostile = "ok\u{202E}gnp.exe\u{200B}\u{0007}\u{1B}[31m\nFAKE: approved\u{E0041}";
        let mut input = verdict(&finding_a, hostile, at(BASE_SECOND)?);
        input.model = Some("evil\u{202E}model\n".to_owned());
        store.append(&input).map_err(ctx("append"))?;

        let record = store
            .latest_for(&finding_a)
            .map_err(ctx("latest"))?
            .ok_or(TestError::Missing("record"))?;
        let rationale = record.rationale();
        assert!(is_clean(rationale));
        assert!(rationale.contains("\\u{202e}"));
        assert!(rationale.contains("\\u{200b}"));
        assert!(rationale.contains("\\u{0007}"));
        assert!(rationale.contains("\\u{001b}"));
        assert!(rationale.contains("\\u{000a}"));
        assert!(rationale.contains("\\u{e0041}"));
        let model = record.model().ok_or(TestError::Missing("model"))?;
        assert!(is_clean(model));
        assert_eq!(model, "evil\\u{202e}model");
        Ok(())
    }

    #[test]
    fn sanitize_rationale_is_bounded_idempotent_and_clean() -> TestResult {
        assert_eq!(sanitize_rationale("kurz und sauber"), "kurz und sauber");
        assert_eq!(sanitize_rationale(""), "");

        let multibyte = "€".repeat(1_000); // 3000 Bytes
        let capped = sanitize_rationale(&multibyte);
        assert!(capped.len() <= MAX_RATIONALE_BYTES);
        assert!(capped.ends_with(TRUNCATION_MARKER));
        assert_eq!(sanitize_rationale(&capped), capped);

        // Eine Ersatzform wird nie zerschnitten.
        let escapes = "\u{202E}".repeat(1_000);
        let capped_escapes = sanitize_rationale(&escapes);
        assert!(capped_escapes.len() <= MAX_RATIONALE_BYTES);
        let body = capped_escapes
            .strip_suffix(TRUNCATION_MARKER)
            .ok_or(TestError::Missing("truncation marker"))?;
        assert_eq!(body.len().checked_rem("\\u{202e}".len()), Some(0));
        assert!(body.split("\\u{202e}").all(str::is_empty));
        assert!(is_clean(&capped_escapes));

        // Genau passender Text bleibt ungekürzt.
        let exact = "a".repeat(MAX_RATIONALE_BYTES);
        assert_eq!(sanitize_rationale(&exact), exact);
        let over = "a".repeat(MAX_RATIONALE_BYTES.saturating_add(1));
        let capped_over = sanitize_rationale(&over);
        assert!(capped_over.len() <= MAX_RATIONALE_BYTES);
        assert!(capped_over.ends_with(TRUNCATION_MARKER));
        Ok(())
    }

    #[test]
    fn invalid_finding_ids_are_rejected_before_touching_disk() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let now = at(BASE_SECOND)?;
        let too_long = "f".repeat(MAX_FINDING_ID_BYTES.saturating_add(1));
        for raw in ["   ", "finding\u{202E}a", too_long.as_str()] {
            let input = verdict(&FindingId::from_str(raw), "text", now);
            assert!(matches!(
                store.append(&input),
                Err(VerdictStoreError::FindingIdInvalid)
            ));
        }
        assert!(!store.root().exists());
        Ok(())
    }

    #[test]
    fn blank_model_is_stored_as_none() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let mut input = verdict(&finding_a, "text", at(BASE_SECOND)?);
        input.model = Some("   ".to_owned());
        store.append(&input).map_err(ctx("append"))?;
        let record = store
            .latest_for(&finding_a)
            .map_err(ctx("latest"))?
            .ok_or(TestError::Missing("record"))?;
        assert_eq!(record.model(), None);
        Ok(())
    }

    #[test]
    fn record_over_the_size_bound_is_rejected() -> TestResult {
        let record = VerdictRecord {
            seq: FIRST_VERDICT_SEQ,
            prev_hash: genesis_prev_hash(),
            record_hash: genesis_prev_hash(),
            finding_id: FindingId::from_str("f".repeat(MAX_VERDICT_RECORD_BYTES)),
            record_digest: ContentDigest::of(b"record"),
            classification: StoredClassification::Benign,
            severity: StoredSeverity::Info,
            suggested_response: None,
            confidence: None,
            rationale: "text".to_owned(),
            model: None,
            model_authored: true,
            created_at: at(BASE_SECOND)?,
        };
        match encode_line(&record) {
            Err(VerdictStoreError::RecordTooLarge { size, limit }) => {
                assert!(size > limit);
                assert_eq!(limit, MAX_VERDICT_RECORD_BYTES);
                Ok(())
            }
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn largest_valid_input_stays_within_the_record_bound() -> TestResult {
        // Begründung, ID und Modell an ihren Grenzen, mit JSON-Escapes (`"`, `\`).
        let finding_id = FindingId::from_str("\"".repeat(MAX_FINDING_ID_BYTES));
        let mut input = verdict(&finding_id, &"\\".repeat(5_000), at(BASE_SECOND)?);
        input.model = Some("\\".repeat(MAX_MODEL_CHARS.saturating_mul(2)));
        input.severity = StoredSeverity::Critical;
        input.classification = StoredClassification::Suspicious;
        input.suggested_response = Some(StoredResponse::Escalate);
        input.confidence = Some(Confidence::VeryHigh);
        let prepared = prepare(&input).map_err(ctx("prepare"))?;
        let record = seal(u64::MAX, genesis_prev_hash(), prepared);
        let line = encode_line(&record).map_err(ctx("encode"))?;
        assert!(line.len() <= MAX_VERDICT_RECORD_BYTES);
        Ok(())
    }

    #[test]
    fn segments_rotate_at_the_size_limit() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path()).with_segment_limit(1_500);
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        for index in 0..8 {
            store
                .append(&verdict(&finding_a, &format!("Begründung {index}"), now))
                .map_err(ctx("append"))?;
        }
        let segments = listed_segments(&store)?;
        assert!(segments.len() >= 2);
        for (first_seq, path) in &segments {
            assert!(std::fs::metadata(path)?.len() <= 1_500);
            let text = std::fs::read_to_string(path)?;
            let first = parse_lines(&text)?
                .into_iter()
                .next()
                .ok_or(TestError::Missing("first record of segment"))?;
            assert_eq!(first.seq(), *first_seq);
        }
        let summary = store.verify_chain().map_err(ctx("verify"))?;
        assert_eq!(summary.records, 8);
        Ok(())
    }

    #[test]
    fn retention_removes_expired_segments_and_keeps_the_chain_verifiable() -> TestResult {
        let temp = tempfile::tempdir()?;
        // Grenze 1: jedes Segment fasst genau einen Datensatz. Ohne Retention
        // beim Anhängen (die hätte die alten Segmente schon beim fünften
        // Anhängen entfernt; siehe
        // `append_applies_retention_when_it_starts_a_new_segment`).
        let store = VerdictStore::new(temp.path())
            .with_segment_limit(1)
            .without_auto_retention();
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        let old = days_before(now, 100)?;
        let recent = days_before(now, 1)?;
        for created_at in [old, old, old, old, recent, recent] {
            store
                .append(&verdict(&finding_a, "Begründung", created_at))
                .map_err(ctx("append"))?;
        }
        assert_eq!(segment_paths(&store)?.len(), 6);

        let report = store.apply_retention(now).map_err(ctx("retention"))?;
        assert_eq!(report.removed_segments, 4);
        assert_eq!(report.removed_records, 4);
        assert_eq!(report.first_seq, 5);
        assert_eq!(segment_paths(&store)?.len(), 2);

        let summary = store
            .verify_chain()
            .map_err(ctx("verify after retention"))?;
        assert_eq!(summary.first_seq, 5);
        assert_eq!(summary.records, 2);

        // Die Kette lässt sich hinter dem Anker fortsetzen und bleibt prüfbar.
        let next = store
            .append(&verdict(&finding_a, "danach", now))
            .map_err(ctx("append after retention"))?;
        assert_eq!(next.seq, 7);
        let summary = store.verify_chain().map_err(ctx("verify after append"))?;
        assert_eq!(summary.records, 3);
        assert_eq!(summary.last, Some(next));

        // Ein zweiter Lauf ändert nichts.
        let again = store.apply_retention(now).map_err(ctx("retention again"))?;
        assert_eq!(again.removed_segments, 0);
        assert_eq!(again.first_seq, 5);
        Ok(())
    }

    #[test]
    fn retention_keeps_young_segments_and_removes_an_expired_active_one() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path()).with_segment_limit(1);
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        let old = days_before(now, 120)?;
        let young = days_before(now, 89)?;
        // Ein junges Segment vor alten stoppt die Retention (nur ein Präfix).
        for created_at in [young, old, old] {
            store
                .append(&verdict(&finding_a, "Begründung", created_at))
                .map_err(ctx("append"))?;
        }
        let report = store.apply_retention(now).map_err(ctx("retention"))?;
        assert_eq!(report.removed_segments, 0);
        assert_eq!(report.first_seq, FIRST_VERDICT_SEQ);

        // Ein junges aktives (letztes) Segment bleibt.
        let temp_young = tempfile::tempdir()?;
        let store_young = VerdictStore::new(temp_young.path()).with_segment_limit(1);
        for created_at in [old, old, young] {
            store_young
                .append(&verdict(&finding_a, "Begründung", created_at))
                .map_err(ctx("append young"))?;
        }
        let report_young = store_young
            .apply_retention(now)
            .map_err(ctx("retention young"))?;
        assert_eq!(report_young.removed_segments, 2);
        assert_eq!(report_young.first_seq, 3);
        assert_eq!(segment_paths(&store_young)?.len(), 1);

        // Alles alt: auch das aktive (letzte) Segment geht, nur der Anker
        // bleibt; die Kette ist danach leer und prüfbar.
        let temp_old = tempfile::tempdir()?;
        let store_old = VerdictStore::new(temp_old.path()).with_segment_limit(1);
        for _ in 0..3 {
            store_old
                .append(&verdict(&finding_a, "Begründung", old))
                .map_err(ctx("append old"))?;
        }
        let report_old = store_old
            .apply_retention(now)
            .map_err(ctx("retention old"))?;
        assert_eq!(report_old.removed_segments, 3);
        assert_eq!(report_old.removed_records, 3);
        assert_eq!(report_old.first_seq, 4);
        assert_eq!(report_old.fault_segment, None);
        assert!(segment_paths(&store_old)?.is_empty());
        let summary = store_old.verify_chain().map_err(ctx("verify"))?;
        assert_eq!(summary.records, 0);
        assert_eq!(summary.first_seq, 4);
        Ok(())
    }

    #[test]
    fn retention_removes_a_single_expired_segment_and_append_continues_from_the_anchor()
    -> TestResult {
        let temp = tempfile::tempdir()?;
        // Standardgrenzen: ein Host mit wenigen Verdicts, alles in einem
        // Segment, das die 4 MiB nie erreicht.
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        let old = days_before(now, 120)?;
        let mut last = None;
        for rationale in ["eins", "zwei", "drei"] {
            last = Some(
                store
                    .append(&verdict(&finding_a, rationale, old))
                    .map_err(ctx("append"))?,
            );
        }
        let last = last.ok_or(TestError::Missing("last reference"))?;
        assert_eq!(last.seq, 3);
        let path = only_segment(&store)?;

        let report = store.apply_retention(now).map_err(ctx("retention"))?;
        assert_eq!(report.removed_segments, 1);
        assert_eq!(report.removed_records, 3);
        assert_eq!(report.first_seq, 4);
        assert_eq!(report.fault_segment, None);
        assert!(!path.exists());
        assert!(segment_paths(&store)?.is_empty());
        assert_eq!(mode_of(&store.root().join(ANCHOR_FILE))?, 0o600);

        // Leere, verankerte Kette: `first_seq` = letzte `seq` + 1.
        let summary = store.verify_chain().map_err(ctx("verify empty"))?;
        assert_eq!(summary.first_seq, 4);
        assert_eq!(summary.records, 0);
        assert_eq!(summary.last, None);
        assert!(
            store
                .latest_for(&finding_a)
                .map_err(ctx("latest"))?
                .is_none()
        );

        // Das nächste Anhängen setzt die Kette ab dem Anker fort.
        let next = store
            .append(&verdict(&finding_a, "danach", now))
            .map_err(ctx("append after retention"))?;
        assert_eq!(next.seq, 4);
        assert_eq!(
            only_segment(&store)?,
            store.root().join(segment_file_name(4))
        );
        let records = store.records().map_err(ctx("records"))?;
        let first = records.first().ok_or(TestError::Missing("first record"))?;
        assert_eq!(first.prev_hash(), last.verdict_digest);
        let summary = store.verify_chain().map_err(ctx("verify after append"))?;
        assert_eq!(summary.first_seq, 4);
        assert_eq!(summary.records, 1);
        assert_eq!(summary.last, Some(next));

        // Ein zweiter Lauf entfernt das junge Segment nicht.
        let again = store.apply_retention(now).map_err(ctx("retention again"))?;
        assert_eq!(again.removed_segments, 0);
        assert_eq!(again.first_seq, 4);
        Ok(())
    }

    #[test]
    fn segments_rotate_by_age() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let start = at(BASE_SECOND)?;
        let hour = SignedDuration::from_secs(3_600);
        let later = start.checked_add(hour).map_err(ctx("later"))?;
        let next_day = start
            .checked_add(VERDICT_SEGMENT_SPAN)
            .map_err(ctx("next day"))?;
        // Die Uhr läuft zurück: kein Grund für ein neues Segment.
        let earlier = start.checked_sub(hour).map_err(ctx("earlier"))?;
        for created_at in [start, later, next_day, earlier] {
            store
                .append(&verdict(&finding_a, "Begründung", created_at))
                .map_err(ctx("append"))?;
        }

        let segments = listed_segments(&store)?;
        let names: Vec<u64> = segments.iter().map(|(first_seq, _)| *first_seq).collect();
        assert_eq!(names, [1_u64, 3]);
        for (_, path) in &segments {
            let records = parse_lines(&std::fs::read_to_string(path)?)?;
            assert_eq!(records.len(), 2);
        }
        let summary = store.verify_chain().map_err(ctx("verify"))?;
        assert_eq!(summary.records, 4);
        Ok(())
    }

    #[test]
    fn retention_tracks_the_window_on_a_low_volume_store() -> TestResult {
        // Ein Verdict etwa alle 30 Tage: ohne Rotation nach Alter läge alles
        // in einem Segment, dessen jüngster Datensatz nie 90 Tage alt würde.
        // Ohne Retention beim Anhängen; dieselbe Folge mit ihr prüft
        // `append_retention_tracks_the_window_without_apply_retention`.
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path()).without_auto_retention();
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        for days in [150, 120, 95, 60, 30, 0] {
            store
                .append(&verdict(&finding_a, "Begründung", days_before(now, days)?))
                .map_err(ctx("append"))?;
        }
        assert_eq!(segment_paths(&store)?.len(), 6);

        let report = store.apply_retention(now).map_err(ctx("retention"))?;
        assert_eq!(report.removed_segments, 3);
        assert_eq!(report.removed_records, 3);
        assert_eq!(report.first_seq, 4);
        let records = store.records().map_err(ctx("records"))?;
        assert_eq!(records.len(), 3);
        for record in &records {
            assert!(now.duration_since(record.created_at()) <= VERDICT_RETENTION);
        }
        Ok(())
    }

    #[test]
    fn append_applies_retention_when_it_starts_a_new_segment() -> TestResult {
        let temp = tempfile::tempdir()?;
        // Standardgrenzen: drei alte Datensätze in einem Segment, und niemand
        // ruft `apply_retention` auf.
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        let old = days_before(now, 120)?;
        let mut last = None;
        for rationale in ["eins", "zwei", "drei"] {
            last = Some(
                store
                    .append(&verdict(&finding_a, rationale, old))
                    .map_err(ctx("append old"))?,
            );
        }
        let last = last.ok_or(TestError::Missing("last reference"))?;
        let old_segment = only_segment(&store)?;

        // Das nächste Anhängen beginnt nach der Zeitspanne ein neues Segment.
        // Die mitlaufende Retention lässt das bisher aktive Segment stehen,
        // obwohl es abgelaufen ist: nur Segmente davor kommen in Frage.
        let next = store
            .append(&verdict(&finding_a, "vier", now))
            .map_err(ctx("append now"))?;
        assert_eq!(next.seq, 4);
        assert!(old_segment.exists());
        assert!(!store.root().join(ANCHOR_FILE).exists());
        assert_eq!(
            store
                .verify_chain()
                .map_err(ctx("verify after the first rotation"))?
                .records,
            4
        );

        // Ohne Segmentwechsel läuft keine Retention.
        let fifth = store
            .append(&verdict(&finding_a, "fünf", now))
            .map_err(ctx("append into the same segment"))?;
        assert_eq!(fifth.seq, 5);
        assert!(old_segment.exists());

        // Der nächste Segmentwechsel entfernt es (`created_at` als Jetzt);
        // jetzt ist Segment 4 das bisher aktive und bleibt.
        let later = now
            .checked_add(VERDICT_SEGMENT_SPAN)
            .map_err(ctx("later"))?;
        let sixth = store
            .append(&verdict(&finding_a, "sechs", later))
            .map_err(ctx("append later"))?;
        assert_eq!(sixth.seq, 6);
        assert!(!old_segment.exists());
        let names: Vec<u64> = listed_segments(&store)?
            .iter()
            .map(|(first_seq, _)| *first_seq)
            .collect();
        assert_eq!(names, [4_u64, 6]);
        assert_eq!(mode_of(&store.root().join(ANCHOR_FILE))?, 0o600);
        let anchor = read_anchor(&root_of(&store)?)
            .map_err(ctx("anchor"))?
            .ok_or(TestError::Missing("anchor"))?;
        assert_eq!(anchor.first_seq, 4);
        assert_eq!(anchor.prev_hash, last.verdict_digest);
        assert_eq!(anchor.anchored_at, later);
        let summary = store.verify_chain().map_err(ctx("verify"))?;
        assert_eq!(summary.first_seq, 4);
        assert_eq!(summary.records, 3);
        assert_eq!(summary.last, Some(sixth));
        Ok(())
    }

    #[test]
    fn append_retention_after_a_forward_clock_jump_keeps_the_newest_prior_segment() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        // Drei junge Segmente (je ein Tag).
        let mut newest = None;
        for days in [3, 2, 1] {
            newest = Some(
                store
                    .append(&verdict(&finding_a, "Begründung", days_before(now, days)?))
                    .map_err(ctx("append"))?,
            );
        }
        let newest = newest.ok_or(TestError::Missing("newest reference"))?;
        assert_eq!(segment_paths(&store)?.len(), 3);

        // Ein einzelnes Verdict mit einer um zehn Jahre vorgestellten Uhr: aus
        // seiner Sicht ist alles abgelaufen.
        let jump = now
            .checked_add(SignedDuration::from_secs(10 * 365 * 86_400))
            .map_err(ctx("clock jump"))?;
        let jumped = store
            .append(&verdict(&finding_a, "Uhrsprung", jump))
            .map_err(ctx("append with a jumped clock"))?;
        assert_eq!(jumped.seq, 4);

        // Entfernt wurden nur die Segmente vor dem bisher aktiven; der jüngste
        // bisherige Stand (Segment 3) bleibt und verknüpft weiter.
        let names: Vec<u64> = listed_segments(&store)?
            .iter()
            .map(|(first_seq, _)| *first_seq)
            .collect();
        assert_eq!(names, [3_u64, 4]);
        let records = store.records().map_err(ctx("records"))?;
        assert_eq!(records.len(), 2);
        let first = records.first().ok_or(TestError::Missing("record 3"))?;
        assert_eq!(first.reference(), newest);
        assert_eq!(first.created_at(), days_before(now, 1)?);
        let summary = store.verify_chain().map_err(ctx("verify"))?;
        assert_eq!(summary.first_seq, 3);
        assert_eq!(summary.last, Some(jumped));
        Ok(())
    }

    #[test]
    fn append_retention_tracks_the_window_without_apply_retention() -> TestResult {
        // Dieselbe Folge wie `retention_tracks_the_window_on_a_low_volume_store`,
        // aber nur mit `append`: jedes Anhängen beginnt ein neues Segment und
        // entfernt dabei, was mehr als 90 Tage vor seinem `created_at` liegt.
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        for days in [150, 120, 95, 60, 30, 0] {
            store
                .append(&verdict(&finding_a, "Begründung", days_before(now, days)?))
                .map_err(ctx("append"))?;
        }
        let names: Vec<u64> = listed_segments(&store)?
            .iter()
            .map(|(first_seq, _)| *first_seq)
            .collect();
        assert_eq!(names, [4_u64, 5, 6]);
        let summary = store.verify_chain().map_err(ctx("verify"))?;
        assert_eq!(summary.first_seq, 4);
        assert_eq!(summary.records, 3);
        for record in &store.records().map_err(ctx("records"))? {
            assert!(now.duration_since(record.created_at()) <= VERDICT_RETENTION);
        }
        // Ein expliziter Lauf findet danach nichts mehr.
        let report = store.apply_retention(now).map_err(ctx("retention"))?;
        assert_eq!(report.removed_segments, 0);
        assert_eq!(report.first_seq, 4);
        Ok(())
    }

    #[test]
    fn append_keeps_the_record_when_retention_after_rotation_fails() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        // Zwei alte Segmente: beim nächsten Segmentwechsel ist das ältere ein
        // Kandidat (das jüngere bleibt als bisher aktives stehen).
        store
            .append(&verdict(&finding_a, "älter", days_before(now, 200)?))
            .map_err(ctx("append older"))?;
        store
            .append(&verdict(&finding_a, "alt", days_before(now, 120)?))
            .map_err(ctx("append old"))?;
        let segments = segment_paths(&store)?;
        let older_segment = segments
            .first()
            .ok_or(TestError::Missing("older segment"))?
            .clone();
        let old_segment = segments
            .get(1)
            .ok_or(TestError::Missing("old segment"))?
            .clone();
        // Ein Verzeichnis an der Stelle der Anker-Tempdatei: `unlinkat`
        // scheitert, die Retention bricht vor dem Anker ab.
        let obstacle = store.root().join(ANCHOR_TEMP_FILE);
        DirBuilder::new().mode(DIR_MODE).create(&obstacle)?;

        let next = store
            .append(&verdict(&finding_a, "neu", now))
            .map_err(ctx("append despite a failed retention"))?;
        assert_eq!(next.seq, 3);
        // Der Datensatz ist quittiert; nichts gelöscht, nichts verankert.
        assert!(older_segment.exists());
        assert!(old_segment.exists());
        assert!(!store.root().join(ANCHOR_FILE).exists());
        let summary = store.verify_chain().map_err(ctx("verify"))?;
        assert_eq!(summary.records, 3);
        assert_eq!(summary.last, Some(next));

        // Ohne das Hindernis holt der nächste Segmentwechsel sie nach (jetzt
        // ist Segment 3 das bisher aktive).
        std::fs::remove_dir(&obstacle)?;
        let later = now
            .checked_add(VERDICT_SEGMENT_SPAN)
            .map_err(ctx("later"))?;
        let fourth = store
            .append(&verdict(&finding_a, "später", later))
            .map_err(ctx("append later"))?;
        assert_eq!(fourth.seq, 4);
        assert!(!older_segment.exists());
        assert!(!old_segment.exists());
        let summary = store.verify_chain().map_err(ctx("verify after catch-up"))?;
        assert_eq!(summary.first_seq, 3);
        assert_eq!(summary.records, 2);
        Ok(())
    }

    #[test]
    fn append_retention_never_removes_the_segment_it_just_wrote() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        let first = store
            .append(&verdict(&finding_a, "älter", days_before(now, 200)?))
            .map_err(ctx("append older"))?;
        let previous = store
            .append(&verdict(&finding_a, "alt", days_before(now, 120)?))
            .map_err(ctx("append old"))?;
        assert_eq!(previous.seq, 2);
        // Zwei leere Reste gescheiterter Anhängevorgänge: `seq` 3 (das Ziel
        // des nächsten Datensatzes, als leeres Segment schon „abgelaufen“)
        // und `seq` 7 (zuletzt gelistet, also kein Kandidat zum Fortsetzen;
        // das nächste Anhängen rotiert deshalb und verwendet Segment 3 als
        // Ziel wieder).
        for seq in [3, 7] {
            let path = store.root().join(segment_file_name(seq));
            std::fs::write(&path, b"")?;
            std::fs::set_permissions(&path, Permissions::from_mode(FILE_MODE))?;
        }

        let third = store
            .append(&verdict(&finding_a, "neu", now))
            .map_err(ctx("append"))?;
        assert_eq!(third.seq, 3);
        // Das abgelaufene Segment 1 ging. Das bisher aktive Segment 2, das
        // gerade beschriebene Segment 3 und alles dahinter kamen für die
        // Retention nicht in Frage.
        let names: Vec<u64> = listed_segments(&store)?
            .iter()
            .map(|(first_seq, _)| *first_seq)
            .collect();
        assert_eq!(names, [2_u64, 3, 7]);
        let records = store.records().map_err(ctx("records"))?;
        assert_eq!(records.len(), 2);
        let kept = records.first().ok_or(TestError::Missing("record 2"))?;
        assert_eq!(kept.reference(), previous);
        assert_eq!(kept.prev_hash(), first.verdict_digest);
        let written = records.get(1).ok_or(TestError::Missing("record 3"))?;
        assert_eq!(written.reference(), third);
        assert_eq!(written.prev_hash(), previous.verdict_digest);
        assert_eq!(store.verify_chain().map_err(ctx("verify"))?.first_seq, 2);
        Ok(())
    }

    #[test]
    fn append_refuses_an_unreadable_line_in_the_active_segment() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        for rationale in ["eins", "zwei"] {
            store
                .append(&verdict(&finding_a, rationale, now))
                .map_err(ctx("append"))?;
        }
        let path = only_segment(&store)?;
        let text = std::fs::read_to_string(&path)?;
        let second = text
            .lines()
            .nth(1)
            .ok_or(TestError::Missing("second line"))?;
        std::fs::write(&path, format!("{{\"not\":\"a record\"}}\n{second}\n"))?;
        let before = std::fs::read(&path)?;

        // Der letzte Datensatz (2) ist für sich intakt, die Kette davor
        // nicht: nichts wird angehängt, auch kein neues Segment begonnen.
        assert!(matches!(
            store.append(&verdict(&finding_a, "drei", now)),
            Err(VerdictStoreError::CorruptRecord {
                segment_seq: 1,
                line: 1
            })
        ));
        assert_eq!(std::fs::read(&path)?, before);
        assert_eq!(segment_paths(&store)?, [path]);
        // Die Verletzung bleibt sichtbar.
        assert!(matches!(
            store.verify_chain(),
            Err(VerdictStoreError::CorruptRecord {
                segment_seq: 1,
                line: 1
            })
        ));
        Ok(())
    }

    #[test]
    fn append_refuses_a_resealed_middle_record_and_leaves_the_file_unchanged() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        for rationale in ["eins", "zwei", "drei"] {
            store
                .append(&verdict(&finding_a, rationale, now))
                .map_err(ctx("append"))?;
        }
        let path = only_segment(&store)?;
        let mut records = parse_lines(&std::fs::read_to_string(&path)?)?;
        // Datensatz 2 (nicht das Ende) ändern und neu versiegeln: jeder
        // Datensatz ist für sich intakt, nur die Verknüpfung 2 → 3 bricht.
        let second = records.get_mut(1).ok_or(TestError::Missing("second"))?;
        second.classification = StoredClassification::Benign;
        let resealed = compute_record_hash(second);
        second.record_hash = resealed;
        write_lines(&path, &records)?;
        for record in &records {
            assert_eq!(seal_violation(record), None);
        }
        let before = std::fs::read(&path)?;

        expect_chain_broken(
            store.append(&verdict(&finding_a, "vier", now)),
            3,
            ChainBreak::PrevHashMismatch,
        )?;
        // Nichts angehängt, nichts abgeschnitten, kein neues Segment.
        assert_eq!(std::fs::read(&path)?, before);
        assert_eq!(segment_paths(&store)?, [path]);
        expect_chain_broken(store.verify_chain(), 3, ChainBreak::PrevHashMismatch)
    }

    #[test]
    fn append_refuses_a_break_in_an_older_segment() -> TestResult {
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        let finding_a = finding("finding-a")?;
        // Vier Ein-Datensatz-Segmente. Datensatz 2 in Segment 2 wird geändert
        // und neu versiegelt: der Bruch liegt an der Verknüpfung zu Datensatz
        // 3 (Segment 3); das aktive Segment 4 ist für sich und zu 3 intakt.
        let store = store_with_segments(temp.path(), 0, 4, now)?;
        let paths = segment_paths(&store)?;
        assert_eq!(paths.len(), 4);
        let second = paths.get(1).ok_or(TestError::Missing("second segment"))?;
        let mut records = parse_lines(&std::fs::read_to_string(second)?)?;
        let record = records.first_mut().ok_or(TestError::Missing("record 2"))?;
        record.severity = StoredSeverity::Low;
        let resealed = compute_record_hash(record);
        record.record_hash = resealed;
        write_lines(second, &records)?;
        let before = snapshot(&paths)?;

        expect_chain_broken(
            store.append(&verdict(&finding_a, "danach", now)),
            3,
            ChainBreak::PrevHashMismatch,
        )?;
        assert_eq!(segment_paths(&store)?, paths);
        assert_eq!(snapshot(&paths)?, before);
        expect_chain_broken(store.verify_chain(), 3, ChainBreak::PrevHashMismatch)?;

        // Ohne Neuversiegelung (Inhalt von Segment 1 geändert) meldet das
        // Anhängen den Hash-Bruch an Datensatz 1 und schreibt ebenfalls nichts.
        let temp_plain = tempfile::tempdir()?;
        let plain = store_with_segments(temp_plain.path(), 0, 3, now)?;
        let plain_paths = segment_paths(&plain)?;
        let first = plain_paths
            .first()
            .ok_or(TestError::Missing("first segment"))?;
        tamper_segment(first, "Begründung 0", "Begründung X")?;
        let plain_before = snapshot(&plain_paths)?;
        expect_chain_broken(
            plain.append(&verdict(&finding_a, "danach", now)),
            1,
            ChainBreak::RecordHashMismatch,
        )?;
        assert_eq!(segment_paths(&plain)?, plain_paths);
        assert_eq!(snapshot(&plain_paths)?, plain_before);

        // Gegenprobe: ist die Kette wieder intakt, wird angehängt.
        tamper_segment(first, "Begründung X", "Begründung 0")?;
        let next = plain
            .append(&verdict(&finding_a, "danach", now))
            .map_err(ctx("append after restore"))?;
        assert_eq!(next.seq, 4);
        assert_eq!(segment_paths(&plain)?.len(), 4);
        Ok(())
    }

    #[test]
    fn append_refuses_a_torn_line_in_a_closed_segment() -> TestResult {
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        let store = store_with_segments(temp.path(), 0, 2, now)?;
        let paths = segment_paths(&store)?;
        let first = paths.first().ok_or(TestError::Missing("first segment"))?;
        // Nur im letzten Segment gilt eine Zeile ohne `\n` als abgerissen.
        OpenOptions::new()
            .append(true)
            .open(first)?
            .write_all(b"{\"seq\":9")?;
        let before = snapshot(&paths)?;

        assert!(matches!(
            store.append(&verdict(&finding("finding-a")?, "danach", now)),
            Err(VerdictStoreError::CorruptRecord {
                segment_seq: 1,
                line: 0
            })
        ));
        assert_eq!(segment_paths(&store)?, paths);
        assert_eq!(snapshot(&paths)?, before);
        Ok(())
    }

    #[test]
    fn append_refuses_an_anchor_that_does_not_link_to_the_first_live_record() -> TestResult {
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        // Vier Segmente, 1–2 alt: die Retention verankert bei `seq` 3.
        let store = store_with_segments(temp.path(), 2, 4, now)?;
        let report = store.apply_retention(now).map_err(ctx("retention"))?;
        assert_eq!(report.first_seq, 3);
        let anchor = read_anchor(&root_of(&store)?)
            .map_err(ctx("anchor"))?
            .ok_or(TestError::Missing("anchor"))?;
        let forged = VerdictAnchor {
            prev_hash: ContentDigest::of(b"forged anchor"),
            ..anchor
        };
        std::fs::write(store.root().join(ANCHOR_FILE), serde_json::to_vec(&forged)?)?;
        let paths = segment_paths(&store)?;
        let before = snapshot(&paths)?;

        // Der letzte Datensatz ist intakt; gebrochen ist nur der Anfang.
        expect_chain_broken(
            store.append(&verdict(&finding("finding-a")?, "danach", now)),
            3,
            ChainBreak::PrevHashMismatch,
        )?;
        assert_eq!(segment_paths(&store)?, paths);
        assert_eq!(snapshot(&paths)?, before);
        expect_chain_broken(store.verify_chain(), 3, ChainBreak::PrevHashMismatch)
    }

    #[test]
    fn retention_recovers_from_a_crash_between_anchor_and_delete() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path())
            .with_segment_limit(1)
            .without_auto_retention();
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        let old = days_before(now, 100)?;
        for created_at in [old, old, now] {
            store
                .append(&verdict(&finding_a, "Begründung", created_at))
                .map_err(ctx("append"))?;
        }
        let first_path = segment_paths(&store)?
            .into_iter()
            .next()
            .ok_or(TestError::Missing("first segment"))?;
        let first_bytes = std::fs::read(&first_path)?;

        store.apply_retention(now).map_err(ctx("retention"))?;
        // Absturz simulieren: das ausgemusterte Segment liegt wieder da.
        std::fs::write(&first_path, &first_bytes)?;
        std::fs::set_permissions(&first_path, Permissions::from_mode(FILE_MODE))?;

        let summary = store.verify_chain().map_err(ctx("verify with retired"))?;
        assert_eq!(summary.first_seq, 3);
        assert_eq!(summary.records, 1);
        let report = store.apply_retention(now).map_err(ctx("retention again"))?;
        assert_eq!(report.removed_segments, 1);
        assert_eq!(report.removed_records, 0);
        assert!(!first_path.exists());
        Ok(())
    }

    #[test]
    fn retention_never_reanchors_over_a_broken_first_segment() -> TestResult {
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        // Drei alte Segmente; das erste ist manipuliert.
        let store = store_with_segments(temp.path(), 3, 3, now)?;
        let first_path = segment_paths(&store)?
            .into_iter()
            .next()
            .ok_or(TestError::Missing("first segment"))?;
        tamper_segment(&first_path, "Begründung 0", "Begründung X")?;

        let report = store.apply_retention(now).map_err(ctx("retention"))?;
        assert_eq!(report.fault_segment, Some(1));
        assert_eq!(report.removed_segments, 0);
        assert_eq!(report.removed_records, 0);
        assert_eq!(report.first_seq, FIRST_VERDICT_SEQ);
        // Nichts gelöscht, nichts verankert, der Bruch bleibt sichtbar.
        assert!(first_path.exists());
        assert!(!store.root().join(ANCHOR_FILE).exists());
        expect_chain_broken(store.verify_chain(), 1, ChainBreak::RecordHashMismatch)
    }

    #[test]
    fn retention_still_removes_the_verified_prefix_before_a_break() -> TestResult {
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        // Sechs Segmente: 1–4 alt, 5–6 jung; Segment 5 ist manipuliert.
        let store = store_with_segments(temp.path(), 4, 6, now)?;
        let paths = segment_paths(&store)?;
        let fifth = paths.get(4).ok_or(TestError::Missing("fifth segment"))?;
        tamper_segment(fifth, "Begründung 4", "Begründung X")?;

        let report = store.apply_retention(now).map_err(ctx("retention"))?;
        assert_eq!(report.fault_segment, Some(5));
        assert_eq!(report.removed_segments, 4);
        assert_eq!(report.removed_records, 4);
        assert_eq!(report.first_seq, 5);
        assert_eq!(segment_paths(&store)?.len(), 2);
        // Der Anker ist ehrlich: die Prüfung scheitert weiter genau am
        // manipulierten Datensatz, nicht an der Verankerung.
        expect_chain_broken(store.verify_chain(), 5, ChainBreak::RecordHashMismatch)?;

        // Wird die Manipulation zurückgenommen, prüft die Kette wieder sauber.
        tamper_segment(fifth, "Begründung X", "Begründung 4")?;
        let summary = store.verify_chain().map_err(ctx("verify restored"))?;
        assert_eq!(summary.first_seq, 5);
        assert_eq!(summary.records, 2);
        Ok(())
    }

    #[test]
    fn retention_continues_past_a_corrupt_line_in_the_active_segment() -> TestResult {
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        // Drei Segmente: 1–2 alt, 3 jung und aktiv, mit einer kaputten Zeile.
        let store = store_with_segments(temp.path(), 2, 3, now)?;
        let paths = segment_paths(&store)?;
        let active = paths.get(2).ok_or(TestError::Missing("active segment"))?;
        OpenOptions::new()
            .append(true)
            .open(active)?
            .write_all(b"{\"not\":\"a record\"}\n")?;

        let report = store.apply_retention(now).map_err(ctx("retention"))?;
        assert_eq!(report.fault_segment, Some(3));
        assert_eq!(report.removed_segments, 2);
        assert_eq!(report.first_seq, 3);
        assert!(active.exists());
        assert!(matches!(
            store.verify_chain(),
            Err(VerdictStoreError::CorruptRecord {
                segment_seq: 3,
                line: 2
            })
        ));
        Ok(())
    }

    #[test]
    fn intact_chain_reports_no_fault_segment() -> TestResult {
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        let store = store_with_segments(temp.path(), 2, 3, now)?;
        let report = store.apply_retention(now).map_err(ctx("retention"))?;
        assert_eq!(report.fault_segment, None);
        assert_eq!(report.removed_segments, 2);
        Ok(())
    }

    /// Review 4505256692: der Probelauf sagt genau den Bericht voraus, den die
    /// Retention zur selben Jetzt-Zeit liefert: abgelaufenes Präfix, alles
    /// abgelaufen (nur der Anker bleibt) und nichts fällig; danach (verankert)
    /// ebenso.
    #[test]
    fn plan_retention_matches_apply_retention() -> TestResult {
        let now = at(BASE_SECOND)?;
        // (alte Segmente, Segmente, entfernte Segmente, entfernte Datensätze,
        // `first_seq` danach)
        for (old, count, removed_segments, removed_records, first_seq) in [
            (4_usize, 6_usize, 4_usize, 4_u64, 5_u64),
            (3, 3, 3, 3, 4),
            (0, 2, 0, 0, FIRST_VERDICT_SEQ),
        ] {
            let temp = tempfile::tempdir()?;
            let store = store_with_segments(temp.path(), old, count, now)?;

            let plan = store.plan_retention(now).map_err(ctx("plan"))?;
            assert_eq!(plan.removed_segments, removed_segments, "{old}/{count}");
            assert_eq!(plan.removed_records, removed_records, "{old}/{count}");
            assert_eq!(plan.first_seq, first_seq, "{old}/{count}");
            assert_eq!(plan.fault_segment, None, "{old}/{count}");
            assert_eq!(
                segment_paths(&store)?.len(),
                count,
                "the plan removes nothing"
            );

            let report = store.apply_retention(now).map_err(ctx("apply"))?;
            assert_eq!(report, plan, "{old}/{count}: apply does what the plan said");
            assert_eq!(
                segment_paths(&store)?.len(),
                count.saturating_sub(removed_segments)
            );

            // Danach ist nichts mehr fällig, und auch das sagen beide gleich.
            let again = store.plan_retention(now).map_err(ctx("plan again"))?;
            assert_eq!(again.removed_segments, 0, "{old}/{count}");
            assert_eq!(again.first_seq, first_seq, "{old}/{count}");
            let applied_again = store.apply_retention(now).map_err(ctx("apply again"))?;
            assert_eq!(applied_again, again, "{old}/{count}");
        }
        Ok(())
    }

    /// Ausgemusterte Segmente (Absturz zwischen Anker und Löschen) zählt der
    /// Probelauf wie die Retention mit, ohne sie zu entfernen.
    #[test]
    fn plan_retention_counts_retired_segments_like_apply() -> TestResult {
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        let store = store_with_segments(temp.path(), 2, 3, now)?;
        let first_path = segment_paths(&store)?
            .into_iter()
            .next()
            .ok_or(TestError::Missing("first segment"))?;
        let first_bytes = std::fs::read(&first_path)?;
        store.apply_retention(now).map_err(ctx("retention"))?;
        // Absturz simulieren: das ausgemusterte Segment liegt wieder da.
        std::fs::write(&first_path, &first_bytes)?;
        std::fs::set_permissions(&first_path, Permissions::from_mode(FILE_MODE))?;

        let plan = store.plan_retention(now).map_err(ctx("plan"))?;
        assert_eq!(plan.removed_segments, 1);
        assert_eq!(plan.removed_records, 0);
        assert_eq!(plan.first_seq, 3);
        assert_eq!(plan.fault_segment, None);
        assert!(first_path.exists(), "the plan removes nothing");

        let report = store.apply_retention(now).map_err(ctx("retention again"))?;
        assert_eq!(report, plan);
        assert!(!first_path.exists());
        Ok(())
    }

    /// Der Probelauf schreibt nichts: keine Sperrdatei, kein Anker, keine
    /// Tempdatei, kein Löschen, kein `fchmod`. Einträge, mtimes, Rechte und
    /// Inhalte bleiben gleich, obwohl die Retention hier verankern und
    /// löschen würde (Gegenprobe am Ende).
    #[test]
    fn plan_retention_never_writes() -> TestResult {
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        let store = store_with_segments(temp.path(), 4, 6, now)?;
        let root = store.root().to_path_buf();
        // Ohne `.lock` (ein Plan darf sie nicht anlegen) und mit `0750` (ein
        // Schreiber nähme das per `fchmod` auf `0700` zurück; nicht für
        // Gruppe/Andere beschreibbar, also weiter eine gültige Grenze).
        std::fs::remove_file(root.join(LOCK_FILE))?;
        std::fs::set_permissions(&root, Permissions::from_mode(0o750))?;
        let before = directory_state(&root)?;

        let plan = store.plan_retention(now).map_err(ctx("plan"))?;
        assert_eq!(plan.removed_segments, 4);
        assert_eq!(directory_state(&root)?, before, "the plan wrote nothing");
        assert!(!root.join(LOCK_FILE).exists(), "no lock file");
        assert!(!root.join(ANCHOR_FILE).exists(), "no anchor");
        assert!(!root.join(ANCHOR_TEMP_FILE).exists(), "no anchor temp file");
        assert_eq!(mode_of(&root)?, 0o750, "no fchmod");
        assert_eq!(segment_paths(&store)?.len(), 6);

        // Gegenprobe: die Retention schreibt hier tatsächlich, und zwar genau
        // das Vorhergesagte.
        let report = store.apply_retention(now).map_err(ctx("apply"))?;
        assert_eq!(report, plan);
        assert!(root.join(ANCHOR_FILE).exists());
        assert_eq!(mode_of(&root)?, 0o700);
        assert_eq!(segment_paths(&store)?.len(), 2);
        Ok(())
    }

    /// Ohne Store ist der Plan leer wie der Bericht der Retention, und es
    /// entsteht nichts: weder `state_dir` noch `dod/` noch `verdicts/`.
    #[test]
    fn plan_retention_on_a_missing_store_is_empty_and_creates_nothing() -> TestResult {
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        let empty = VerdictRetentionReport {
            removed_segments: 0,
            removed_records: 0,
            first_seq: FIRST_VERDICT_SEQ,
            fault_segment: None,
        };

        let absent = temp.path().join("absent");
        let plan = VerdictStore::new(&absent)
            .plan_retention(now)
            .map_err(ctx("plan without state dir"))?;
        assert_eq!(plan, empty);
        assert!(!absent.exists(), "no state dir created");

        // `state_dir` vorhanden, `dod/` nicht.
        let store = VerdictStore::new(temp.path());
        let plan = store.plan_retention(now).map_err(ctx("plan without dod"))?;
        assert_eq!(plan, empty);
        assert_eq!(entry_count(temp.path())?, 0, "no dod/ or verdicts/ created");
        let report = store
            .apply_retention(now)
            .map_err(ctx("apply without dod"))?;
        assert_eq!(report, plan);
        Ok(())
    }

    /// Bei einem Kettenbruch plant der Probelauf wie die Retention nur das
    /// geprüfte Präfix davor und meldet das verletzte Segment, ohne es oder
    /// sonst etwas anzufassen.
    #[test]
    fn plan_retention_reports_the_fault_segment_like_apply() -> TestResult {
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        // Drei alte Segmente; das zweite ist manipuliert.
        let store = store_with_segments(temp.path(), 3, 3, now)?;
        let paths = segment_paths(&store)?;
        let second = paths.get(1).ok_or(TestError::Missing("second segment"))?;
        tamper_segment(second, "Begründung 1", "Begründung X")?;
        let before = snapshot(&paths)?;

        let plan = store.plan_retention(now).map_err(ctx("plan"))?;
        assert_eq!(plan.fault_segment, Some(2));
        assert_eq!(plan.removed_segments, 1, "the verified prefix only");
        assert_eq!(plan.removed_records, 1);
        assert_eq!(plan.first_seq, 2);
        assert_eq!(segment_paths(&store)?, paths, "nothing removed");
        assert_eq!(snapshot(&paths)?, before, "nothing changed");
        assert!(!store.root().join(ANCHOR_FILE).exists(), "nothing anchored");

        let report = store.apply_retention(now).map_err(ctx("apply"))?;
        assert_eq!(report, plan);
        assert!(second.exists(), "the broken segment stays visible");
        expect_chain_broken(store.verify_chain(), 2, ChainBreak::RecordHashMismatch)
    }

    #[test]
    fn reader_listing_before_a_retention_and_anchor_after_it_sees_no_break() -> TestResult {
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        let store = store_with_segments(temp.path(), 4, 6, now)?;
        let root = root_of(&store)?;
        // Ordnung von `walk_root`: erst listen …
        let listed_before = list_segments(&root).map_err(ctx("list"))?;
        // … dann läuft eine Retention in einem anderen Prozess …
        store.apply_retention(now).map_err(ctx("retention"))?;
        // … und erst danach liest der Leser den (neuen) Anker.
        let anchor_after = read_anchor(&root).map_err(ctx("anchor"))?;

        let walk = walk_listed(&root, listed_before, anchor_after.as_ref(), |_| {})
            .map_err(ctx("walk"))?;
        assert!(walk.fault.is_none(), "no false tamper alarm");
        assert_eq!(walk.summary.first_seq, 5);
        assert_eq!(walk.summary.records, 2);
        // Die schon gelöschten Segmente 1–4 gelten als ausgemustert und
        // werden nicht geöffnet.
        assert_eq!(walk.retired.len(), 4);
        Ok(())
    }

    #[test]
    fn stale_anchor_with_a_fresh_listing_would_look_broken() -> TestResult {
        // Die Ordnung, die `walk` vermeidet: alter Anker, dann eine Liste
        // nach der Retention. Das wäre ein falscher Kettenbruch.
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        let store = store_with_segments(temp.path(), 4, 6, now)?;
        let root = root_of(&store)?;
        let anchor_before = read_anchor(&root).map_err(ctx("anchor"))?;
        store.apply_retention(now).map_err(ctx("retention"))?;
        let listed_after = list_segments(&root).map_err(ctx("list"))?;

        let walk = walk_listed(&root, listed_after, anchor_before.as_ref(), |_| {})
            .map_err(ctx("walk"))?;
        assert!(matches!(
            walk.fault,
            Some((
                5,
                VerdictStoreError::ChainBroken {
                    seq: 5,
                    reason: ChainBreak::SequenceMismatch { expected: 1 }
                }
            ))
        ));
        // Der echte Leser (Liste vor Anker) sieht den Stand korrekt.
        let summary = store.verify_chain().map_err(ctx("verify"))?;
        assert_eq!(summary.first_seq, 5);
        Ok(())
    }

    #[test]
    fn reader_walks_again_when_a_listed_segment_vanishes() -> TestResult {
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        let store = store_with_segments(temp.path(), 4, 6, now)?;
        let root = root_of(&store)?;
        // Ein Leser listet und liest den (noch fehlenden) Anker …
        let listed = list_segments(&root).map_err(ctx("list"))?;
        let listed_for_retry = list_segments(&root).map_err(ctx("list again"))?;
        let anchor_before = read_anchor(&root).map_err(ctx("anchor"))?;
        assert!(anchor_before.is_none());
        // … dann verankert und löscht eine Retention (anderer Prozess) …
        store.apply_retention(now).map_err(ctx("retention"))?;
        // … bevor der Leser das erste Segment öffnet: ein einzelner Durchlauf
        // scheitert mit `NotFound`.
        match walk_listed(&root, listed, anchor_before.as_ref(), |_| {}) {
            Err(VerdictStoreError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(other) => return Err(TestError::Unexpected(format!("{other:?}"))),
            Ok(walk) => return Err(TestError::Unexpected(format!("{:?}", walk.summary))),
        }

        // Mit `retry_vanished` läuft der Leser neu ab (neue Liste, neuer
        // Anker) und sieht den Stand nach der Retention ohne Fehlalarm.
        let mut stale = Some(listed_for_retry);
        let mut attempts = 0_u32;
        let walk = retry_vanished(|| {
            attempts = attempts.saturating_add(1);
            match stale.take() {
                Some(segments) => walk_listed(&root, segments, anchor_before.as_ref(), |_| {}),
                None => walk_root(&root, |_| {}),
            }
        })
        .map_err(ctx("walk with retry"))?;
        assert_eq!(attempts, 2);
        assert!(walk.fault.is_none());
        assert_eq!(walk.summary.first_seq, 5);
        assert_eq!(walk.summary.records, 2);
        // Die öffentlichen Leser sehen denselben Stand.
        assert_eq!(store.records().map_err(ctx("records"))?.len(), 2);
        Ok(())
    }

    #[test]
    fn retry_vanished_is_bounded_and_repeats_only_not_found() -> TestResult {
        // Ein dauerhaft fehlendes Segment: höchstens `READ_ATTEMPTS` Durchläufe.
        let mut calls = 0_u32;
        let result: VerdictStoreResult<()> = retry_vanished(|| {
            calls = calls.saturating_add(1);
            Err(VerdictStoreError::Io(io::Error::from(
                io::ErrorKind::NotFound,
            )))
        });
        assert!(matches!(
            result,
            Err(VerdictStoreError::Io(ref error)) if error.kind() == io::ErrorKind::NotFound
        ));
        assert_eq!(calls, READ_ATTEMPTS);

        // Andere I/O-Fehler und Kettenbrüche werden nie wiederholt.
        let mut calls = 0_u32;
        let result: VerdictStoreResult<()> = retry_vanished(|| {
            calls = calls.saturating_add(1);
            Err(VerdictStoreError::Io(io::Error::from(
                io::ErrorKind::PermissionDenied,
            )))
        });
        assert!(matches!(result, Err(VerdictStoreError::Io(_))));
        assert_eq!(calls, 1);
        let mut calls = 0_u32;
        let result: VerdictStoreResult<()> = retry_vanished(|| {
            calls = calls.saturating_add(1);
            Err(VerdictStoreError::ChainBroken {
                seq: 3,
                reason: ChainBreak::PrevHashMismatch,
            })
        });
        expect_chain_broken(result, 3, ChainBreak::PrevHashMismatch)?;
        assert_eq!(calls, 1);

        // Ein zweiter Durchlauf, der gelingt, liefert sein Ergebnis.
        let mut calls = 0_u32;
        let result = retry_vanished(|| {
            calls = calls.saturating_add(1);
            if calls == 1 {
                Err(VerdictStoreError::Io(io::Error::from(
                    io::ErrorKind::NotFound,
                )))
            } else {
                Ok(calls)
            }
        })
        .map_err(ctx("second attempt"))?;
        assert_eq!(result, 2);
        Ok(())
    }

    #[test]
    fn severity_is_covered_by_the_record_hash() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        store
            .append(&verdict(&finding_a, "eins", at(BASE_SECOND)?))
            .map_err(ctx("append"))?;
        let path = only_segment(&store)?;
        let mut records = parse_lines(&std::fs::read_to_string(&path)?)?;
        let first = records.first_mut().ok_or(TestError::Missing("first"))?;
        first.severity = StoredSeverity::Info;
        write_lines(&path, &records)?;

        expect_chain_broken(store.verify_chain(), 1, ChainBreak::RecordHashMismatch)
    }

    #[test]
    fn append_waits_for_a_briefly_held_lock() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        store
            .append(&verdict(&finding_a, "eins", now))
            .map_err(ctx("append first"))?;

        // Ein zweiter Schreiber hält die Sperre kurz (flock je Deskriptor).
        let holder = File::options()
            .write(true)
            .open(store.root().join(LOCK_FILE))?;
        FileExt::try_lock(&holder).map_err(ctx("take lock"))?;
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            FileExt::unlock(&holder)
        });

        let second = store
            .append(&verdict(&finding_a, "zwei", now))
            .map_err(ctx("append while lock is held briefly"))?;
        releaser
            .join()
            .map_err(|_| TestError::Unexpected("lock holder thread panicked".to_owned()))?
            .map_err(ctx("release lock"))?;
        assert_eq!(second.seq, 2);
        Ok(())
    }

    #[test]
    fn append_reports_lock_contention_after_the_bounded_wait() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        store
            .append(&verdict(&finding_a, "eins", now))
            .map_err(ctx("append first"))?;

        let holder = File::options()
            .write(true)
            .open(store.root().join(LOCK_FILE))?;
        FileExt::try_lock(&holder).map_err(ctx("take lock"))?;
        assert!(matches!(
            store.append(&verdict(&finding_a, "blockiert", now)),
            Err(VerdictStoreError::LockContended)
        ));
        assert!(matches!(
            store.apply_retention(now),
            Err(VerdictStoreError::LockContended)
        ));
        // Nichts wurde geschrieben, während die Sperre belegt war.
        assert_eq!(store.verify_chain().map_err(ctx("verify"))?.records, 1);

        FileExt::unlock(&holder)?;
        let next = store
            .append(&verdict(&finding_a, "zwei", now))
            .map_err(ctx("append after release"))?;
        assert_eq!(next.seq, 2);
        Ok(())
    }

    // Entfernt `.lock` und legt an seiner Stelle eine neue, leere Datei an
    // (wie ein dateiweiser Sweep, nach dem der nächste Schreiber neu anlegt).
    fn replace_lock_file(lock_path: &Path) -> io::Result<()> {
        std::fs::remove_file(lock_path)?;
        std::fs::write(lock_path, b"")
    }

    #[test]
    fn lock_on_a_replaced_lock_file_is_taken_again_on_the_current_one() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        store
            .append(&verdict(&finding_a, "eins", now))
            .map_err(ctx("append first"))?;
        let lock_path = store.root().join(LOCK_FILE);

        // Ein Schreiber hält die Sperre. Währenddessen wird `.lock` ersetzt;
        // erst danach gibt er die alte, nun verwaiste Inode frei.
        let holder = File::options().write(true).open(&lock_path)?;
        FileExt::try_lock(&holder).map_err(ctx("take lock"))?;
        let replace_path = lock_path.clone();
        let releaser = std::thread::spawn(move || -> io::Result<()> {
            std::thread::sleep(Duration::from_millis(50));
            replace_lock_file(&replace_path)?;
            FileExt::unlock(&holder)
        });

        // Der Wartende öffnet zuerst die alte Datei und bekommt deren Sperre,
        // sobald sie frei wird; weil `.lock` inzwischen eine andere Inode ist,
        // sperrt er die aktuelle Datei neu.
        let root = root_of(&store)?;
        let lock = acquire_lock(&root).map_err(ctx("acquire"))?;
        releaser
            .join()
            .map_err(|_| TestError::Unexpected("lock holder thread panicked".to_owned()))?
            .map_err(ctx("replace and release"))?;
        let held = lock.metadata()?;
        let current = std::fs::symlink_metadata(&lock_path)?;
        assert_eq!((held.dev(), held.ino()), (current.dev(), current.ino()));
        assert!(lock_is_current(&root, &lock).map_err(ctx("lock is current"))?);
        // Ein weiterer Schreiber auf der aktuellen Datei muss also warten.
        let other = File::options().write(true).open(&lock_path)?;
        assert!(matches!(
            FileExt::try_lock(&other),
            Err(fs4::TryLockError::WouldBlock)
        ));
        release(lock, Ok(())).map_err(ctx("release"))?;
        Ok(())
    }

    #[test]
    fn replaced_lock_file_keeps_two_writers_apart() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        store
            .append(&verdict(&finding_a, "eins", now))
            .map_err(ctx("append first"))?;
        let lock_path = store.root().join(LOCK_FILE);

        // Schreiber A hält die alte Inode. `.lock` wird ersetzt, Schreiber B
        // sperrt die neue Datei, und erst dann gibt A die alte frei.
        let holder = File::options().write(true).open(&lock_path)?;
        FileExt::try_lock(&holder).map_err(ctx("take lock"))?;
        let replace_path = lock_path.clone();
        let releaser = std::thread::spawn(move || -> io::Result<File> {
            std::thread::sleep(Duration::from_millis(50));
            replace_lock_file(&replace_path)?;
            let newer = File::options().write(true).open(&replace_path)?;
            FileExt::try_lock(&newer)?;
            FileExt::unlock(&holder)?;
            Ok(newer)
        });

        // Ein dritter Schreiber, der auf die alte Inode gewartet hat, hält sich
        // nicht an ihr fest, sondern wartet auf B und meldet dann `LockContended`.
        let root = root_of(&store)?;
        assert!(matches!(
            acquire_lock(&root),
            Err(VerdictStoreError::LockContended)
        ));
        let newer = releaser
            .join()
            .map_err(|_| TestError::Unexpected("lock holder thread panicked".to_owned()))?
            .map_err(ctx("replace, lock and release"))?;
        // Nichts wurde geschrieben, solange B hielt.
        assert_eq!(store.verify_chain().map_err(ctx("verify"))?.records, 1);
        FileExt::unlock(&newer)?;
        let next = store
            .append(&verdict(&finding_a, "zwei", now))
            .map_err(ctx("append after release"))?;
        assert_eq!(next.seq, 2);
        Ok(())
    }

    #[test]
    fn writers_abort_when_the_lock_file_is_replaced_while_held() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        store
            .append(&verdict(&finding_a, "eins", now))
            .map_err(ctx("append first"))?;
        let segment = only_segment(&store)?;
        let before = std::fs::read(&segment)?;

        // Die Sperre ist gehalten; danach ersetzt ein fremdes Werkzeug `.lock`,
        // und ein anderer Schreiber könnte die neue Datei sperren.
        let root = root_of(&store)?;
        let lock = acquire_lock(&root).map_err(ctx("acquire"))?;
        replace_lock_file(&store.root().join(LOCK_FILE))?;

        // Anhängen bricht vor der ersten Änderung ab …
        let prepared = prepare(&verdict(&finding_a, "zwei", now)).map_err(ctx("prepare"))?;
        assert!(matches!(
            store.append_locked(prepared, &root, &lock),
            Err(VerdictStoreError::LockContended)
        ));
        assert_eq!(std::fs::read(&segment)?, before);
        assert_eq!(segment_paths(&store)?, [segment.clone()]);
        // … und die Retention verankert und löscht nichts, obwohl das Segment
        // aus ihrer Sicht abgelaufen ist.
        let far = now
            .checked_add(SignedDuration::from_secs(100 * 86_400))
            .map_err(ctx("far"))?;
        assert!(matches!(
            retention_locked(&root, &lock, far),
            Err(VerdictStoreError::LockContended)
        ));
        assert!(segment.exists());
        assert!(!store.root().join(ANCHOR_FILE).exists());
        release(lock, Ok(())).map_err(ctx("release"))?;

        // Ein neuer Aufruf sperrt die aktuelle Datei und hängt an.
        let second = store
            .append(&verdict(&finding_a, "zwei", now))
            .map_err(ctx("append after the replacement"))?;
        assert_eq!(second.seq, 2);
        Ok(())
    }

    #[test]
    fn missing_components_are_created_private_and_only_by_writers() -> TestResult {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        let store = VerdictStore::new(&state);
        // Oberhalb der Grenze (das vertraute `state_dir` samt Eltern): die
        // fehlenden Glieder, das oberste zuerst.
        let nested = state.join("a").join("b");
        assert_eq!(
            missing_directories(&nested).map_err(ctx("missing"))?,
            [state.clone(), state.join("a"), nested.clone()]
        );
        assert_eq!(parent_or_current(Path::new("dod")), Path::new("."));
        assert_eq!(parent_or_current(&state), temp.path());

        // Leser und Retention legen nichts an.
        assert_eq!(store.verify_chain().map_err(ctx("verify"))?.records, 0);
        let report = store
            .apply_retention(at(BASE_SECOND)?)
            .map_err(ctx("retention"))?;
        assert_eq!(report.removed_segments, 0);
        assert!(!state.exists());

        // Anhängen legt alle Glieder ohne Gruppen-/Fremdrechte an (`0700`
        // abzüglich umask; `dod` und `verdicts` per `mkdirat`); danach fehlt
        // keines mehr.
        store
            .append(&verdict(&finding("finding-a")?, "eins", at(BASE_SECOND)?))
            .map_err(ctx("append"))?;
        assert!(
            missing_directories(store.root())
                .map_err(ctx("missing after"))?
                .is_empty()
        );
        let dod = state.join(DOD_DIR);
        assert_eq!(mode_of(&state)? & 0o077, 0);
        assert_eq!(mode_of(&dod)? & 0o077, 0);
        assert_eq!(mode_of(store.root())?, 0o700);
        assert!(std::fs::symlink_metadata(&dod)?.is_dir());
        assert!(std::fs::symlink_metadata(store.root())?.is_dir());
        Ok(())
    }

    #[test]
    fn empty_segment_left_by_a_failed_append_is_reused() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        // Ein früheres Anhängen hat das Segment angelegt und ist dann
        // gescheitert: leere Datei, benannt nach der nächsten `seq`.
        precreate_root(&store)?;
        let path = store.root().join(segment_file_name(FIRST_VERDICT_SEQ));
        std::fs::write(&path, b"")?;
        std::fs::set_permissions(&path, Permissions::from_mode(FILE_MODE))?;

        let first = store
            .append(&verdict(&finding_a, "eins", now))
            .map_err(ctx("append into empty segment"))?;
        assert_eq!(first.seq, FIRST_VERDICT_SEQ);
        assert_eq!(only_segment(&store)?, path);
        assert_eq!(store.verify_chain().map_err(ctx("verify"))?.records, 1);
        Ok(())
    }

    #[test]
    fn files_are_0600_and_the_directory_is_0700() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path()).with_segment_limit(1);
        // Ein lesbares, vorhandenes Verzeichnis (nicht beschreibbar für
        // Gruppe/Andere) wird auf 0700 gesetzt.
        precreate_root(&store)?;
        std::fs::set_permissions(store.root(), Permissions::from_mode(0o755))?;

        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        let old = days_before(now, 100)?;
        for created_at in [old, now] {
            store
                .append(&verdict(&finding_a, "Begründung", created_at))
                .map_err(ctx("append"))?;
        }
        store.apply_retention(now).map_err(ctx("retention"))?;

        assert_eq!(mode_of(store.root())?, 0o700);
        let mut files = segment_paths(&store)?;
        files.push(store.root().join(ANCHOR_FILE));
        files.push(store.root().join(LOCK_FILE));
        for path in &files {
            assert_eq!(mode_of(path)?, 0o600);
            ensure_private_regular(&File::open(path)?)?;
        }
        Ok(())
    }

    #[test]
    fn torn_tail_is_skipped_by_readers_and_removed_on_append() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        store
            .append(&verdict(&finding_a, "eins", now))
            .map_err(ctx("append"))?;
        let path = only_segment(&store)?;
        OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(b"{\"seq\":2,\"prev_ha")?;

        let summary = store.verify_chain().map_err(ctx("verify torn"))?;
        assert_eq!(summary.records, 1);

        let second = store
            .append(&verdict(&finding_a, "zwei", now))
            .map_err(ctx("append after torn"))?;
        assert_eq!(second.seq, 2);
        let records = parse_lines(&std::fs::read_to_string(&path)?)?;
        assert_eq!(records.len(), 2);
        let summary = store.verify_chain().map_err(ctx("verify repaired"))?;
        assert_eq!(summary.records, 2);
        Ok(())
    }

    #[test]
    fn symlinked_segments_and_roots_are_rejected() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        store
            .append(&verdict(&finding_a, "eins", at(BASE_SECOND)?))
            .map_err(ctx("append"))?;
        let elsewhere = temp.path().join("elsewhere.jsonl");
        std::fs::write(&elsewhere, b"")?;
        symlink(&elsewhere, store.root().join(segment_file_name(2)))?;
        assert!(matches!(
            store.verify_chain(),
            Err(VerdictStoreError::UnsafeSegment)
        ));

        let other = tempfile::tempdir()?;
        DirBuilder::new()
            .mode(DIR_MODE)
            .create(other.path().join(DOD_DIR))?;
        let target = other.path().join("real");
        DirBuilder::new().mode(DIR_MODE).create(&target)?;
        symlink(&target, other.path().join(DOD_DIR).join(VERDICTS_DIR))?;
        let linked = VerdictStore::new(other.path());
        expect_unsafe_directory(linked.append(&verdict(&finding_a, "eins", at(BASE_SECOND)?)))?;
        expect_unsafe_directory(linked.verify_chain())?;
        expect_unsafe_directory(linked.apply_retention(at(BASE_SECOND)?))?;
        // Am Ziel des Symlinks wurde nichts angelegt.
        assert_eq!(entry_count(&target)?, 0);
        Ok(())
    }

    #[test]
    fn intermediate_dod_symlink_is_refused_without_creating_anything() -> TestResult {
        let temp = tempfile::tempdir()?;
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        // `state/dod` ist ein Symlink auf ein fremdes (hier: eigenes) Verzeichnis.
        let state = temp.path().join("state");
        let target = temp.path().join("elsewhere");
        DirBuilder::new().mode(DIR_MODE).create(&state)?;
        DirBuilder::new().mode(DIR_MODE).create(&target)?;
        symlink(&target, state.join(DOD_DIR))?;
        let store = VerdictStore::new(&state);

        expect_unsafe_directory(store.append(&verdict(&finding_a, "eins", now)))?;
        expect_unsafe_directory(store.verify_chain())?;
        expect_unsafe_directory(store.records())?;
        expect_unsafe_directory(store.latest_for(&finding_a))?;
        expect_unsafe_directory(store.apply_retention(now))?;
        // Nichts wurde am Ziel angelegt (früher: `create_dir_all` folgte dem
        // Symlink und legte `elsewhere/verdicts` an), der Symlink blieb.
        assert_eq!(entry_count(&target)?, 0);
        assert!(
            std::fs::symlink_metadata(state.join(DOD_DIR))?
                .file_type()
                .is_symlink()
        );

        // Auch ein Store, der hinter dem Symlink schon vollständig liegt, wird
        // nicht benutzt.
        let real = VerdictStore::new(&target);
        real.append(&verdict(&finding_a, "dort", now))
            .map_err(ctx("append behind the link"))?;
        let linked_dod = temp.path().join("linked");
        DirBuilder::new().mode(DIR_MODE).create(&linked_dod)?;
        symlink(target.join(DOD_DIR), linked_dod.join(DOD_DIR))?;
        let linked = VerdictStore::new(&linked_dod);
        expect_unsafe_directory(linked.verify_chain())?;
        expect_unsafe_directory(linked.append(&verdict(&finding_a, "hier", now)))?;
        assert_eq!(real.verify_chain().map_err(ctx("verify real"))?.records, 1);

        // Eine reguläre Datei an der Stelle von `dod` ist kein Verzeichnis.
        let plain_state = temp.path().join("plain");
        DirBuilder::new().mode(DIR_MODE).create(&plain_state)?;
        std::fs::write(plain_state.join(DOD_DIR), b"")?;
        let plain = VerdictStore::new(&plain_state);
        expect_unsafe_directory(plain.append(&verdict(&finding_a, "eins", now)))?;
        expect_unsafe_directory(plain.verify_chain())?;
        assert!(std::fs::read(plain_state.join(DOD_DIR))?.is_empty());
        Ok(())
    }

    #[test]
    fn symlinked_state_dir_is_trusted_but_links_below_it_are_not() -> TestResult {
        let temp = tempfile::tempdir()?;
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        // `state` → `real`, wie `~/.harw` → `/data/harw`: erlaubt.
        let real = temp.path().join("real");
        DirBuilder::new().mode(DIR_MODE).create(&real)?;
        let state = temp.path().join("state");
        symlink(&real, &state)?;
        let store = VerdictStore::new(&state);
        let appended = store
            .append(&verdict(&finding_a, "eins", now))
            .map_err(ctx("append through a linked state_dir"))?;
        let summary = store.verify_chain().map_err(ctx("verify"))?;
        assert_eq!(summary.records, 1);
        assert_eq!(summary.last, Some(appended));
        let segment = real
            .join(DOD_DIR)
            .join(VERDICTS_DIR)
            .join(segment_file_name(FIRST_VERDICT_SEQ));
        assert!(std::fs::symlink_metadata(&segment)?.is_file());
        assert!(std::fs::symlink_metadata(&state)?.file_type().is_symlink());

        // Unter einem verlinkten `state_dir` gilt die Grenze unverändert: ein
        // Symlink an der Stelle von `dod` wird verweigert, am Ziel entsteht
        // nichts.
        let real_two = temp.path().join("real-two");
        let elsewhere = temp.path().join("elsewhere");
        DirBuilder::new().mode(DIR_MODE).create(&real_two)?;
        DirBuilder::new().mode(DIR_MODE).create(&elsewhere)?;
        symlink(&elsewhere, real_two.join(DOD_DIR))?;
        let state_two = temp.path().join("state-two");
        symlink(&real_two, &state_two)?;
        let linked = VerdictStore::new(&state_two);
        expect_unsafe_directory(linked.append(&verdict(&finding_a, "zwei", now)))?;
        expect_unsafe_directory(linked.verify_chain())?;
        assert_eq!(entry_count(&elsewhere)?, 0);
        Ok(())
    }

    #[test]
    fn state_dir_that_is_not_a_directory_is_refused() -> TestResult {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        std::fs::write(&state, b"kein Verzeichnis")?;
        let store = VerdictStore::new(&state);
        let now = at(BASE_SECOND)?;
        expect_unsafe_directory(store.verify_chain())?;
        expect_unsafe_directory(store.append(&verdict(&finding("finding-a")?, "eins", now)))?;
        expect_unsafe_directory(store.apply_retention(now))?;
        assert_eq!(std::fs::read(&state)?, b"kein Verzeichnis");
        Ok(())
    }

    // Legt per `mkfifo(1)` ein FIFO an; `false`, wenn das Programm fehlt.
    fn make_fifo(path: &Path) -> bool {
        std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .is_ok_and(|status| status.success())
    }

    #[test]
    fn fifos_in_place_of_store_directories_are_refused_without_blocking() -> TestResult {
        let temp = tempfile::tempdir()?;
        // Ein FIFO als `state_dir` und ein FIFO als `dod`: beide werden nie
        // geöffnet (ein `open(2)` darauf blockierte ohne Schreiber).
        let fifo_state = temp.path().join("fifo-state");
        let state = temp.path().join("state");
        DirBuilder::new().mode(DIR_MODE).create(&state)?;
        if !make_fifo(&fifo_state) || !make_fifo(&state.join(DOD_DIR)) {
            // Ohne `mkfifo` lässt sich der Fall hier nicht herstellen.
            return Ok(());
        }
        let now = at(BASE_SECOND)?;
        let candidate = verdict(&finding("finding-a")?, "eins", now);
        for state_dir in [fifo_state, state] {
            let store = VerdictStore::new(&state_dir);
            let attempt = candidate.clone();
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let refused = |result: Result<(), VerdictStoreError>| {
                    matches!(result, Err(VerdictStoreError::UnsafeDirectory { .. }))
                };
                let outcome = (
                    refused(store.verify_chain().map(|_| ())),
                    refused(store.append(&attempt).map(|_| ())),
                );
                // Ohne Empfänger ist der Test schon an der Frist gescheitert.
                let _ = sender.send(outcome);
            });
            let outcome = receiver
                .recv_timeout(Duration::from_secs(10))
                .map_err(ctx("store call blocked on a FIFO"))?;
            assert_eq!(outcome, (true, true));
        }
        Ok(())
    }

    #[test]
    fn group_or_world_writable_store_directories_are_refused() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        let dod = temp.path().join(DOD_DIR);
        DirBuilder::new().mode(DIR_MODE).create(&dod)?;

        // Ein für Gruppe oder Andere beschreibbares `dod` ist keine Grenze:
        // abgelehnt, nichts darunter angelegt, nichts repariert.
        for mode in [0o770, 0o703, 0o777] {
            std::fs::set_permissions(&dod, Permissions::from_mode(mode))?;
            expect_unsafe_directory(store.append(&verdict(&finding_a, "eins", now)))?;
            expect_unsafe_directory(store.verify_chain())?;
            expect_unsafe_directory(store.apply_retention(now))?;
            assert!(!store.root().exists());
            assert_eq!(mode_of(&dod)?, mode);
        }

        // Lesbar für andere ist erlaubt (`dod` bleibt, wie es ist); das
        // Store-Verzeichnis selbst wird `0700`.
        std::fs::set_permissions(&dod, Permissions::from_mode(0o755))?;
        store
            .append(&verdict(&finding_a, "eins", now))
            .map_err(ctx("append"))?;
        assert_eq!(mode_of(&dod)?, 0o755);
        assert_eq!(mode_of(store.root())?, 0o700);

        // Ein beschreibbares `verdicts` wird ebenso abgelehnt, auch von Lesern.
        std::fs::set_permissions(store.root(), Permissions::from_mode(0o770))?;
        expect_unsafe_directory(store.append(&verdict(&finding_a, "zwei", now)))?;
        expect_unsafe_directory(store.records())?;
        assert_eq!(mode_of(store.root())?, 0o770);

        std::fs::set_permissions(store.root(), Permissions::from_mode(DIR_MODE))?;
        assert_eq!(store.verify_chain().map_err(ctx("verify"))?.records, 1);
        // Grenze dieses Tests: ein fremder Eigentümer lässt sich ohne
        // Root-Rechte nicht herstellen; `ensure_owned_directory` prüft ihn
        // (Test in `harw-fsutil`).
        Ok(())
    }

    #[test]
    fn symlinked_lock_and_anchor_files_are_never_followed() -> TestResult {
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        let finding_a = finding("finding-a")?;
        let store = store_with_segments(temp.path(), 2, 3, now)?;

        // `.lock` → fremde Datei: weder angelegt, noch beschrieben, noch per
        // `fchmod` geändert.
        let elsewhere = temp.path().join("elsewhere");
        std::fs::write(&elsewhere, b"fremd")?;
        std::fs::set_permissions(&elsewhere, Permissions::from_mode(0o644))?;
        let lock = store.root().join(LOCK_FILE);
        std::fs::remove_file(&lock)?;
        symlink(&elsewhere, &lock)?;
        expect_unsafe_directory(store.append(&verdict(&finding_a, "danach", now)))?;
        expect_unsafe_directory(store.apply_retention(now))?;
        assert_eq!(std::fs::read(&elsewhere)?, b"fremd");
        assert_eq!(mode_of(&elsewhere)?, 0o644);
        assert_eq!(segment_paths(&store)?.len(), 3);
        std::fs::remove_file(&lock)?;

        // `anchor.json` → Kopie eines gültigen Ankers: trotzdem `AnchorCorrupt`.
        store.apply_retention(now).map_err(ctx("retention"))?;
        let anchor = store.root().join(ANCHOR_FILE);
        let anchor_copy = temp.path().join("anchor-copy.json");
        std::fs::copy(&anchor, &anchor_copy)?;
        std::fs::remove_file(&anchor)?;
        symlink(&anchor_copy, &anchor)?;
        assert!(matches!(
            store.verify_chain(),
            Err(VerdictStoreError::AnchorCorrupt)
        ));
        assert!(matches!(
            store.append(&verdict(&finding_a, "danach", now)),
            Err(VerdictStoreError::AnchorCorrupt)
        ));
        Ok(())
    }

    #[test]
    fn anchor_is_written_through_a_fresh_temp_file_inside_the_root() -> TestResult {
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        let store = store_with_segments(temp.path(), 2, 3, now)?;
        // Ein Rest der Tempdatei aus einem abgebrochenen Lauf, sogar als
        // Symlink auf eine fremde Datei: wird entfernt, nie verfolgt.
        let elsewhere = temp.path().join("elsewhere");
        std::fs::write(&elsewhere, b"fremd")?;
        let stale = store.root().join(ANCHOR_TEMP_FILE);
        symlink(&elsewhere, &stale)?;

        let report = store.apply_retention(now).map_err(ctx("retention"))?;
        assert_eq!(report.removed_segments, 2);
        assert_eq!(report.first_seq, 3);
        assert!(std::fs::symlink_metadata(&stale).is_err());
        assert_eq!(std::fs::read(&elsewhere)?, b"fremd");
        let anchor = store.root().join(ANCHOR_FILE);
        assert!(std::fs::symlink_metadata(&anchor)?.is_file());
        assert_eq!(mode_of(&anchor)?, 0o600);
        let summary = store.verify_chain().map_err(ctx("verify"))?;
        assert_eq!(summary.first_seq, 3);
        assert_eq!(summary.records, 1);
        Ok(())
    }

    #[test]
    fn swapped_root_does_not_redirect_an_append_in_flight() -> TestResult {
        let temp = tempfile::tempdir()?;
        let store = VerdictStore::new(temp.path());
        let finding_a = finding("finding-a")?;
        let now = at(BASE_SECOND)?;
        store
            .append(&verdict(&finding_a, "eins", now))
            .map_err(ctx("append"))?;

        // Der Aufruf hat sein Store-Verzeichnis schon geöffnet und geprüft …
        let root = store.create_root().map_err(ctx("root"))?;
        // … dann wird es umbenannt und durch einen Symlink auf einen Köder
        // ersetzt.
        let moved = temp.path().join(DOD_DIR).join("verdicts.moved");
        std::fs::rename(store.root(), &moved)?;
        let decoy = temp.path().join("decoy");
        DirBuilder::new().mode(DIR_MODE).create(&decoy)?;
        symlink(&decoy, store.root())?;

        let prepared = prepare(&verdict(&finding_a, "zwei", now)).map_err(ctx("prepare"))?;
        let second = store
            .append_at(&root, prepared)
            .map_err(ctx("append in flight"))?;
        assert_eq!(second.seq, 2);
        // Sperre, Prüfung und Schreiben blieben im ursprünglichen Verzeichnis.
        assert_eq!(entry_count(&decoy)?, 0);
        let moved_segment = moved.join(segment_file_name(FIRST_VERDICT_SEQ));
        assert_eq!(
            parse_lines(&std::fs::read_to_string(&moved_segment)?)?.len(),
            2
        );
        assert!(moved.join(LOCK_FILE).exists());

        // Ein neuer Aufruf steigt neu ab und verweigert den Symlink.
        expect_unsafe_directory(store.append(&verdict(&finding_a, "drei", now)))?;
        expect_unsafe_directory(store.verify_chain())?;
        assert_eq!(entry_count(&decoy)?, 0);

        // Ein echtes, leeres Verzeichnis an der alten Stelle: der laufende
        // Aufruf schreibt weiter in das ursprüngliche.
        std::fs::remove_file(store.root())?;
        DirBuilder::new().mode(DIR_MODE).create(store.root())?;
        let prepared = prepare(&verdict(&finding_a, "drei", now)).map_err(ctx("prepare"))?;
        let third = store
            .append_at(&root, prepared)
            .map_err(ctx("append in flight after swap"))?;
        assert_eq!(third.seq, 3);
        assert_eq!(entry_count(store.root())?, 0);
        assert_eq!(
            parse_lines(&std::fs::read_to_string(&moved_segment)?)?.len(),
            3
        );
        Ok(())
    }

    #[test]
    fn retention_deletes_only_inside_the_established_root() -> TestResult {
        let temp = tempfile::tempdir()?;
        let now = at(BASE_SECOND)?;
        // Drei Segmente, 1–2 abgelaufen.
        let store = store_with_segments(temp.path(), 2, 3, now)?;
        let root = root_of(&store)?;

        // Zwischen Abstieg und Löschen wird das Verzeichnis gegen einen
        // Symlink auf einen Köder mit gleichnamigen Dateien getauscht.
        let moved = temp.path().join(DOD_DIR).join("verdicts.moved");
        std::fs::rename(store.root(), &moved)?;
        let decoy = temp.path().join("decoy");
        DirBuilder::new().mode(DIR_MODE).create(&decoy)?;
        let decoy_names = [segment_file_name(1), segment_file_name(2)];
        for name in &decoy_names {
            std::fs::write(decoy.join(name), b"decoy\n")?;
        }
        symlink(&decoy, store.root())?;

        let report = retention_at(&root, now).map_err(ctx("retention in flight"))?;
        assert_eq!(report.removed_segments, 2);
        assert_eq!(report.first_seq, 3);
        // Gelöscht und verankert wurde nur im ursprünglichen Verzeichnis.
        for name in &decoy_names {
            assert_eq!(std::fs::read(decoy.join(name))?, b"decoy\n");
            assert!(!moved.join(name).exists());
        }
        assert!(moved.join(segment_file_name(3)).exists());
        assert!(moved.join(ANCHOR_FILE).exists());
        assert!(!decoy.join(ANCHOR_FILE).exists());
        assert_eq!(entry_count(&decoy)?, 2);
        Ok(())
    }

    #[test]
    fn segment_names_round_trip_and_reject_foreign_names() -> TestResult {
        assert_eq!(segment_file_name(42), "verdicts-00000000000000000042.jsonl");
        assert_eq!(parse_segment_name(&segment_file_name(42)), Some(42));
        assert_eq!(
            parse_segment_name(&segment_file_name(u64::MAX)),
            Some(u64::MAX)
        );
        for foreign in [
            "anchor.json",
            ".lock",
            "verdicts-42.jsonl",
            "verdicts-00000000000000000000.jsonl",
            "verdicts-0000000000000000004x.jsonl",
            "verdicts-00000000000000000042.jsonl.corrupt-1",
        ] {
            assert_eq!(parse_segment_name(foreign), None);
        }
        Ok(())
    }

    #[test]
    fn corrupt_line_in_a_closed_segment_is_reported_without_content() -> TestResult {
        let bytes = b"{\"not\":\"a record\"}\n";
        let error = match parse_segment(bytes, 7, false) {
            Err(
                error @ VerdictStoreError::CorruptRecord {
                    segment_seq: 7,
                    line: 1,
                },
            ) => error,
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        };
        assert!(!error.to_string().contains("a record"));
        // Eine abgerissene Zeile mitten in der Kette (nicht im letzten Segment).
        assert!(matches!(
            parse_segment(b"{\"seq\":1", 1, false),
            Err(VerdictStoreError::CorruptRecord { line: 0, .. })
        ));
        assert!(
            parse_segment(b"{\"seq\":1", 1, true)
                .map_err(ctx("parse torn tail"))?
                .is_empty()
        );
        Ok(())
    }
}
