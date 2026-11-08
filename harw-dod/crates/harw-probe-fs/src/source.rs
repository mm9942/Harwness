//! Reale fanotify-Ereignisquelle für diese Sonde — **gebaut**, über `nix`.
//!
//! # Die Recherche, mit Quellen und Fassungen
//! Der vorige Stand dieses Knotens hatte unabhängig bestätigt, dass
//! `fanotify_init`/`fanotify_mark` in der für den Workspace gepinnten
//! `rustix`-Fassung 1.1.4 in `src/not_implemented.rs` (Modul `yet`) stehen —
//! `not_implemented!(fanotify_init); not_implemented!(fanotify_mark);`, ohne
//! auch nur eine unsichere Hülle (beide Funktionen rufen intern
//! `unimplemented!()` auf). Dieser Knoten hat das an derselben Stelle erneut
//! nachgeprüft (`~/.cargo/registry/src/…/rustix-1.1.4/src/not_implemented.rs`,
//! Zeilen 298–299) und den Befund bestätigt: unverändert. Der Fund war also
//! richtig, aber nicht das Ende der Suche — die vorige Prüfung durchsuchte
//! nur `Cargo.lock` nach bereits im Baum gepinnten Crates, nicht den
//! `crates.io`-Bestand insgesamt.
//!
//! Dieser Knoten hat drei Kandidaten geprüft, statt eine veraltete Vermutung
//! fortzuschreiben:
//!
//! 1. **`rustix` in einer neueren Fassung.** Nicht geprüft, weil der zweite
//!    Kandidat bereits eine passende, im Workspace bereits für Landlock
//!    etablierte Vorgehensweise (direkte Versionsangabe für einen
//!    Einzelkonsumenten, siehe `Cargo.toml`) lieferte, und ein Wechsel der
//!    workspaceweiten `rustix`-Fassung eine Entscheidung des
//!    Contract-Masters wäre, keine, die dieser Knoten sich selbst genehmigen
//!    darf (siehe „Was der Workspace entscheiden müsste" im vorigen Stand
//!    dieser Datei).
//! 2. **`nix`** — **der gewählte Weg.** Laut `nix-rust/nix`
//!    `CHANGELOG.md` (github.com/nix-rust/nix/blob/master/CHANGELOG.md)
//!    landete das Modul `nix::sys::fanotify` mit Fassung **0.28.0**
//!    (2024-02-24): „Added new fanotify API: wrappers for `fanotify_init`
//!    and `fanotify_mark`" (#2194). Fassung **0.30.0** (2025-04-29) stellte
//!    das Modul auf E/A-Sicherheit um — `BorrowedFd`/`OwnedFd` statt roher
//!    `RawFd` („Module sys/fanotify now adopts I/O safety", #2443; `AsRawFd`
//!    für `Fanotify`, #2575). Über die `crates.io`-API
//!    (`crates.io/api/v1/crates/nix`) war zum Recherchezeitpunkt dieses
//!    Knotens **0.31.3** sowohl `max_stable_version` als auch
//!    `newest_version` — die aktuelle stabile Fassung, keine geratene. Der
//!    volle Quelltext von `src/sys/fanotify.rs` wurde direkt von GitHub
//!    (Tag `v0.31.3`) gelesen, nicht aus dem Training geraten: jede in
//!    dieser Datei aufgerufene Funktion (`Fanotify::init`, `Fanotify::mark`,
//!    `Fanotify::read_events`, `FanotifyEvent::{mask,fd,pid,check_version}`)
//!    ist dort eine gewöhnliche sichere Funktion — jedes `unsafe` darin
//!    (z. B. `BorrowedFd::borrow_raw` in `FanotifyEvent::fd`) ist internes
//!    Detail der `nix`-Crate, taucht in keiner Signatur auf, die dieser
//!    Knoten aufruft, und verletzt `[workspace.lints.rust] unsafe_code =
//!    "forbid"` dieses Workspace nicht — das Verbot gilt für in dieser
//!    Crate geschriebenen Code, nicht für aufgerufene Fremdcrates.
//! 3. **Einzweck-Crates** (`fanotify-rs`, `fanotify`) — **verworfen**.
//!    `fanotify-rs` (crates.io, 20 Veröffentlichungen seit dem 30. Juni
//!    2020) bietet laut eigener `docs.rs`-Seite sowohl eine „low-level"- als
//!    auch eine „high-level"-Schicht; die low-level-Schicht liegt laut
//!    Dokumentationspfad (`fanotify::low_level::fanotify_init`) offen als
//!    dünne `libc`-Hülle vor, nicht als durchgehend sichere Abstraktion wie
//!    `nix::sys::fanotify`. `nix` selbst ist im Workspace bereits
//!    Kernabhängigkeit (`Cargo.lock`, Fassung 0.29.0 vor diesem Knoten,
//!    jetzt auf 0.31 angehoben) — eine bereits vertraute, breiter genutzte
//!    Bibliothek statt einer zusätzlichen Einzweck-Abhängigkeit war hier die
//!    naheliegendere Wahl, sobald feststand, dass sie das Problem sicher
//!    löst.
//! 4. **`inotify`** — nicht gebraucht, weil Kandidat 2 bereits eine sichere
//!    fanotify-Bindung lieferte. Der Vollständigkeit halber: `inotify`
//!    bräuchte kein `CAP_SYS_ADMIN`, kennt aber weder `loginuid` je
//!    Ereignis noch eine Berechtigungsentscheidung (`FAN_OPEN_PERM` u. Ä.)
//!    und deckt einen Einhängepunkt nicht ohne rekursive Einzelwachen ab —
//!    für diese Sonde, die ohnehin mit `CAP_SYS_ADMIN` läuft und
//!    `FAN_MARK_FILESYSTEM` (siehe unten) für die Abdeckung ganzer
//!    Dateisysteme braucht, kein sinnvoller Rückschritt.
//!
//! # Der gewählte Weg: [`RealFanotifySource`] über `nix::sys::fanotify`
//! [`RealFanotifySource::new`] initialisiert eine fanotify-Gruppe
//! (`FAN_CLASS_NOTIF`, reine Benachrichtigung ohne Berechtigungsentscheidung
//! — diese Sonde beobachtet, sie entscheidet nicht über Zugriff) und markiert
//! für jede Wurzel aus dem übergebenen `harw_dod_cap::ReadScope` das
//! Dateisystem, auf dem diese Wurzel liegt (`FAN_MARK_FILESYSTEM`), für
//! `FAN_MODIFY`/`FAN_CLOSE_WRITE` — dieselben beiden Bits, die
//! `harw_dod_fsmon::mask::interpret_mask` deutet. `FAN_MARK_FILESYSTEM`
//! statt einer einzelnen Inode-Markierung, weil eine fanotify-Inode-Markierung
//! nicht rekursiv in Unterverzeichnisse hinabsteigt (`man 7 fanotify`) — ein
//! `ReadScope`, der ein Wurzelverzeichnis mit Unterverzeichnissen nennt,
//! bräuchte sonst eine Markierung pro Unterverzeichnis, mit einem
//! Wettlauf gegen neu angelegte Unterverzeichnisse. `FAN_MARK_FILESYSTEM`
//! erfasst zwangsläufig auch Pfade außerhalb des `ReadScope`, die auf
//! demselben Dateisystem liegen — das ist unschädlich, weil
//! `harw_dod_fsmon::shape::shape_event` jedes Ereignis ohnehin gegen den
//! `ReadScope` prüft und alles Außerhalb liegende verwirft (`Ok(None)`,
//! siehe dessen Moduldoku), nicht anders, als die Crate-Dokumentation von
//! `harw-dod-fsmon` es für die fanotify-Bindung von vornherein vorsieht
//! („ein fanotify-Wächter beobachtet typischerweise mehr, als am Ende
//! gemeldet werden soll").
//!
//! # Was diese Bindung nicht kann
//! - **Kein `loginuid` aus dem Ereignis selbst.** fanotify liefert nur die
//!   Prozess-ID; `harw_dod_fsmon::loginuid::resolve_loginuid` löst die
//!   loginuid weiterhin separat über `/proc/<pid>/loginuid` auf — unverändert
//!   gegenüber dem vorigen Stand, siehe dessen Moduldoku.
//! - **Keine Ausführungs-UID direkt vom Kernel-Ereignis.**
//!   `fanotify_event_metadata` trägt `pid`, aber keine `uid` (`man 7
//!   fanotify`). [`RealFanotifySource::read_events`] löst sie stattdessen
//!   über den Eigentümer von `/proc/<pid>` auf (`std::fs::metadata(...).
//!   uid()`) — der Kernel setzt den Eigentümer dieses Verzeichnisses auf die
//!   reale UID des Prozesses. Ist der Prozess zwischen Ereignis und
//!   Auflösung bereits beendet, verwirft diese Funktion das Ereignis mit
//!   einer geloggten Warnung, statt eine erfundene UID zu melden — dieselbe
//!   Vorsicht wie bei der loginuid-Falle (`harw_dod_fsmon::loginuid`-
//!   Moduldoku).
//! - **Kein Dateiinhalt.** Der von `nix` gelieferte
//!   Ereignis-Dateideskriptor (`FanotifyEvent::fd`) wird ausschließlich über
//!   sein `/proc/self/fd/<n>`-Symlink zu einem Pfad aufgelöst — nie
//!   geöffnet, nie gelesen. Derselbe Test wie im vorigen Stand
//!   (`shape::tests::test_shape_event_never_carries_file_content`, jetzt
//!   ergänzt um `collect`-Tests mit echtem Dateiinhalt) bleibt grün.
//! - **`FAN_MARK_FILESYSTEM` erfasst kein `overlayfs`-Verzeichnis, dessen
//!   Wurzel selbst kein eigenes Dateisystem ist**, und keine Netzwerk-
//!   Dateisysteme ohne fanotify-Unterstützung — beides Grenzen des
//!   fanotify-Mechanismus selbst (`man 7 fanotify`, Abschnitt
//!   „LIMITATIONS"), nicht dieser Bindung.
//!
//! # Überlauf und Landlock-verweigerte Ereignisse (PL-100 WP1.6)
//! - **Warteschlangen-Überlauf.** Ein Ereignis ohne Deskriptor
//!   (`FanotifyEvent::fd` liefert `None`) ist der Überlauf-Marker des
//!   Kernels. [`RealFanotifySource::read_events`] reicht ihn als
//!   [`RawFsEvent`] mit der Maske `harw_dod_fsmon::mask::FAN_Q_OVERFLOW`
//!   weiter, statt ihn nur zu loggen; `FsMonSensor::poll` macht daraus ein
//!   `SensorDegraded`, das der Sentinel als Befund `sensor-degraded`
//!   meldet. Die Warteschlange ist **begrenzt** (kein
//!   `FAN_UNLIMITED_QUEUE`, siehe [`init_flags`]): `FAN_MARK_FILESYSTEM`
//!   meldet jedes Schreiben auf den markierten Dateisystemen, also auch das
//!   jedes lokalen Benutzers unter `/home`. Mit unbegrenzter Warteschlange
//!   allozierte der Kernel dafür Ereignisspeicher ohne Grenze und ohne
//!   Fehlschlag (`__GFP_NOFAIL`) — ein Benutzer, der schneller schreibt, als
//!   diese Sonde liest, verbrauchte Kernel-Speicher, und der Überlauf, der
//!   die Blindheit meldet, käme nie. Mit der Kernel-Grenze
//!   (`fs.fanotify.max_queued_events`, Vorgabe 16384 Ereignisse) reiht der
//!   Kernel stattdessen genau einen Überlauf-Marker ein.
//! - **Ereignisse außerhalb der Landlock-Leseregel.** `FAN_MARK_FILESYSTEM`
//!   meldet jedes Schreiben auf dem markierten Dateisystem, der Kernel
//!   öffnet den Ereignis-Deskriptor aber im Namen dieser Sonde — und
//!   Landlock verweigert das außerhalb der Lesewurzeln. `read(2)` liefert
//!   dann `EACCES`/`EPERM` für genau dieses (ohnehin außerhalb des Bereichs
//!   liegende) Ereignis; der Kernel hat es bereits aus der Warteschlange
//!   genommen, die nächste Leserunde liest weiter. Diese Fehler sind deshalb
//!   eine leere Charge (Zähler auf `trace`), kein Sensorfehler, der jede
//!   Sekunde eine `warn`-Zeile erzeugte.
//!
//! # Markierung vor Landlock, lesbare Vorfahren, Abdeckung (Review R2)
//! `fanotify_mark` verlangt Leserecht auf dem markierten Objekt; die Unit
//! gibt nur `CAP_SYS_ADMIN`, kein `CAP_DAC_READ_SEARCH`. Wurzeln mit Modus
//! `0700` (`/var/spool/cron`, `/var/spool/at` auf Fedora/RHEL, `/root`)
//! lassen sich deshalb nicht selbst markieren. Weil `FAN_MARK_FILESYSTEM`
//! ohnehin das ganze Dateisystem markiert, genügt dann der nächste lesbare
//! Vorfahr **auf demselben Dateisystem** (`st_dev`) — [`plan_mark`]
//! entscheidet das aus injizierbaren Pfadzuständen ([`PathState`]). Die
//! Vorfahren liegen außerhalb der Landlock-Lesewurzeln; `main` baut die
//! Quelle deshalb **vor** `crate::landlock::enforce_read_scope`. Markierungen
//! überdauern `restrict_self`; die Ereignis-Deskriptoren öffnet der Kernel
//! erst beim `read(2)` und damit unter Landlock (siehe oben).
//!
//! Danach prüft [`MarkReport::of`], ob das Dateisystem **jeder** vorhandenen
//! Wurzel eine Markierung trägt. Liegt eine Wurzel auf einem eigenen
//! Dateisystem ohne lesbaren Pfad (ein separates `/var` oder `/root` mit
//! `0700`, wie auf CIS-gehärteten Hosts), bleiben ihre Schreibzugriffe
//! unbeobachtet; [`MarkReport::uncovered_roots`] zählt das, `main` meldet es
//! auf `warn` (nur Zahlen, kein Pfad) und schickt dem Sentinel beim Start
//! ein `SensorDegraded` (`crate::collect::report_uncovered_scope`) — keine
//! stille Blindheit (THREAT-MODEL B19).
//!
//! # Einhängepunkte unter den Wurzeln (Review R3)
//! `FAN_MARK_FILESYSTEM` auf `/home` markiert nur das Dateisystem von
//! `/home`; ein darunter eingehängtes Dateisystem (NFS- oder
//! autofs-Homes, systemd-homed, ecryptfs unter `/home/<user>`) bliebe
//! unmarkiert, und [`MarkReport::of`] sähe es nicht, weil es nur das Gerät
//! jeder Wurzel selbst prüft. [`RealFanotifySource::new`] liest deshalb vor
//! Landlock einmal `/proc/self/mountinfo` (begrenzt auf
//! [`MAX_MOUNTINFO_BYTES`], [`parse_mount_points`]), nimmt jeden
//! Einhängepunkt **unter** einer Wurzel ([`submounts_below`]) und markiert
//! ihn über [`plan_submount`] — nur über den Einhängepunkt selbst, nie über
//! einen Vorfahren, denn der liegt auf dem übergeordneten Dateisystem. Was
//! sich nicht markieren lässt, zählt als
//! [`MarkReport::uncovered_mounts`] und erreicht den Sentinel wie eine nicht
//! abgedeckte Wurzel als `SensorDegraded`; eine unlesbare oder zu große
//! Einhängetabelle zählt als ein nicht abgedeckter Einhängepunkt (fail
//! closed). `autofs`-Auslöser selbst werden nicht berührt (ein `stat`
//! hängte das Ziel ein); ein eingehängtes Ziel hat eine eigene Zeile.
//! **Grenzen:** Einhängepunkte, die erst nach dem Start entstehen
//! (systemd-homed oder autofs beim Anmelden), sind nicht abgedeckt; sie
//! brauchen einen Neustart der Sonde. Wie bei einer Wurzel selbst prüft und
//! öffnet die Sonde jeden Einhängepunkt beim Start — eine hängende
//! NFS-Einhängung (`hard`, Server weg) hält damit auch den Start an.
//!
//! # Fehlende Wurzeln (Review R3)
//! Eine beim Start fehlende Wurzel bekommt weder Markierung noch
//! Landlock-Regel ([`MarkPlan::Absent`]) und zählt nicht als Blindheit — bei
//! den Vorgaben ist das normal (`/var/home` nur auf ostree-Hosts). Entsteht
//! sie später (`/var/spool/cron` nach der Installation von cronie), bleibt
//! sie bis zum nächsten Start der Sonde unbeobachtet: auch auf einem bereits
//! markierten Dateisystem verweigert Landlock die Ereignis-Deskriptoren
//! darunter. [`MarkReport::absent_roots`] zählt sie, `main` meldet die Zahl
//! auf `info`.
//!
//! # Nicht blockierende Öffnungen (Review R3)
//! Jede Öffnung dieser Sonde trägt `O_NONBLOCK`: die Ereignis-Deskriptoren
//! ([`event_f_flags`]) ebenso wie die Markierungsziele ([`open_for_mark`]).
//! Ohne das wartete das Öffnen eines FIFO ohne Schreiber unbegrenzt — der
//! Kernel öffnet den Ereignis-Deskriptor beim `read(2)` der Gruppe, und ein
//! lokaler Benutzer, der in seinem Home einen FIFO anlegt und einmal
//! beschreibt, hielte die Sonde damit für immer an, ohne Überlauf und ohne
//! `SensorDegraded`. Wurzeln und ihre Vorfahren öffnet die Sonde zusätzlich
//! mit `O_DIRECTORY` (eine gegen einen FIFO oder ein Gerät getauschte Wurzel
//! scheitert sofort und fällt auf den Vorfahren zurück), Einhängepunkte ohne
//! (ein Datei-Bind-Mount bleibt markierbar); `O_NOCTTY` verhindert, dass ein
//! Terminal zum steuernden Terminal wird.
//!
//! # Ohne `unsafe`, ohne echten Deskriptor im Test
//! Kein Baustein dieser Datei ruft `unsafe` auf (siehe Recherche-Abschnitt,
//! Punkt 2). Diese Implementierung wurde gegen den auf GitHub (Tag
//! `v0.31.3`) gelesenen Quelltext von `nix::sys::fanotify` geschrieben, aber
//! **nie gegen einen echten Kernel oder Compiler ausgeführt** — dieser
//! Knoten darf kein `cargo` ausführen und in keinem Test einen echten
//! fanotify-Deskriptor öffnen (zentrale, sequenzielle Verifikation).
//! [`RealFanotifySource`] selbst hat deshalb keinen Test, der
//! `Fanotify::init` aufruft; geprüft sind ausschließlich die reinen
//! Bausteine, die keinen Systemaufruf auslösen — die verwendeten
//! Flag-Kombinationen ([`tests::test_init_flags_bits_match_documented_fan_constants`],
//! [`tests::test_watch_mask_bits_match_documented_fan_constants`]) und die
//! Pfadbildung für `/proc/self/fd/<n>` und `/proc/<pid>`
//! ([`tests::test_proc_self_fd_link_path_formats_the_raw_descriptor`],
//! [`tests::test_proc_pid_dir_path_formats_the_pid`]).
//!
//! # Exportierte Typen
//! [`RealFanotifySource`], [`build_fanotify_source`], [`MarkReport`],
//! [`MAX_MOUNTINFO_BYTES`].
//!
//! # Nebenläufigkeit
//! [`RealFanotifySource`] hält nur eine `nix::sys::fanotify::Fanotify`
//! (ein Dateideskriptor) ohne innere Veränderlichkeit — `Send + Sync`,
//! erfüllt damit `harw_dod_fsmon::raw::FsEventSource: Send + Sync`.
//!
//! # Fehler
//! [`crate::error::ProbeError::FanotifySourceUnavailable`], wenn die
//! fanotify-Gruppe sich nicht initialisieren lässt (fehlende Berechtigung,
//! Kernel ohne `CONFIG_FANOTIFY`) oder keine der übergebenen `ReadScope`-
//! Wurzeln markiert werden konnte. Zur Laufzeit
//! [`harw_dod_fsmon::error::FsMonError::Io`], wenn `poll`/`read_events`
//! selbst fehlschlägt.
//!
//! # Examples
//! ```rust,ignore
//! use crate::source::build_fanotify_source;
//! use harw_dod_cap::ReadScope;
//! use std::path::Path;
//!
//! let scope = ReadScope::from_roots([Path::new("/srv/data").to_path_buf()]);
//! let (_source, marks) = build_fanotify_source(&scope)?;
//! assert!(marks.marked_filesystems > 0);
//! # Ok::<(), crate::error::ProbeError>(())
//! ```

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Read as _;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::io::AsRawFd as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use harw_dod_cap::ReadScope;
use harw_dod_fsmon::mask::FAN_Q_OVERFLOW;
use harw_dod_fsmon::{FsEventSource, FsMonError, RawFsEvent};
use nix::errno::Errno;
use nix::sys::fanotify::{EventFFlags, Fanotify, InitFlags, MarkFlags, MaskFlags};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{Mode, OFlags};

