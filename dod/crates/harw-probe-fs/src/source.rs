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
//! [`RealFanotifySource`], [`build_fanotify_source`].
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
//! let _source = build_fanotify_source(&scope)?;
//! # Ok::<(), crate::error::ProbeError>(())
//! ```

use std::os::unix::fs::MetadataExt as _;
use std::os::unix::io::AsRawFd as _;
use std::path::Path;
use std::time::Duration;

use harw_dod_cap::ReadScope;
use harw_dod_fsmon::{FsEventSource, FsMonError, RawFsEvent};
use nix::sys::fanotify::{EventFFlags, Fanotify, InitFlags, MarkFlags, MaskFlags};
use rustix::event::{PollFd, PollFlags, Timespec, poll};

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
/// unbegrenzt zu blockieren. `FAN_UNLIMITED_QUEUE`/`FAN_UNLIMITED_MARKS`:
/// heben die Vorgaben von je 16384 Ereignissen bzw. 8192 Markierungen auf
/// (`man 7 fanotify`) — beide verlangen laut derselben Quelle
/// `CAP_SYS_ADMIN`, die einzige Fähigkeit, mit der dieses Binary ohnehin
/// läuft (siehe `crate`-Moduldoku, Abschnitt „Berechtigungsklasse").
///
/// # Returns
/// Die für diese Sonde vollständige `InitFlags`-Kombination.
#[must_use]
fn init_flags() -> InitFlags {
    InitFlags::FAN_CLASS_NOTIF
        | InitFlags::FAN_CLOEXEC
        | InitFlags::FAN_NONBLOCK
        | InitFlags::FAN_UNLIMITED_QUEUE
        | InitFlags::FAN_UNLIMITED_MARKS
}