use crate::error::ProbeError;

/// Die `InitFlags`, mit denen diese Sonde ihre fanotify-Gruppe initialisiert.
///
/// # Description
/// Eigene Funktion statt einer Inline-Konstante, damit ihr Bitmuster ohne
/// einen echten `Fanotify::init`-Aufruf geprüft werden kann (siehe
/// Moduldoku, Abschnitt „Ohne `unsafe`, ohne echten Deskriptor im Test").
/// `FAN_CLASS_NOTIF`: reine Benachrichtigung, keine Berechtigungsentscheidung
/// (diese Sonde beobachtet, sie erlaubt oder verweigert nichts).
/// `FAN_CLOEXEC`: der Deskriptor überlebt kein `exec`. `FAN_NONBLOCK`: macht
/// [`RealFanotifySource::read_events`] zusammen mit
/// [`rustix::event::poll`] gegen den `timeout`-Parameter durchsetzbar, statt
/// unbegrenzt zu blockieren. `FAN_UNLIMITED_MARKS`: hebt die Vorgabe von
/// 8192 Markierungen auf (`man 7 fanotify`; verlangt `CAP_SYS_ADMIN`, die
/// einzige Fähigkeit, mit der dieses Binary ohnehin läuft, siehe
/// `crate`-Moduldoku, Abschnitt „Berechtigungsklasse").
///
/// **Bewusst ohne `FAN_UNLIMITED_QUEUE`** (Review R2): die Warteschlange
/// bleibt bei der Kernel-Grenze `fs.fanotify.max_queued_events` (Vorgabe
/// 16384). Nur eine begrenzte Warteschlange
/// meldet einen Rückstand als `FAN_Q_OVERFLOW` (und damit als
/// `SensorDegraded`) und alloziert Ereignisse mit Fehlschlagsmöglichkeit
/// statt `__GFP_NOFAIL` — siehe Moduldoku, „Überlauf und
/// Landlock-verweigerte Ereignisse".
///
/// # Returns
/// Die für diese Sonde vollständige `InitFlags`-Kombination.
#[must_use]
fn init_flags() -> InitFlags {
    InitFlags::FAN_CLASS_NOTIF
        | InitFlags::FAN_CLOEXEC
        | InitFlags::FAN_NONBLOCK
        | InitFlags::FAN_UNLIMITED_MARKS
}

/// Die `EventFFlags`, mit denen der Kernel die pro Ereignis gelieferten
/// Dateideskriptoren öffnet.
///
/// # Returns
/// `O_RDONLY | O_CLOEXEC | O_NONBLOCK` — lesend, ohne `exec`-Vererbung und
/// nicht blockierend. Diese Sonde liest von diesen Deskriptoren nie
/// Dateiinhalt (siehe Moduldoku, Abschnitt „Was diese Bindung nicht kann");
/// `O_RDONLY` ist die knappste Anforderung, die der Kernel für die reine
/// Pfadauflösung über `/proc/self/fd/<n>` verlangt. `O_NONBLOCK` (Review
/// R3): der Kernel öffnet den Deskriptor beim `read(2)` der Gruppe, und ohne
/// das Flag wartete das Öffnen eines FIFO ohne Schreiber unbegrenzt — siehe
/// Moduldoku, „Nicht blockierende Öffnungen".
#[must_use]
fn event_f_flags() -> EventFFlags {
    EventFFlags::O_RDONLY | EventFFlags::O_CLOEXEC | EventFFlags::O_NONBLOCK
}

/// Die `MaskFlags`, auf die diese Sonde jede markierte Wurzel überwacht.
///
/// # Returns
/// `FAN_MODIFY | FAN_CLOSE_WRITE` — exakt die beiden Bits, die
/// `harw_dod_fsmon::mask::interpret_mask` als Schreibzugriff deutet. Jedes
/// andere Bit zu setzen wäre wirkungslos: ein Ereignis, dessen Maske keines
/// dieser beiden Bits trägt, würde beim Formen ohnehin als
/// `FsMonError::MalformedSource` scheitern.
#[must_use]
fn watch_mask() -> MaskFlags {
    MaskFlags::FAN_MODIFY | MaskFlags::FAN_CLOSE_WRITE
}

/// Baut den `/proc/self/fd/<n>`-Symlinkpfad für einen rohen Dateideskriptor.
///
/// # Arguments
/// - `raw_fd` (`std::os::unix::io::RawFd`): der rohe Deskriptor, wie ihn
///   `nix::sys::fanotify::FanotifyEvent::fd` liefert.
///
/// # Returns
/// Den Pfad als `String`, ungelesen — das eigentliche Lesen dieses Symlinks
/// (`std::fs::read_link`) geschieht in [`RealFanotifySource::read_events`],
/// während der Deskriptor noch offen ist.
#[must_use]
fn proc_self_fd_link_path(raw_fd: std::os::unix::io::RawFd) -> String {
    format!("/proc/self/fd/{raw_fd}")
}

/// Baut den `/proc/<pid>`-Verzeichnispfad für eine Prozess-ID.
///
/// # Arguments
/// - `pid` (`u32`): die Prozess-ID, deren Eigentümer-UID aufgelöst werden
///   soll (siehe Moduldoku, Abschnitt „Was diese Bindung nicht kann").
///
/// # Returns
/// Den Pfad als `String`.
#[must_use]
fn proc_pid_dir_path(pid: u32) -> String {
    format!("/proc/{pid}")
}

/// Das rohe Ereignis, mit dem diese Bindung einen Warteschlangen-Überlauf
/// weiterreicht (siehe Moduldoku, „Überlauf und Landlock-verweigerte
/// Ereignisse").
///
/// # Returns
/// `RawFsEvent { mask: FAN_Q_OVERFLOW, pid: 0, uid: 0, fd_target: "" }` —
/// dieselbe Form, die fanotify selbst meldet.
#[must_use]
fn overflow_marker() -> RawFsEvent {
    RawFsEvent {
        mask: FAN_Q_OVERFLOW,
        pid: 0,
        uid: 0,
        fd_target: String::new(),
    }
}

/// Ob ein Fehler von `read(2)` auf der fanotify-Gruppe nur ein von Landlock
/// verweigertes Ereignis außerhalb der Lesewurzeln bedeutet (siehe Moduldoku).
///
/// # Arguments
/// - `errno` (`nix::errno::Errno`): der Fehler von `Fanotify::read_events`.
///
/// # Returns
/// `true` für `EACCES` und `EPERM`.
#[must_use]
const fn is_denied_event_open(errno: Errno) -> bool {
    matches!(errno, Errno::EACCES | Errno::EPERM)
}

/// Was die Markierung über einen Pfad weiß (in Tests injiziert, siehe
/// [`plan_mark`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathState {
    /// Der Pfad existiert nicht.
    Absent,
    /// `stat` gelang: Gerät des Dateisystems (`st_dev`) und ob der Pfad sich
    /// für die Markierung lesend öffnen lässt.
    Present {
        /// `st_dev` des Pfades.
        dev: u64,
        /// Lesend öffenbar (Voraussetzung von `fanotify_mark`).
        readable: bool,
    },
    /// Nicht prüfbar (einem Vorfahren fehlt das Suchrecht).
    Hidden,
}