/// Die `EventFFlags`, mit denen der Kernel die pro Ereignis gelieferten
/// Dateideskriptoren öffnet.
///
/// # Returns
/// `O_RDONLY | O_CLOEXEC` — lesend und ohne `exec`-Vererbung. Diese Sonde
/// liest von diesen Deskriptoren nie Dateiinhalt (siehe Moduldoku, Abschnitt
/// „Was diese Bindung nicht kann"); `O_RDONLY` ist die knappste Anforderung,
/// die der Kernel für die reine Pfadauflösung über `/proc/self/fd/<n>`
/// verlangt.
#[must_use]
fn event_f_flags() -> EventFFlags {
    EventFFlags::O_RDONLY | EventFFlags::O_CLOEXEC
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

/// Die reale fanotify-Ereignisquelle: eine initialisierte fanotify-Gruppe
/// mit Markierungen auf jeder Wurzel des übergebenen `ReadScope`.
///
/// # Description
/// Siehe Moduldoku für Recherche, gewählten Weg und Grenzen.
pub struct RealFanotifySource {
    fanotify: Fanotify,
}

impl RealFanotifySource {
    /// Initialisiert eine fanotify-Gruppe und markiert jede erreichbare
    /// Wurzel aus `scope`.
    ///
    /// # Description
    /// Öffnet für jede Wurzel einen gewöhnlichen, lesenden
    /// Verzeichnis-Deskriptor (`std::fs::File::open`) und übergibt ihn als
    /// `dirfd` an `Fanotify::mark` mit `path = None` — nach `man 2
    /// fanotify_mark` markiert das, zusammen mit `FAN_MARK_FILESYSTEM`, das
    /// Dateisystem, auf dem dieser Deskriptor liegt. Scheitert eine
    /// einzelne Wurzel (fehlt, keine Berechtigung, `mark` selbst schlägt
    /// fehl), wird sie geloggt und übersprungen, statt den gesamten Aufbau
    /// abzubrechen — dieselbe Fail-open-Richtung wie
    /// `crate::landlock::open_rule`, hier unschädlich, weil eine
    /// übersprungene Wurzel schlicht keine Ereignisse liefert, statt
    /// fälschlich mehr zu erlauben. Konnte **keine** Wurzel markiert werden,
    /// ist die Quelle nutzlos und dieser Konstruktor scheitert.
    ///
    /// # Arguments
    /// - `scope` (`&harw_dod_cap::ReadScope`): die zu überwachenden Wurzeln.
    ///
    /// # Returns
    /// Eine einsatzbereite `RealFanotifySource`.
    ///
    /// # Errors
    /// - [`ProbeError::FanotifySourceUnavailable`]: wenn `Fanotify::init`
    ///   scheitert (fehlende Berechtigung, Kernel ohne `CONFIG_FANOTIFY`)
    ///   oder keine der übergebenen Wurzeln markiert werden konnte.
    fn new(scope: &ReadScope) -> Result<Self, ProbeError> {
        let fanotify = Fanotify::init(init_flags(), event_f_flags())
            .map_err(|_| ProbeError::FanotifySourceUnavailable)?;

        let source = Self { fanotify };

        let mut marked_any = false;
        for root in scope.roots() {
            match source.mark_root(root) {
                Ok(()) => marked_any = true,
                Err(error) => tracing::warn!(
                    path = %root.display(),
                    error = %error,
                    "fanotify mark failed for this root; it will not be watched"
                ),
            }
        }

        if marked_any {
            Ok(source)
        } else {
            tracing::error!("no read-scope root could be marked; the fanotify source is unusable");
            Err(ProbeError::FanotifySourceUnavailable)
        }
    }

    /// Markiert das Dateisystem einer einzelnen Wurzel für [`watch_mask`].
    ///
    /// # Returns
    /// `Ok(())` bei Erfolg.
    ///
    /// # Errors
    /// [`std::io::Error`], wenn die Wurzel nicht als Verzeichnis geöffnet
    /// werden kann oder `Fanotify::mark` selbst fehlschlägt.
    fn mark_root(&self, root: &Path) -> Result<(), std::io::Error> {
        let dir = std::fs::File::open(root)?;
        self.fanotify
            .mark(
                MarkFlags::FAN_MARK_ADD | MarkFlags::FAN_MARK_FILESYSTEM,
                watch_mask(),
                dir,
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
    /// Metadatenversion nicht passt, dessen Deskriptor fehlt (Warteschlangen-
    /// Überlauf, `FanotifyEvent::fd` liefert dann `None`), dessen Pfad sich
    /// nicht auflösen lässt oder dessen auslösender Prozess bereits beendet
    /// ist, wird geloggt und verworfen — nicht als Fehler der gesamten
    /// Runde gemeldet, damit ein einzelnes unvollständiges Ereignis nicht
    /// die übrigen, vollständigen Ereignisse derselben Runde unterdrückt.
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

        let raw_events = self.fanotify.read_events().map_err(std::io::Error::from)?;
        let mut events = Vec::with_capacity(raw_events.len());

        for event in &raw_events {
            if !event.check_version() {
                tracing::warn!("fanotify event has a mismatched metadata version; dropping it");
                continue;
            }

            let Some(fd) = event.fd() else {
                tracing::warn!(
                    "fanotify event queue overflowed; an unknown number of events was lost"
                );
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
/// Siehe Moduldoku für die Recherche und den gewählten Weg.
///
/// # Arguments
/// - `scope` (`&harw_dod_cap::ReadScope`): die zu überwachenden Wurzeln —
///   dieselbe Instanz, mit der `main` zuvor
///   [`crate::landlock::enforce_read_scope`] aufgerufen hat.
///
/// # Returns
/// Eine einsatzbereite Quelle.
///
/// # Errors
/// - [`ProbeError::FanotifySourceUnavailable`]: unter den Bedingungen, die
///   [`RealFanotifySource::new`] nennt.
pub fn build_fanotify_source(scope: &ReadScope) -> Result<Box<dyn FsEventSource>, ProbeError> {
    Ok(Box::new(RealFanotifySource::new(scope)?))
}

#[cfg(test)]
mod tests {
    use nix::sys::fanotify::{EventFFlags, InitFlags, MaskFlags};

    use super::{event_f_flags, init_flags, proc_pid_dir_path, proc_self_fd_link_path, watch_mask};

    /// Belegt die gewählten `InitFlags` gegen die in `man 7 fanotify`
    /// dokumentierten Rohwerte, ohne `Fanotify::init` aufzurufen — reine
    /// Bit-Arithmetik, kein Systemaufruf.
    #[test]
    fn test_init_flags_bits_match_documented_fan_constants() {
        let flags = init_flags();
        assert!(flags.contains(InitFlags::FAN_CLASS_NOTIF));
        assert!(flags.contains(InitFlags::FAN_CLOEXEC));
        assert!(flags.contains(InitFlags::FAN_NONBLOCK));
        assert!(flags.contains(InitFlags::FAN_UNLIMITED_QUEUE));
        assert!(flags.contains(InitFlags::FAN_UNLIMITED_MARKS));
        // Bewusst nicht gesetzt: Berechtigungsklassen (`FAN_CLASS_CONTENT`,
        // `FAN_CLASS_PRE_CONTENT`) — diese Sonde beobachtet, sie entscheidet
        // nicht über Zugriff.
        assert!(!flags.contains(InitFlags::FAN_CLASS_CONTENT));
        assert!(!flags.contains(InitFlags::FAN_CLASS_PRE_CONTENT));
    }

    #[test]
    fn test_event_f_flags_are_read_only_and_close_on_exec() {
        let flags = event_f_flags();
        assert!(flags.contains(EventFFlags::O_RDONLY));
        assert!(flags.contains(EventFFlags::O_CLOEXEC));
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