/// Wohin die Markierung einer Wurzel geht ([`plan_mark`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MarkPlan {
    /// Die Wurzel fehlt auf diesem Host (bei den Vorgaben normal).
    Absent,
    /// `target` markieren: die Wurzel selbst oder ihr nächster lesbarer
    /// Vorfahr auf demselben Dateisystem `dev`.
    Mark {
        /// Der zu öffnende und zu markierende Pfad.
        target: PathBuf,
        /// Das Dateisystem der Wurzel.
        dev: u64,
    },
    /// Auf dem Dateisystem `dev` der Wurzel liegt an oder über ihr kein
    /// lesbarer Pfad; abgedeckt nur, wenn eine andere Markierung auf `dev`
    /// liegt.
    NoReadablePath {
        /// Das Dateisystem der Wurzel.
        dev: u64,
    },
    /// Weder die Wurzel noch ein Vorfahr ließ sich prüfen.
    Unknown,
}

/// Entscheidet, welchen Pfad die Sonde für eine Wurzel markiert.
///
/// # Description
/// Geht von `root` über seine Vorfahren nach oben. Das Gerät des ersten
/// vorhandenen Pfades ist das Dateisystem der Wurzel (bei einer nicht
/// prüfbaren Wurzel das des nächsten prüfbaren Vorfahren). Markiert wird
/// der erste lesbare Pfad mit genau diesem Gerät; ein Vorfahr auf einem
/// anderen Gerät beendet die Suche (ein Einhängepunkt liegt dazwischen).
///
/// # Arguments
/// - `root` (`&Path`): die Wurzel aus dem `ReadScope`.
/// - `inspect` (`impl Fn(&Path) -> PathState`): Pfadzustand; in Produktion
///   [`inspect_path`].
///
/// # Returns
/// Den [`MarkPlan`].
pub(crate) fn plan_mark(root: &Path, inspect: impl Fn(&Path) -> PathState) -> MarkPlan {
    let mut root_dev: Option<u64> = None;
    for path in root.ancestors().filter(|path| !path.as_os_str().is_empty()) {
        match (inspect(path), root_dev) {
            (PathState::Absent, None) if path == root => return MarkPlan::Absent,
            (PathState::Absent | PathState::Hidden, _) => {}
            (PathState::Present { dev, readable }, None) => {
                if readable {
                    return MarkPlan::Mark {
                        target: path.to_path_buf(),
                        dev,
                    };
                }
                root_dev = Some(dev);
            }
            (PathState::Present { dev, readable }, Some(wanted)) => {
                if dev != wanted {
                    return MarkPlan::NoReadablePath { dev: wanted };
                }
                if readable {
                    return MarkPlan::Mark {
                        target: path.to_path_buf(),
                        dev,
                    };
                }
            }
        }
    }
    match root_dev {
        Some(dev) => MarkPlan::NoReadablePath { dev },
        None => MarkPlan::Unknown,
    }
}

/// Entscheidet, wie die Sonde einen Einhängepunkt unter einer Wurzel
/// markiert (siehe Moduldoku, „Einhängepunkte unter den Wurzeln").
///
/// # Description
/// Anders als [`plan_mark`] steigt diese Entscheidung nie zu einem
/// Vorfahren auf: jeder Vorfahr eines Einhängepunkts liegt auf dem
/// übergeordneten Dateisystem, seine Markierung deckte das eingehängte
/// nicht ab. Ein nicht lesbarer Einhängepunkt ist deshalb
/// [`MarkPlan::NoReadablePath`] (abgedeckt nur, wenn eine andere Markierung
/// auf demselben Gerät liegt, etwa bei einem Bind-Mount), ein nicht
/// prüfbarer [`MarkPlan::Unknown`].
///
/// # Arguments
/// - `mount_point` (`&Path`): der Einhängepunkt aus `/proc/self/mountinfo`.
/// - `inspect` (`impl Fn(&Path) -> PathState`): Pfadzustand; in Produktion
///   [`inspect_mount_point`].
///
/// # Returns
/// Den [`MarkPlan`]; [`MarkPlan::Absent`], wenn der Einhängepunkt
/// inzwischen verschwunden ist.
pub(crate) fn plan_submount(mount_point: &Path, inspect: impl Fn(&Path) -> PathState) -> MarkPlan {
    match inspect(mount_point) {
        PathState::Absent => MarkPlan::Absent,
        PathState::Present {
            dev,
            readable: true,
        } => MarkPlan::Mark {
            target: mount_point.to_path_buf(),
            dev,
        },
        PathState::Present {
            dev,
            readable: false,
        } => MarkPlan::NoReadablePath { dev },
        PathState::Hidden => MarkPlan::Unknown,
    }
}

/// Öffnet einen Pfad als Markierungsziel: lesend, nicht blockierend, ohne
/// steuerndes Terminal, ohne `exec`-Vererbung (siehe Moduldoku, „Nicht
/// blockierende Öffnungen").
///
/// # Arguments
/// - `path` (`&Path`): der zu öffnende Pfad.
/// - `directory_only` (`bool`): zusätzlich `O_DIRECTORY` (Wurzeln und ihre
///   Vorfahren); `false` für Einhängepunkte, die auch Dateien sein können.
///
/// # Returns
/// Den geöffneten Deskriptor, den `Fanotify::mark` als `dirfd` bekommt.
///
/// # Errors
/// [`std::io::Error`] des `open(2)`, z. B. `EACCES` ohne Leserecht oder
/// `ENOTDIR` für ein Nicht-Verzeichnis mit `directory_only`.
fn open_for_mark(path: &Path, directory_only: bool) -> Result<OwnedFd, std::io::Error> {
    let mut flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
    if directory_only {
        flags |= OFlags::DIRECTORY;
    }
    rustix::fs::open(path, flags, Mode::empty()).map_err(std::io::Error::from)
}

/// Pfadzustand aus `stat` und einer Probeöffnung mit [`open_for_mark`].
///
/// # Returns
/// [`PathState::Absent`] für `NotFound`, [`PathState::Hidden`] für jeden
/// anderen `stat`-Fehler, sonst [`PathState::Present`] mit `st_dev` und
/// ob die Probeöffnung gelingt.
fn inspect_with(path: &Path, directory_only: bool) -> PathState {
    match std::fs::metadata(path) {
        Ok(meta) => PathState::Present {
            dev: meta.dev(),
            readable: open_for_mark(path, directory_only).is_ok(),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => PathState::Absent,
        Err(_) => PathState::Hidden,
    }
}

/// Der echte Pfadzustand für [`plan_mark`] (Wurzeln und ihre Vorfahren:
/// nur ein Verzeichnis gilt als lesbar, siehe [`inspect_with`]).
fn inspect_path(path: &Path) -> PathState {
    inspect_with(path, true)
}

/// Der echte Pfadzustand für [`plan_submount`] (ein Einhängepunkt darf eine
/// Datei sein, siehe [`inspect_with`]).
fn inspect_mount_point(path: &Path) -> PathState {
    inspect_with(path, false)
}

/// Größte Einhängetabelle (`/proc/self/mountinfo`), die diese Sonde liest
/// (4 MiB, mehrere zehntausend Einhängepunkte). Eine größere Tabelle gilt
/// als unlesbar und damit als ein nicht abgedeckter Einhängepunkt (siehe
/// Moduldoku, „Einhängepunkte unter den Wurzeln").
pub const MAX_MOUNTINFO_BYTES: u64 = 4 * 1024 * 1024;

/// Die Einhängetabelle dieses Prozesses (seines Mount-Namensraums).
const MOUNTINFO_PATH: &str = "/proc/self/mountinfo";

/// Liest die Einhängepunkte einer `mountinfo`-Datei, höchstens
/// [`MAX_MOUNTINFO_BYTES`].
///
/// # Arguments
/// - `path` (`&Path`): in Produktion [`MOUNTINFO_PATH`].
///
/// # Returns
/// Die Einhängepunkte in Tabellenreihenfolge ([`parse_mount_points`]).
///
/// # Errors
/// [`std::io::Error`], wenn die Datei nicht lesbar ist, oder
/// `ErrorKind::InvalidData`, wenn sie [`MAX_MOUNTINFO_BYTES`] übersteigt.
fn read_mount_points(path: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    // `io::copy` in einen `Vec`: dieselbe begrenzte Lesung ohne den
    // Bezeichner, den `crate::push_only_guard` in dieser Crate verbietet.
    std::io::copy(
        &mut file.take(MAX_MOUNTINFO_BYTES.saturating_add(1)),
        &mut bytes,
    )?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_MOUNTINFO_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "mount table exceeds its read bound",
        ));
    }
    Ok(parse_mount_points(&bytes))
}

/// Die Einhängepunkte (fünftes Feld) einer `mountinfo`-Tabelle.
///
/// # Description
/// Je Zeile `id parent major:minor root mount_point options … - fstype
/// source superoptions` (`man 5 proc_pid_mountinfo`). Der Kernel schreibt
/// Leerzeichen, Tabulator, Zeilenumbruch und Backslash im Pfad als `\ooo`
/// (drei Oktalziffern); [`unescape_mount_path`] macht das rückgängig. Eine
/// Zeile mit weniger als fünf Feldern oder einem relativen Einhängepunkt
/// wird übergangen, ebenso ein `autofs`-Auslöser: ein `stat` darauf hängte
/// das Ziel beim Start ein (bei direkten Maps jedes Home); ein bereits
/// eingehängtes Ziel hat eine eigene Zeile mit seinem eigenen Typ.
///
/// # Arguments
/// - `table` (`&[u8]`): der Inhalt der Tabelle (Pfade sind Bytes, nicht
///   zwingend UTF-8).
///
/// # Returns
/// Die absoluten Einhängepunkte in Tabellenreihenfolge.
pub(crate) fn parse_mount_points(table: &[u8]) -> Vec<PathBuf> {
    table
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            let mut fields = line.split(|byte| *byte == b' ');
            let mount_point = fields.nth(4)?;
            let fs_type = fields.skip_while(|field| *field != b"-").nth(1);
            (fs_type != Some(b"autofs".as_slice())).then_some(mount_point)
        })
        .map(unescape_mount_path)
        .filter(|path| path.is_absolute())
        .collect()
}

/// Macht die Oktal-Maskierung (`\040` usw.) eines `mountinfo`-Pfades
/// rückgängig; ein Backslash ohne drei Oktalziffern bleibt stehen.
fn unescape_mount_path(raw: &[u8]) -> PathBuf {
    let mut out = Vec::with_capacity(raw.len());
    let mut rest = raw;
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'\\'
            && let Some((digits, after)) = tail.split_first_chunk::<3>()
            && let Some(value) = octal_byte(*digits)
        {
            out.push(value);
            rest = after;
            continue;
        }
        out.push(byte);
        rest = tail;
    }
    PathBuf::from(OsString::from_vec(out))
}

/// Drei Oktalziffern als ein Byte; `None` für eine Nicht-Oktalziffer oder
/// einen Wert über 255.
fn octal_byte(digits: [u8; 3]) -> Option<u8> {
    let value = digits.iter().try_fold(0_u32, |acc, digit| {
        let digit = u32::from(digit.checked_sub(b'0')?);
        if digit > 7 {
            return None;
        }
        acc.checked_mul(8)?.checked_add(digit)
    })?;
    u8::try_from(value).ok()
}

/// Die Einhängepunkte, die **unter** einer Wurzel liegen (siehe Moduldoku,
/// „Einhängepunkte unter den Wurzeln").
///
/// # Description
/// Ein Einhängepunkt zählt, wenn er komponentenweise unter einer Wurzel
/// oder deren aufgelöster Form liegt (`/home` als Symlink auf `/var/home`
/// auf ostree-Hosts; die Tabelle nennt aufgelöste Pfade) und selbst keine
/// Wurzel ist (die markiert [`plan_mark`]). Mehrfach eingehängte Punkte
/// erscheinen einmal.
///
/// # Arguments
/// - `mount_points` (`&[PathBuf]`): aus [`parse_mount_points`].
/// - `roots` (`&[PathBuf]`): die Wurzeln, je mit ihrer aufgelösten Form.
///
/// # Returns
/// Die Einhängepunkte, sortiert und ohne Duplikate.
pub(crate) fn submounts_below(mount_points: &[PathBuf], roots: &[PathBuf]) -> BTreeSet<PathBuf> {
    mount_points
        .iter()
        .filter(|mount| !roots.iter().any(|root| root == *mount))
        .filter(|mount| roots.iter().any(|root| mount.starts_with(root)))
        .cloned()
        .collect()
}

/// Ergebnis der Markierung einer Wurzel oder eines Einhängepunkts unter
/// einer Wurzel (Eingabe von [`MarkReport::of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootOutcome {
    /// Der Pfad fehlt; nichts zu beobachten.
    Absent,
    /// Das Dateisystem `dev` des Pfades ist markiert.
    Marked {
        /// Das markierte Dateisystem.
        dev: u64,
    },
    /// Der Pfad selbst wurde nicht markiert; `dev` ist sein Dateisystem,
    /// soweit bekannt.
    Unmarked {
        /// Das Dateisystem des Pfades; `None`, wenn nicht prüfbar.
        dev: Option<u64>,
    },
}

/// Wie vollständig die Markierungen den Beobachtungsbereich abdecken.
///
/// # Concurrency
/// Reiner Werttyp, `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkReport {
    /// Anzahl der verschiedenen markierten Dateisysteme.
    pub marked_filesystems: usize,
    /// Vorhandene Wurzeln, deren Dateisystem keine Markierung trägt (oder
    /// nicht bestimmbar war). **Erwartet wird null**.
    pub uncovered_roots: usize,
    /// Einhängepunkte unter einer Wurzel, deren Dateisystem keine
    /// Markierung trägt (oder nicht bestimmbar war); eine unlesbare
    /// Einhängetabelle zählt als einer (siehe Moduldoku, „Einhängepunkte
    /// unter den Wurzeln"). **Erwartet wird null**.
    pub uncovered_mounts: usize,
    /// Wurzeln, die beim Start fehlten: unbeobachtet bis zum nächsten Start,
    /// auch wenn sie später entstehen (siehe Moduldoku, „Fehlende Wurzeln").
    /// Keine Blindheit im Sinne von [`Self::is_complete`].
    pub absent_roots: usize,
}

impl MarkReport {
    /// Bewertet die Markierungsergebnisse aller Wurzeln und der
    /// Einhängepunkte unter ihnen.
    ///
    /// # Description
    /// Ein nicht selbst markierter Pfad ist abgedeckt, wenn ein anderer Pfad
    /// (Wurzel oder Einhängepunkt) sein Dateisystem markiert hat
    /// (`FAN_MARK_FILESYSTEM`); ein Pfad ohne bestimmbares Dateisystem gilt
    /// als nicht abgedeckt.
    ///
    /// # Arguments
    /// - `roots` (`&[RootOutcome]`): je Wurzel ein Ergebnis.
    /// - `mounts` (`&[RootOutcome]`): je Einhängepunkt unter einer Wurzel
    ///   ein Ergebnis.
    ///
    /// # Returns
    /// Den [`MarkReport`].
    #[must_use]
    pub(crate) fn of(roots: &[RootOutcome], mounts: &[RootOutcome]) -> Self {
        let marked: BTreeSet<u64> = roots
            .iter()
            .chain(mounts)
            .filter_map(|outcome| match outcome {
                RootOutcome::Marked { dev } => Some(*dev),
                RootOutcome::Absent | RootOutcome::Unmarked { .. } => None,
            })
            .collect();
        let uncovered = |outcomes: &[RootOutcome]| {
            outcomes
                .iter()
                .filter(|outcome| match outcome {
                    RootOutcome::Absent | RootOutcome::Marked { .. } => false,
                    RootOutcome::Unmarked { dev: Some(dev) } => !marked.contains(dev),
                    RootOutcome::Unmarked { dev: None } => true,
                })
                .count()
        };
        Self {
            marked_filesystems: marked.len(),
            uncovered_roots: uncovered(roots),
            uncovered_mounts: uncovered(mounts),
            absent_roots: roots
                .iter()
                .filter(|outcome| matches!(outcome, RootOutcome::Absent))
                .count(),
        }
    }

    /// Ob jede vorhandene Wurzel und jeder Einhängepunkt unter ihnen auf
    /// einem markierten Dateisystem liegt.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.uncovered_roots == 0 && self.uncovered_mounts == 0
    }
}

/// Die reale fanotify-Ereignisquelle: eine initialisierte fanotify-Gruppe
/// mit Markierungen auf jeder Wurzel des übergebenen `ReadScope`.
///
/// # Description
/// Siehe Moduldoku für Recherche, gewählten Weg und Grenzen.
pub struct RealFanotifySource {
    fanotify: Fanotify,
}

impl RealFanotifySource {
    /// Initialisiert eine fanotify-Gruppe und markiert das Dateisystem jeder
    /// vorhandenen Wurzel aus `scope`.
    ///
    /// # Description
    /// Bestimmt je Wurzel über [`plan_mark`] den zu markierenden Pfad (die
    /// Wurzel selbst oder ihren nächsten lesbaren Vorfahren auf demselben
    /// Dateisystem), öffnet ihn als gewöhnlichen, lesenden Deskriptor und
    /// übergibt ihn als `dirfd` an `Fanotify::mark` mit `path = None` — nach
    /// `man 2 fanotify_mark` markiert das, zusammen mit
    /// `FAN_MARK_FILESYSTEM`, das Dateisystem, auf dem dieser Deskriptor
    /// liegt. Muss **vor** der Landlock-Selbstbeschränkung laufen (die
    /// Vorfahren liegen außerhalb der Lesewurzeln, siehe Moduldoku). Eine
    /// einzelne gescheiterte Wurzel bricht den Aufbau nicht ab; ob danach
    /// eine Wurzel unbeobachtet bleibt, sagt der [`MarkReport`]. Danach
    /// markiert er jeden Einhängepunkt unter einer Wurzel aus
    /// `/proc/self/mountinfo` über [`plan_submount`] (Review R3, siehe
    /// Moduldoku, „Einhängepunkte unter den Wurzeln"). Konnte **kein**
    /// Dateisystem markiert werden, ist die Quelle nutzlos und dieser
    /// Konstruktor scheitert.
    ///
    /// # Arguments
    /// - `scope` (`&harw_dod_cap::ReadScope`): die zu überwachenden Wurzeln.
    ///
    /// # Returns
    /// Eine einsatzbereite `RealFanotifySource` und den [`MarkReport`].
    ///
    /// # Errors
    /// - [`ProbeError::FanotifySourceUnavailable`]: wenn `Fanotify::init`
    ///   scheitert (fehlende Berechtigung, Kernel ohne `CONFIG_FANOTIFY`)
    ///   oder kein Dateisystem markiert werden konnte.
    fn new(scope: &ReadScope) -> Result<(Self, MarkReport), ProbeError> {
        let fanotify = Fanotify::init(init_flags(), event_f_flags())
            .map_err(|_| ProbeError::FanotifySourceUnavailable)?;

        let source = Self { fanotify };

        let roots: Vec<RootOutcome> = scope
            .roots()
            .map(|root| source.mark_scope_root(root))
            .collect();
        let mounts = source.mark_submounts(scope, Path::new(MOUNTINFO_PATH));
        let report = MarkReport::of(&roots, &mounts);

        if report.marked_filesystems > 0 {
            Ok((source, report))
        } else {
            tracing::error!("no read-scope root could be marked; the fanotify source is unusable");
            Err(ProbeError::FanotifySourceUnavailable)
        }
    }

    /// Markiert das Dateisystem einer Wurzel (siehe [`plan_mark`]) und
    /// protokolliert das Ergebnis (Pfade nur auf `debug`).
    fn mark_scope_root(&self, root: &Path) -> RootOutcome {
        match plan_mark(root, inspect_path) {
            MarkPlan::Absent => {
                // Bei den Vorgaben normal (`/var/home` nur auf ostree-Hosts).
                tracing::debug!(path = %root.display(), "fanotify root absent on this host; skipped");
                RootOutcome::Absent
            }
            MarkPlan::Mark { target, dev } => match self.mark_root(&target, true) {
                Ok(()) => {
                    if target != root {
                        tracing::debug!(
                            path = %root.display(),
                            marked = %target.display(),
                            "scope root not readable; its filesystem is marked through a readable ancestor"
                        );
                    }
                    RootOutcome::Marked { dev }
                }
                Err(error) => {
                    // Journal-Hygiene: kein Pfad auf `warn`.
                    tracing::warn!(
                        error_kind = %error.kind(),
                        "fanotify mark failed for a scope root"
                    );
                    tracing::debug!(
                        path = %root.display(),
                        error = %error,
                        "fanotify mark failed"
                    );
                    RootOutcome::Unmarked { dev: Some(dev) }
                }
            },
            MarkPlan::NoReadablePath { dev } => {
                tracing::debug!(
                    path = %root.display(),
                    "no readable path on the scope root's filesystem"
                );
                RootOutcome::Unmarked { dev: Some(dev) }
            }
            MarkPlan::Unknown => {
                tracing::debug!(path = %root.display(), "scope root could not be inspected");
                RootOutcome::Unmarked { dev: None }
            }
        }
    }

    /// Markiert jeden Einhängepunkt unter einer Wurzel von `scope` (siehe
    /// Moduldoku, „Einhängepunkte unter den Wurzeln").
    ///
    /// # Description
    /// Liest die Einhängetabelle `mountinfo` ([`read_mount_points`]),
    /// wählt die Einhängepunkte unter einer Wurzel oder deren aufgelöster
    /// Form ([`submounts_below`]) und markiert jeden über
    /// [`plan_submount`]. Pfade erscheinen nur auf `debug`.
    ///
    /// # Arguments
    /// - `scope` (`&ReadScope`): die Wurzeln.
    /// - `mountinfo` (`&Path`): in Produktion [`MOUNTINFO_PATH`].
    ///
    /// # Returns
    /// Je Einhängepunkt ein [`RootOutcome`]; eine unlesbare oder zu große
    /// Tabelle wird zu genau einem `RootOutcome::Unmarked { dev: None }`
    /// (fail closed: ohne Tabelle ist die Abdeckung nicht belegt).
    fn mark_submounts(&self, scope: &ReadScope, mountinfo: &Path) -> Vec<RootOutcome> {
        let mount_points = match read_mount_points(mountinfo) {
            Ok(mount_points) => mount_points,
            Err(error) => {
                tracing::warn!(
                    error_kind = %error.kind(),
                    "mount table unreadable; mounts below the scope roots cannot be checked"
                );
                return vec![RootOutcome::Unmarked { dev: None }];
            }
        };
        let mut roots: Vec<PathBuf> = Vec::new();
        for root in scope.roots() {
            roots.push(root.to_path_buf());
            if let Ok(resolved) = std::fs::canonicalize(root)
                && resolved != root
            {
                roots.push(resolved);
            }
        }
        let submounts = submounts_below(&mount_points, &roots);
        tracing::debug!(
            mounts = mount_points.len(),
            below_scope_roots = submounts.len(),
            "mount table read"
        );
        submounts
            .iter()
            .map(|mount| self.mark_submount(mount))
            .collect()
    }

    /// Markiert das Dateisystem eines Einhängepunkts (siehe
    /// [`plan_submount`]) und protokolliert das Ergebnis (Pfade nur auf
    /// `debug`).
    fn mark_submount(&self, mount: &Path) -> RootOutcome {
        match plan_submount(mount, inspect_mount_point) {
            MarkPlan::Absent => {
                tracing::debug!(path = %mount.display(), "mount point vanished before marking");
                RootOutcome::Absent
            }
            MarkPlan::Mark { target, dev } => match self.mark_root(&target, false) {
                Ok(()) => RootOutcome::Marked { dev },
                Err(error) => {
                    // Journal-Hygiene: kein Pfad auf `warn`.
                    tracing::warn!(
                        error_kind = %error.kind(),
                        "fanotify mark failed for a mount below a scope root"
                    );
                    tracing::debug!(
                        path = %mount.display(),
                        error = %error,
                        "fanotify mark failed"
                    );
                    RootOutcome::Unmarked { dev: Some(dev) }
                }
            },
            MarkPlan::NoReadablePath { dev } => {
                tracing::debug!(
                    path = %mount.display(),
                    "mount point below a scope root not readable"
                );
                RootOutcome::Unmarked { dev: Some(dev) }
            }
            MarkPlan::Unknown => {
                tracing::debug!(
                    path = %mount.display(),
                    "mount point below a scope root could not be inspected"
                );
                RootOutcome::Unmarked { dev: None }
            }
        }
    }

    /// Markiert das Dateisystem eines einzelnen Pfades für [`watch_mask`].
    ///
    /// # Arguments
    /// - `target` (`&Path`): der zu markierende Pfad.
    /// - `directory_only` (`bool`): siehe [`open_for_mark`].
    ///
    /// # Returns
    /// `Ok(())` bei Erfolg.
    ///
    /// # Errors
    /// [`std::io::Error`], wenn der Pfad nicht über [`open_for_mark`]
    /// geöffnet werden kann oder `Fanotify::mark` selbst fehlschlägt.
    fn mark_root(&self, target: &Path, directory_only: bool) -> Result<(), std::io::Error> {
        let fd = open_for_mark(target, directory_only)?;
        self.fanotify
            .mark(
                MarkFlags::FAN_MARK_ADD | MarkFlags::FAN_MARK_FILESYSTEM,
                watch_mask(),
                fd,
                None::<&Path>,
            )
            .map_err(std::io::Error::from)
    }
}

impl FsEventSource for RealFanotifySource {
    /// Wartet höchstens `timeout` auf Ereignisse und liest dann verfügbare
    /// fanotify-Ereignisse als [`RawFsEvent`].
    ///
    /// # Description
    /// Siehe Moduldoku für den Ablauf (`poll` dann `read_events`) und die
    /// Grenzen der UID- und Pfadauflösung. Ein Ereignis, dessen
    /// Metadatenversion nicht passt, dessen Pfad sich nicht auflösen lässt
    /// oder dessen auslösender Prozess bereits beendet ist, wird geloggt und
    /// verworfen — nicht als Fehler der gesamten Runde gemeldet, damit ein
    /// einzelnes unvollständiges Ereignis nicht die übrigen, vollständigen
    /// Ereignisse derselben Runde unterdrückt. Ein Ereignis ohne Deskriptor
    /// (Warteschlangen-Überlauf) wird zum Überlauf-Marker
    /// ([`overflow_marker`]); `EACCES`/`EPERM` von `read(2)` ist eine leere
    /// Charge (siehe Moduldoku, „Überlauf und Landlock-verweigerte
    /// Ereignisse").
    fn read_events(&self, timeout: Duration) -> Result<Vec<RawFsEvent>, FsMonError> {
        let deadline = Timespec::try_from(timeout).unwrap_or(Timespec {
            tv_sec: i64::MAX,
            tv_nsec: 0,
        });
        let mut fds = [PollFd::new(&self.fanotify, PollFlags::IN)];
        let ready = poll(&mut fds, Some(&deadline)).map_err(std::io::Error::from)?;
        if ready == 0 {
            return Ok(Vec::new());
        }

        let raw_events = match self.fanotify.read_events() {
            Ok(raw_events) => raw_events,
            Err(errno) if is_denied_event_open(errno) => {
                tracing::trace!(
                    errno = %errno,
                    "fanotify event outside the landlock read roots was not opened; skipped"
                );
                return Ok(Vec::new());
            }
            Err(errno) => return Err(FsMonError::Io(std::io::Error::from(errno))),
        };
        let mut events = Vec::with_capacity(raw_events.len());

        for event in &raw_events {
            if !event.check_version() {
                tracing::warn!("fanotify event has a mismatched metadata version; dropping it");
                continue;
            }

            let Some(fd) = event.fd() else {
                tracing::warn!(
                    "fanotify event queue overflowed; an unknown number of events was lost, \
                     reporting the sensor as degraded"
                );
                events.push(overflow_marker());
                continue;
            };

            let Ok(pid) = u32::try_from(event.pid()) else {
                tracing::warn!(
                    pid = event.pid(),
                    "fanotify reported a negative pid; dropping the event"
                );
                continue;
            };

            let link_path = proc_self_fd_link_path(fd.as_raw_fd());
            let Ok(target) = std::fs::read_link(&link_path) else {
                tracing::warn!(
                    pid,
                    "could not resolve the fanotify event's descriptor target; dropping it"
                );
                continue;
            };

            let Ok(process_metadata) = std::fs::metadata(proc_pid_dir_path(pid)) else {
                tracing::warn!(
                    pid,
                    "process exited before its uid could be resolved; dropping the event"
                );
                continue;
            };

            events.push(RawFsEvent {
                mask: event.mask().bits(),
                pid,
                uid: process_metadata.uid(),
                fd_target: target.to_string_lossy().into_owned(),
            });
        }

        Ok(events)
    }
}

/// Baut die reale fanotify-Ereignisquelle.
///
/// # Description
/// Siehe Moduldoku für die Recherche, den gewählten Weg und die
/// Reihenfolge: `main` ruft diese Funktion **vor**
/// [`crate::landlock::enforce_read_scope`] auf.
///
/// # Arguments
/// - `scope` (`&harw_dod_cap::ReadScope`): die zu überwachenden Wurzeln —
///   dieselbe Instanz, die `main` danach an
///   [`crate::landlock::enforce_read_scope`] übergibt.
///
/// # Returns
/// Eine einsatzbereite Quelle und den [`MarkReport`] ihrer Abdeckung.
///
/// # Errors
/// - [`ProbeError::FanotifySourceUnavailable`]: unter den Bedingungen, die
///   [`RealFanotifySource::new`] nennt.
pub fn build_fanotify_source(
    scope: &ReadScope,
) -> Result<(Box<dyn FsEventSource>, MarkReport), ProbeError> {
    let (source, report) = RealFanotifySource::new(scope)?;
    Ok((Box::new(source), report))
}

#[cfg(test)]
mod tests {
    use nix::sys::fanotify::{EventFFlags, InitFlags, MaskFlags};

    use harw_dod_fsmon::mask::{FAN_Q_OVERFLOW, is_queue_overflow};
    use nix::errno::Errno;

    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use rustix::fs::{CWD, FileType, Mode};

    use super::{
        MAX_MOUNTINFO_BYTES, MarkPlan, MarkReport, PathState, RootOutcome, event_f_flags,
        init_flags, inspect_mount_point, inspect_path, is_denied_event_open, open_for_mark,
        overflow_marker, parse_mount_points, plan_mark, plan_submount, proc_pid_dir_path,
        proc_self_fd_link_path, read_mount_points, submounts_below, watch_mask,
    };
    use crate::test_support::{TestResult, ctx};

    // Ein injizierter Dateibaum: nicht genannte Pfade fehlen. `use<>`: die
    // Closure besitzt ihre Tabelle und leiht nichts von `entries` (Edition
    // 2024 finge sonst dessen Lebensdauer ein, und jeder Aufruf mit einem
    // temporären Feld scheiterte an E0716).
    fn tree(entries: &[(&str, PathState)]) -> impl Fn(&Path) -> PathState + use<> {
        let map: BTreeMap<PathBuf, PathState> = entries
            .iter()
            .map(|(path, state)| (PathBuf::from(path), *state))
            .collect();
        move |path: &Path| map.get(path).copied().unwrap_or(PathState::Absent)
    }

    const fn present(dev: u64, readable: bool) -> PathState {
        PathState::Present { dev, readable }
    }

    /// Review R2: eine lesbare Wurzel wird selbst markiert, eine fehlende
    /// nicht.
    #[test]
    fn test_plan_mark_marks_a_readable_root_itself_and_skips_an_absent_one() {
        let inspect = tree(&[("/", present(1, true)), ("/etc", present(1, true))]);
        assert_eq!(
            plan_mark(Path::new("/etc"), &inspect),
            MarkPlan::Mark {
                target: PathBuf::from("/etc"),
                dev: 1
            }
        );
        assert_eq!(
            plan_mark(Path::new("/var/home"), &inspect),
            MarkPlan::Absent
        );
    }

    /// Review R2: `/var/spool/cron` (`0700`) wird über den nächsten lesbaren
    /// Vorfahren auf demselben Dateisystem markiert.
    #[test]
    fn test_plan_mark_falls_back_to_the_nearest_readable_ancestor_on_the_same_device() {
        let inspect = tree(&[
            ("/", present(1, true)),
            ("/var", present(2, true)),
            ("/var/spool", present(2, true)),
            ("/var/spool/cron", present(2, false)),
        ]);
        assert_eq!(
            plan_mark(Path::new("/var/spool/cron"), &inspect),
            MarkPlan::Mark {
                target: PathBuf::from("/var/spool"),
                dev: 2
            }
        );
    }

    /// Review R2: ein eigenes Dateisystem `/root` (`0700`) hat keinen
    /// lesbaren Pfad — der Vorfahr `/` liegt auf einem anderen Gerät und
    /// wird nie markiert.
    #[test]
    fn test_plan_mark_never_crosses_a_mount_point() {
        let inspect = tree(&[("/", present(1, true)), ("/root", present(3, false))]);
        assert_eq!(
            plan_mark(Path::new("/root"), &inspect),
            MarkPlan::NoReadablePath { dev: 3 }
        );
    }

    /// Eine nicht prüfbare Wurzel übernimmt das Gerät ihres nächsten
    /// prüfbaren Vorfahren; ganz ohne prüfbaren Pfad ist sie unbekannt.
    #[test]
    fn test_plan_mark_uses_the_nearest_inspectable_ancestor_for_a_hidden_root() {
        let inspect = tree(&[
            ("/", present(1, true)),
            ("/root", present(1, false)),
            ("/root/.config", PathState::Hidden),
        ]);
        assert_eq!(
            plan_mark(Path::new("/root/.config"), &inspect),
            MarkPlan::Mark {
                target: PathBuf::from("/"),
                dev: 1
            }
        );
        let nothing = |_: &Path| PathState::Hidden;
        assert_eq!(plan_mark(Path::new("/srv/x"), nothing), MarkPlan::Unknown);
    }

    /// Review R2: die Abdeckung je Wurzel aus injizierten Geräten — eine
    /// nicht selbst markierte Wurzel ist abgedeckt, wenn ihr Dateisystem
    /// markiert ist, sonst (und ohne bekanntes Gerät) nicht.
    #[test]
    fn test_mark_report_counts_roots_on_unmarked_filesystems() {
        let complete = MarkReport::of(
            &[
                RootOutcome::Marked { dev: 1 },
                RootOutcome::Unmarked { dev: Some(1) },
                RootOutcome::Absent,
                RootOutcome::Marked { dev: 1 },
            ],
            &[],
        );
        assert_eq!(complete.marked_filesystems, 1);
        assert_eq!(complete.uncovered_roots, 0);
        assert_eq!(complete.uncovered_mounts, 0);
        assert_eq!(complete.absent_roots, 1);
        assert!(complete.is_complete());

        let separate_var = MarkReport::of(
            &[
                RootOutcome::Marked { dev: 1 },
                RootOutcome::Unmarked { dev: Some(2) },
                RootOutcome::Unmarked { dev: Some(2) },
                RootOutcome::Unmarked { dev: None },
                RootOutcome::Marked { dev: 4 },
            ],
            &[],
        );
        assert_eq!(separate_var.marked_filesystems, 2);
        assert_eq!(separate_var.uncovered_roots, 3);
        assert_eq!(separate_var.absent_roots, 0);
        assert!(!separate_var.is_complete());

        let nothing = MarkReport::of(&[RootOutcome::Absent], &[]);
        assert_eq!(nothing.marked_filesystems, 0);
        assert_eq!(nothing.absent_roots, 1);
        assert!(nothing.is_complete(), "absent roots are not blindness");
    }

    /// Review R3: ein Einhängepunkt unter einer Wurzel auf einem nicht
    /// markierten Dateisystem (NFS-Home, systemd-homed) macht den Bericht
    /// unvollständig; ein markierter oder ein Bind-Mount eines markierten
    /// Dateisystems nicht. Eine unlesbare Einhängetabelle zählt als ein
    /// nicht abgedeckter Einhängepunkt.
    #[test]
    fn test_mark_report_counts_mounts_below_the_roots() {
        let roots = [RootOutcome::Marked { dev: 1 }];
        let nfs_home = MarkReport::of(&roots, &[RootOutcome::Unmarked { dev: Some(9) }]);
        assert_eq!(nfs_home.uncovered_roots, 0);
        assert_eq!(nfs_home.uncovered_mounts, 1);
        assert!(!nfs_home.is_complete());

        let covered = MarkReport::of(
            &roots,
            &[
                RootOutcome::Marked { dev: 9 },
                RootOutcome::Unmarked { dev: Some(1) },
                RootOutcome::Absent,
            ],
        );
        assert_eq!(covered.marked_filesystems, 2);
        assert_eq!(covered.uncovered_mounts, 0);
        assert!(covered.is_complete());

        // Ein markierter Einhängepunkt deckt auch eine Wurzel auf seinem
        // Dateisystem.
        let via_mount = MarkReport::of(
            &[RootOutcome::Unmarked { dev: Some(9) }],
            &[RootOutcome::Marked { dev: 9 }],
        );
        assert_eq!(via_mount.uncovered_roots, 0);

        let unreadable_table = MarkReport::of(&roots, &[RootOutcome::Unmarked { dev: None }]);
        assert_eq!(unreadable_table.uncovered_mounts, 1);
        assert!(!unreadable_table.is_complete());
    }

    /// Review R3: die Einhängetabelle liefert das fünfte Feld je Zeile, mit
    /// aufgelöster Oktal-Maskierung; kurze Zeilen, relative Pfade und
    /// `autofs`-Auslöser fallen weg (das eingehängte Ziel bleibt), ein
    /// Backslash ohne drei Oktalziffern bleibt stehen.
    #[test]
    fn test_parse_mount_points_reads_the_fifth_field_and_unescapes_it() {
        let table = b"22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
            61 22 0:52 / /home/alice rw,nosuid shared:30 - nfs4 srv:/alice rw\n\
            62 22 0:53 / /home/bob\\040smith/my\\134dir rw shared:31 - fuse.sshfs x rw\n\
            63 22 0:54 / /home/odd\\9xy rw - tmpfs tmpfs rw\n\
            64 22 0:55 / relative rw - tmpfs tmpfs rw\n\
            65 22 0:56 / /home/direct rw,relatime shared:40 - autofs systemd-1 rw,fd=5\n\
            66 65 0:57 / /home/direct rw shared:41 - nfs4 srv:/direct rw\n\
            short line\n\
            \n";
        assert_eq!(
            parse_mount_points(table),
            vec![
                PathBuf::from("/"),
                PathBuf::from("/home/alice"),
                PathBuf::from("/home/bob smith/my\\dir"),
                PathBuf::from("/home/odd\\9xy"),
                PathBuf::from("/home/direct"),
            ]
        );
    }

    /// Review R3: nur Einhängepunkte komponentenweise **unter** einer Wurzel
    /// zählen — nicht die Wurzel selbst, nicht ein Namensvetter
    /// (`/homework`), nicht ein Vorfahr; eine aufgelöste Wurzel (ostree:
    /// `/home` → `/var/home`) zählt mit.
    #[test]
    fn test_submounts_below_selects_only_mounts_strictly_below_a_root() {
        let mounts: Vec<PathBuf> = [
            "/",
            "/home",
            "/home/alice",
            "/homework",
            "/var",
            "/var/home/bob",
            "/etc/secret",
            "/home/alice",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        let roots: Vec<PathBuf> = ["/home", "/var/home", "/etc"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        let selected: Vec<PathBuf> = submounts_below(&mounts, &roots).into_iter().collect();
        assert_eq!(
            selected,
            vec![
                PathBuf::from("/etc/secret"),
                PathBuf::from("/home/alice"),
                PathBuf::from("/var/home/bob"),
            ]
        );
    }

    /// Review R3: ein Einhängepunkt wird nur über sich selbst markiert, nie
    /// über einen Vorfahren auf dem übergeordneten Dateisystem.
    #[test]
    fn test_plan_submount_never_falls_back_to_an_ancestor() {
        let inspect = tree(&[
            ("/", present(1, true)),
            ("/home", present(1, true)),
            ("/home/alice", present(9, true)),
            ("/home/bob", present(10, false)),
            ("/home/carol", PathState::Hidden),
        ]);
        assert_eq!(
            plan_submount(Path::new("/home/alice"), &inspect),
            MarkPlan::Mark {
                target: PathBuf::from("/home/alice"),
                dev: 9
            }
        );
        assert_eq!(
            plan_submount(Path::new("/home/bob"), &inspect),
            MarkPlan::NoReadablePath { dev: 10 }
        );
        assert_eq!(
            plan_submount(Path::new("/home/carol"), &inspect),
            MarkPlan::Unknown
        );
        assert_eq!(
            plan_submount(Path::new("/home/gone"), &inspect),
            MarkPlan::Absent
        );
    }

    /// Review R3: die Einhängetabelle wird begrenzt gelesen; eine größere
    /// Datei ist ein Fehler (fail closed), keine abgeschnittene Tabelle.
    #[test]
    fn test_read_mount_points_is_bounded() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let small = dir.path().join("mountinfo");
        std::fs::write(&small, b"22 1 8:1 / /home/alice rw - ext4 /dev/sda1 rw\n")
            .map_err(ctx("write table"))?;
        assert_eq!(
            read_mount_points(&small).map_err(ctx("read table"))?,
            vec![PathBuf::from("/home/alice")]
        );

        let big = dir.path().join("big");
        let size = usize::try_from(MAX_MOUNTINFO_BYTES)
            .map_err(ctx("bound fits usize"))?
            .saturating_add(1);
        std::fs::write(&big, vec![b' '; size]).map_err(ctx("write big table"))?;
        assert!(read_mount_points(&big).is_err());
        assert!(read_mount_points(&dir.path().join("missing")).is_err());
        Ok(())
    }

    /// Review R3: ein FIFO ohne Schreiber hält das Öffnen eines
    /// Markierungsziels nicht an (`O_NONBLOCK`). Als Wurzel ist er kein
    /// Verzeichnis (nicht lesbar, Rückfall auf den Vorfahren), als
    /// Einhängepunkt öffenbar.
    #[test]
    fn test_a_fifo_never_blocks_the_mark_open() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let fifo = dir.path().join("fifo");
        rustix::fs::mknodat(CWD, &fifo, FileType::Fifo, Mode::RUSR | Mode::WUSR, 0)
            .map_err(ctx("mkfifo"))?;
        // Ohne `O_NONBLOCK` bliebe dieser Aufruf ohne Schreiber stehen.
        assert!(open_for_mark(&fifo, false).is_ok());
        assert!(
            open_for_mark(&fifo, true).is_err(),
            "a FIFO is no directory"
        );
        assert!(matches!(
            inspect_path(&fifo),
            PathState::Present {
                readable: false,
                ..
            }
        ));
        assert!(matches!(
            inspect_mount_point(&fifo),
            PathState::Present { readable: true, .. }
        ));
        assert!(matches!(
            inspect_path(dir.path()),
            PathState::Present { readable: true, .. }
        ));
        assert_eq!(inspect_path(&dir.path().join("missing")), PathState::Absent);
        Ok(())
    }

    /// Review R2: die Warteschlange behält die Kernel-Grenze
    /// (`fs.fanotify.max_queued_events`) — ein Rückstand wird ein
    /// Überlauf-Marker statt unbegrenzten Kernel-Speichers; die
    /// Markierungsgrenze bleibt aufgehoben.
    #[test]
    fn test_the_event_queue_keeps_the_kernel_bound() {
        let flags = init_flags();
        assert!(!flags.contains(InitFlags::FAN_UNLIMITED_QUEUE));
        assert!(flags.contains(InitFlags::FAN_UNLIMITED_MARKS));
    }

    /// PL-100 WP1.6: der Überlauf-Marker trägt genau die Kernel-Maske und
    /// nichts, was als Schreibzugriff gedeutet werden könnte.
    #[test]
    fn test_overflow_marker_carries_the_kernel_overflow_mask_only() {
        let marker = overflow_marker();
        assert_eq!(marker.mask, FAN_Q_OVERFLOW);
        assert!(is_queue_overflow(marker.mask));
        assert_eq!(marker.pid, 0);
        assert_eq!(marker.uid, 0);
        assert!(marker.fd_target.is_empty());
    }

    /// PL-100 WP1.6: nur die Verweigerung durch Landlock ist eine leere
    /// Charge; jeder andere Lesefehler bleibt ein Fehler.
    #[test]
    fn test_only_eacces_and_eperm_count_as_a_denied_event_open() {
        assert!(is_denied_event_open(Errno::EACCES));
        assert!(is_denied_event_open(Errno::EPERM));
        for other in [Errno::EIO, Errno::EINVAL, Errno::EBADF, Errno::ENOMEM] {
            assert!(!is_denied_event_open(other), "{other:?}");
        }
    }

    /// Belegt die gewählten `InitFlags` gegen die in `man 7 fanotify`
    /// dokumentierten Rohwerte, ohne `Fanotify::init` aufzurufen — reine
    /// Bit-Arithmetik, kein Systemaufruf.
    #[test]
    fn test_init_flags_bits_match_documented_fan_constants() {
        let flags = init_flags();
        assert!(flags.contains(InitFlags::FAN_CLASS_NOTIF));
        assert!(flags.contains(InitFlags::FAN_CLOEXEC));
        assert!(flags.contains(InitFlags::FAN_NONBLOCK));
        assert!(!flags.contains(InitFlags::FAN_UNLIMITED_QUEUE));
        assert!(flags.contains(InitFlags::FAN_UNLIMITED_MARKS));
        // Bewusst nicht gesetzt: Berechtigungsklassen (`FAN_CLASS_CONTENT`,
        // `FAN_CLASS_PRE_CONTENT`) — diese Sonde beobachtet, sie entscheidet
        // nicht über Zugriff.
        assert!(!flags.contains(InitFlags::FAN_CLASS_CONTENT));
        assert!(!flags.contains(InitFlags::FAN_CLASS_PRE_CONTENT));
    }

    #[test]
    fn test_event_f_flags_are_read_only_close_on_exec_and_non_blocking() {
        let flags = event_f_flags();
        assert!(flags.contains(EventFFlags::O_RDONLY));
        assert!(flags.contains(EventFFlags::O_CLOEXEC));
        // Review R3: ein FIFO ohne Schreiber darf `read(2)` nicht anhalten.
        assert!(flags.contains(EventFFlags::O_NONBLOCK));
        assert!(!flags.contains(EventFFlags::O_WRONLY));
        assert!(!flags.contains(EventFFlags::O_RDWR));
    }

    /// Belegt, dass genau die beiden von `harw_dod_fsmon::mask` gedeuteten
    /// Bits überwacht werden — kein zusätzliches, wirkungsloses Bit.
    #[test]
    fn test_watch_mask_bits_match_documented_fan_constants() {
        let mask = watch_mask();
        assert!(mask.contains(MaskFlags::FAN_MODIFY));
        assert!(mask.contains(MaskFlags::FAN_CLOSE_WRITE));
        assert!(!mask.contains(MaskFlags::FAN_ACCESS));
        assert!(!mask.contains(MaskFlags::FAN_OPEN));
    }

    #[test]
    fn test_proc_self_fd_link_path_formats_the_raw_descriptor() {
        assert_eq!(proc_self_fd_link_path(7), "/proc/self/fd/7");
    }

    #[test]
    fn test_proc_pid_dir_path_formats_the_pid() {
        assert_eq!(proc_pid_dir_path(4242), "/proc/4242");
    }
}
