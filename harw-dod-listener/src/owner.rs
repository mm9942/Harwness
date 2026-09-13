//! Kostenbewusste Auflösung: welcher Prozess besitzt einen Listener-Socket, und zu welcher cgroup gehört er.
//!
//! # Verantwortungsbereich
//! Löst eine Menge gesuchter Socket-Inodes (aus [`crate::procnet`]) auf
//! besitzende Prozess-IDs auf ([`resolve_owners`]) und liest — nur für
//! tatsächlich gefundene Prozesse — deren reale UID und cgroup
//! ([`read_actor`]). Kennt keine Verbindungsziele und keine Portnummern; das
//! bleibt Sache von [`crate::procnet`] und [`crate::sensor`].
//!
//! # Kostenmodell (der teure Teil dieses Sensors)
//! Die Umkehrung "Inode → Prozess" existiert im Kernel nicht direkt; der
//! einzige Weg dorthin ist, `/proc/<pid>/fd/*` je Prozess aufzulisten und die
//! Symlink-Ziele (`socket:[<inode>]`) mit den gesuchten Inodes abzugleichen.
//! Im ungünstigen Fall bedeutet das: alle PIDs mal alle Deskriptoren. Zwei
//! Entscheidungen halten das im Rahmen:
//!
//! 1. **Eine Sammlung, ein Durchlauf.** [`resolve_owners`] bekommt die
//!    *vollständige* Menge gesuchter Inodes auf einmal (siehe
//!    [`crate::sensor::ListenerSensor::poll`]) und durchläuft `/proc`
//!    genau einmal — nicht einmal je Socket. Ein Sensor mit N offenen
//!    Listenern kostet also einen Prozess-Durchlauf, nicht N.
//! 2. **Früher Abbruch.** Sobald jede gesuchte Inode einem Prozess
//!    zugeordnet ist, bricht [`resolve_owners`] sowohl die
//!    Deskriptor-Schleife des aktuellen Prozesses als auch die
//!    PID-Schleife selbst ab — ein Host mit tausend Prozessen, aber nur
//!    drei offenen Listenern, deren Besitzer früh in der PID-Liste
//!    auftauchen, durchsucht in der Praxis nur einen Bruchteil von `/proc`.
//!
//! Trotzdem bleibt das der teuerste Teil dieses Sensors. Ein Sentinel, der
//! ihn bei jedem Abruf erneut in voller Tiefe laufen lässt, ohne die
//! Poll-Frequenz dieses Sensors bewusst niedrig zu halten, riskiert genau
//! das Schicksal, vor dem die Aufgabenstellung warnt: ein Sensor, der eine
//! Sekunde braucht, wird abgeschaltet — und meldet danach gar nichts mehr.
//!
//! UID und cgroup werden **nicht** im selben Durchlauf mitgesammelt, sondern
//! erst danach, gezielt für die (typischerweise wenigen) tatsächlich
//! gefundenen Besitzer-PIDs gelesen ([`read_actor`]) — auch das hält die
//! Zusatzkosten proportional zur Zahl der Listener, nicht zur Zahl der
//! Prozesse.
//!
//! # Bewusste Ausnahme von „kein direkter `std::fs`-Zugriff"
//! `harw_dod_readfs` bietet weder ein Auflisten von Verzeichnisinhalten noch
//! ein Lesen eines Symlink-*Ziels* an (nur [`harw_dod_cap::ReadScope::open`],
//! das eine Datei nach Symlink-Auflösung öffnet — für ein `/proc/<pid>/fd`-
//! Element wie `socket:[12345]` schlägt das fehl, weil dieses Ziel keine
//! reale Datei ist, die sich öffnen ließe). [`resolve_owners`] ruft deshalb,
//! wie schon `harw_dod_readfs::glob` es für die Verzeichnisauflistung tut
//! (siehe dessen Moduldokumentation), ausdrücklich `std::fs::read_dir` und
//! zusätzlich `std::fs::read_link` auf — Letzteres hat in `harw-dod-readfs`
//! noch **keine** Entsprechung (siehe Crate-Dokumentation, Abschnitt
//! "Lücke"). Jeder so konstruierte Pfad wird vor dem Zugriff über
//! [`harw_dod_cap::ReadScope::allows`] geprüft (eine reine, kanonisierungsfreie
//! Prüfung — `std::fs::canonicalize` liefe für einen Socket-Symlink ohnehin
//! ins Leere, siehe oben); er kann strukturell nie außerhalb liegen, da er
//! stets als `root.join(..)` unterhalb eines bereits geprüften `root`
//! entsteht, aber die Prüfung bleibt als Verteidigung in der Tiefe erhalten.
//!
//! # Prozesse, die nicht gelesen werden können
//! Ein `/proc/<pid>/fd`, das nicht gelistet werden kann (fremder Benutzer,
//! oder der Prozess ist zwischen dem Lesen von `/proc/net/*` und diesem
//! Durchlauf beendet worden), wird **übersprungen** — das ist die
//! Voreinstellung eines unprivilegierten Sensors, kein Fehler dieses
//! Abrufs. Ebenso ist eine nicht auflösbare UID oder cgroup kein Fehler:
//! [`read_actor`] liefert dafür `None`.
//!
//! # Nebenläufigkeit
//! Zustandslose freie Funktionen; `Send + Sync`. Beide Funktionen führen
//! Betriebssystem-I/O aus (`read_dir`, `read_link`, Dateilesungen über
//! `harw_dod_readfs`), halten aber keinen Zustand über den Aufruf hinaus.
//!
//! # Fehler
//! Keine eigenen — beide Funktionen sind total: ein nicht auflösbarer
//! Prozess, eine nicht lesbare cgroup-Datei oder eine fehlende UID-Zeile
//! führen zu `None`/einem leeren Ergebnis, nie zu einem `Result::Err`. Das
//! ist eine bewusste Entscheidung: nur das Lesen der vier Listener-Tabellen
//! selbst ([`crate::procnet::collect_listeners`]) kann diesen Sensor-Abruf
//! insgesamt scheitern lassen.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use harw_dod_cap::ReadScope;
use harw_dod_signals::Actor;
use harw_types::CgroupId;

/// Findet für jede gesuchte Inode-Nummer die besitzende Prozess-ID, in genau
/// einem Durchlauf über `/proc/<pid>/fd/*`.
///
/// # Description
/// Siehe Moduldokumentation für das vollständige Kostenmodell. Listet
/// `root` (die numerischen Einträge darin sind PIDs), dann je PID
/// `root/<pid>/fd`; jeder Eintrag darin ist ein Symlink, dessen Ziel bei
/// einem Socket-Deskriptor die Form `socket:[<inode>]` hat
/// ([`parse_socket_inode`]). Bricht früh ab, sobald jede gesuchte Inode
/// einer PID zugeordnet ist.
///
/// # Arguments
/// - `scope` (`&ReadScope`): der Lesebereich; `root` und jeder besuchte
///   Unterpfad werden dagegen geprüft.
/// - `root` (`&Path`): die `/proc`-Wurzel (oder ihr Fixture-Äquivalent).
/// - `inodes` (`&HashSet<u64>`): die gesuchten Socket-Inodes, typischerweise
///   die Inodes aller in einem Abruf gefundenen [`crate::procnet::ListenerRecord`]s.
///
/// # Returns
/// Eine Abbildung von Inode auf besitzende PID, für jede gefundene Inode.
/// Eine gesuchte Inode, deren Prozess nicht auflösbar war (fremder Benutzer,
/// bereits beendet, oder auf diesem Host gar nicht mehr vorhanden), fehlt im
/// Ergebnis — das ist kein Fehler dieser Funktion.
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar (jeder Aufruf öffnet und
/// schließt seine eigenen Verzeichnis-Handles).
///
/// # Examples
/// ```rust,no_run
/// use std::collections::HashSet;
/// use std::path::Path;
/// use harw_dod_cap::ReadScope;
/// use harw_dod_listener::owner::resolve_owners;
///
/// let scope = ReadScope::from_roots([Path::new("/proc").to_path_buf()]);
/// let inodes: HashSet<u64> = [12345].into_iter().collect();
/// let owners = resolve_owners(&scope, Path::new("/proc"), &inodes);
/// assert!(owners.get(&12345).is_none() || owners.len() <= 1);
/// ```
pub fn resolve_owners(scope: &ReadScope, root: &Path, inodes: &HashSet<u64>) -> HashMap<u64, u32> {
    let mut remaining: HashSet<u64> = inodes.clone();
    let mut owners: HashMap<u64, u32> = HashMap::new();

    if remaining.is_empty() || !scope.allows(root) {
        return owners;
    }

    let Ok(pid_entries) = std::fs::read_dir(root) else {
        return owners;
    };

    for pid_entry in pid_entries.flatten() {
        if remaining.is_empty() {
            break;
        }

        let Some(pid) = pid_entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            // Nicht-numerischer Eintrag unter `/proc` (z. B. "self", "net",
            // "sys") — keine PID, nichts zu tun.
            continue;
        };

        let fd_dir = root.join(pid.to_string()).join("fd");
        if !scope.allows(&fd_dir) {
            continue;
        }

        // Unlesbar (fremder Benutzer) oder der Prozess ist zwischen dem
        // Lesen der Listener-Tabellen und diesem Durchlauf beendet worden —
        // beides führt zum selben, harmlosen "diesen Prozess überspringen".
        let Ok(fd_entries) = std::fs::read_dir(&fd_dir) else {
            continue;
        };

        for fd_entry in fd_entries.flatten() {
            if remaining.is_empty() {
                break;
            }

            let fd_path = fd_entry.path();
            if !scope.allows(&fd_path) {
                continue;
            }

            // `std::fs::read_link` liest das rohe Symlink-Ziel, ohne es
            // aufzulösen — siehe Moduldokumentation, warum
            // `ReadScope::open`/`canonicalize` hier nicht funktionieren
            // würden.
            let Ok(target) = std::fs::read_link(&fd_path) else {
                continue;
            };

            let Some(inode) = parse_socket_inode(&target) else {
                continue;
            };

            if remaining.remove(&inode) {
                owners.insert(inode, pid);
            }
        }
    }

    owners
}

/// Extrahiert die Inode-Nummer aus einem `socket:[<inode>]`-Symlink-Ziel.
///
/// Andere Ziele (reguläre Dateien, Pipes `pipe:[<inode>]`, anonyme Inodes,
/// Verzeichnisse) liefern `None` — sie sind keine Sockets und für diesen
/// Sensor uninteressant.
fn parse_socket_inode(target: &Path) -> Option<u64> {
    let text = target.to_str()?;
    text.strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse::<u64>()
        .ok()
}

/// Liest UID und cgroup des Prozesses `pid`, bestmöglich.
///
/// # Description
/// Liefert `None`, wenn nicht einmal die UID auflösbar ist — ein `Actor`
/// ohne UID lässt sich nicht ehrlich bilden (`Actor::uid` ist keine
/// `Option`). Ist die UID lesbar, aber die cgroup nicht, entsteht trotzdem
/// ein `Actor`, mit `cgroup: None` (siehe [`read_cgroup`] und die
/// Crate-Dokumentation für die Unterscheidung dieser beiden Fälle).
///
/// # Arguments
/// - `scope` (`&ReadScope`): der Lesebereich.
/// - `root` (`&Path`): die `/proc`-Wurzel (oder ihr Fixture-Äquivalent).
/// - `pid` (`u32`): die aufzulösende Prozess-ID.
///
/// # Returns
/// `Some(Actor)` mit der realen UID aus `/proc/<pid>/status` und, falls
/// auflösbar, der `CgroupId` aus `/proc/<pid>/cgroup`; `auid` ist immer
/// `None` — dieser Sensor hat keinen Zugriff auf das Audit-Subsystem
/// (`Capability::ReadAuditNetlink` gehört einer anderen Sensor-Crate). `None`,
/// wenn nicht einmal die UID lesbar war.
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_dod_cap::ReadScope;
/// use harw_dod_listener::owner::read_actor;
///
/// let scope = ReadScope::from_roots([Path::new("/proc").to_path_buf()]);
/// let actor = read_actor(&scope, Path::new("/proc"), 1);
/// // `init`/PID 1 ist meist lesbar; auf Hosts ohne Berechtigung liefert dies `None`.
/// let _ = actor;
/// ```
pub fn read_actor(scope: &ReadScope, root: &Path, pid: u32) -> Option<Actor> {
    let uid = read_uid(scope, root, pid)?;
    let cgroup = read_cgroup(scope, root, pid);
    Some(Actor {
        uid,
        auid: None,
        cgroup,
    })
}

/// Liest die reale UID aus `/proc/<pid>/status` (die Zeile `Uid:` trägt
/// real, effektiv, gespeichert und Dateisystem-UID in dieser Reihenfolge;
/// die erste ist die reale UID).
fn read_uid(scope: &ReadScope, root: &Path, pid: u32) -> Option<u32> {
    let path: PathBuf = root.join(pid.to_string()).join("status");
    let lines = harw_dod_readfs::read_lines(scope, &path).ok()?;
    let uid_line = lines.iter().find(|line| line.starts_with("Uid:"))?;
    uid_line.split_whitespace().nth(1)?.parse::<u32>().ok()
}

/// Liest die cgroup aus `/proc/<pid>/cgroup`, siehe [`parse_cgroup_id`] für
/// die Auswahlregel bei mehreren Zeilen.
fn read_cgroup(scope: &ReadScope, root: &Path, pid: u32) -> Option<CgroupId> {
    let path: PathBuf = root.join(pid.to_string()).join("cgroup");
    let content = harw_dod_readfs::read_to_string(scope, &path).ok()?;
    parse_cgroup_id(&content)
}

/// Parst den Inhalt von `/proc/<pid>/cgroup` zu einer [`CgroupId`].
///
/// # Description
/// Jede Zeile hat die Form `<hierarchy-id>:<controller-liste>:<pfad>`. Auf
/// einem cgroup-v2-System (dem Regelfall) gibt es genau eine Zeile mit
/// Hierarchie-ID `0` und leerer Controller-Liste (`0::/pfad`); diese hat
/// Vorrang, falls vorhanden. Sonst — reines cgroup-v1, mehrere
/// Hierarchie-Zeilen — wird der Pfad der ersten wohlgeformten, nicht-leeren
/// Zeile verwendet: ein Prozess gehört in jeder v1-Hierarchie potenziell zu
/// einer anderen cgroup, und ohne eine einzige, konsistente Auswahlregel
/// würde diese Funktion für denselben Prozess bei jedem Aufruf ein anderes
/// Ergebnis liefern können, je nachdem, welche Hierarchie gerade zuerst
/// geprüft wird.
///
/// Verwendet [`CgroupId::from_str`] — den unvalidierten Konstruktor, der den
/// übergebenen Pfad unverändert übernimmt, **nicht** [`CgroupId::new`], das
/// eine zufällige UUID erzeugen würde und mit dem gelesenen Pfad nichts zu
/// tun hätte.
///
/// # Arguments
/// - `content` (`&str`): der vollständige Inhalt von `/proc/<pid>/cgroup`.
///
/// # Returns
/// `Some(CgroupId)`, wenn mindestens eine Zeile einen nicht-leeren Pfad
/// trägt; `None` bei leerem Inhalt oder wenn jede Zeile weniger als drei
/// durch `:` getrennte Felder hat oder einen leeren Pfad trägt.
///
/// # Examples
/// ```rust
/// use harw_dod_listener::owner::parse_cgroup_id;
///
/// let v2 = "0::/user.slice/user-1000.slice/session-3.scope\n";
/// assert_eq!(
///     parse_cgroup_id(v2).map(|id| id.as_str().to_owned()),
///     Some("/user.slice/user-1000.slice/session-3.scope".to_owned())
/// );
/// assert_eq!(parse_cgroup_id(""), None);
/// ```
pub fn parse_cgroup_id(content: &str) -> Option<CgroupId> {
    let mut first_valid: Option<String> = None;

    for line in content.lines() {
        let mut fields = line.splitn(3, ':');
        let (Some(hierarchy), Some(_controllers), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };

        if path.trim().is_empty() {
            continue;
        }

        if hierarchy == "0" {
            return Some(CgroupId::from_str(path.to_owned()));
        }

        if first_valid.is_none() {
            first_valid = Some(path.to_owned());
        }
    }

    first_valid.map(CgroupId::from_str)
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_parse_cgroup_id_prefers_unified_v2_line() {
        let content = "12:pids:/user.slice/user-1000.slice\n\
                        0::/user.slice/user-1000.slice/session-3.scope\n";
        let id = parse_cgroup_id(content).expect("v2-Zeile muss aufgelöst werden");
        assert_eq!(id.as_str(), "/user.slice/user-1000.slice/session-3.scope");
    }

    #[test]
    fn test_parse_cgroup_id_falls_back_to_first_v1_line() {
        let content = "4:memory:/user.slice/user-1000.slice\n\
                        7:cpu:/user.slice/user-1000.slice\n";
        let id = parse_cgroup_id(content).expect("erste v1-Zeile muss aufgelöst werden");
        assert_eq!(id.as_str(), "/user.slice/user-1000.slice");
    }

    #[test]
    fn test_parse_cgroup_id_empty_content_is_none() {
        assert_eq!(parse_cgroup_id(""), None);
    }

    #[test]
    fn test_parse_cgroup_id_skips_lines_with_empty_path() {
        let content = "0::\n4:memory:/real.slice\n";
        let id = parse_cgroup_id(content).expect("Zeile mit leerem Pfad wird übersprungen");
        assert_eq!(id.as_str(), "/real.slice");
    }

    /// Baut `root/<pid>/fd/<fd>` als Symlink auf `socket:[<inode>]` — das
    /// procfs-Muster für einen Socket-Deskriptor, ohne dass "socket:[..]"
    /// selbst ein reales Ziel sein muss (siehe Moduldokumentation).
    #[cfg(unix)]
    fn write_socket_fd(root: &Path, pid: u32, fd: u32, inode: u64) {
        let fd_dir = root.join(pid.to_string()).join("fd");
        fs::create_dir_all(&fd_dir).expect("fd-Verzeichnis anlegen");
        symlink(format!("socket:[{inode}]"), fd_dir.join(fd.to_string()))
            .expect("Socket-Symlink anlegen");
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_owners_finds_pid_owning_inode() {
        let dir = tempdir().expect("tempdir");
        write_socket_fd(dir.path(), 1000, 3, 12345);
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);

        let inodes: HashSet<u64> = [12345].into_iter().collect();
        let owners = resolve_owners(&scope, dir.path(), &inodes);

        assert_eq!(owners.get(&12345), Some(&1000));
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_owners_skips_process_with_missing_fd_directory() {
        let dir = tempdir().expect("tempdir");
        // `2000` existiert als PID-Verzeichnis, aber ohne lesbares `fd` —
        // dieselbe Fehlerbahn wie eine fehlende Leseberechtigung.
        fs::create_dir_all(dir.path().join("2000")).expect("pid-Verzeichnis anlegen");
        write_socket_fd(dir.path(), 1000, 3, 12345);
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);

        let inodes: HashSet<u64> = [12345, 99999].into_iter().collect();
        let owners = resolve_owners(&scope, dir.path(), &inodes);

        assert_eq!(owners.get(&12345), Some(&1000));
        assert_eq!(owners.get(&99999), None);
    }

    #[test]
    fn test_resolve_owners_empty_inode_set_returns_empty_map() {
        let dir = tempdir().expect("tempdir");
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);
        let owners = resolve_owners(&scope, dir.path(), &HashSet::new());
        assert!(owners.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn test_read_actor_resolves_uid_and_cgroup() {
        let dir = tempdir().expect("tempdir");
        let pid_dir = dir.path().join("1000");
        fs::create_dir_all(&pid_dir).expect("pid-Verzeichnis anlegen");
        fs::write(
            pid_dir.join("status"),
            "Name:\tsshd\nUid:\t1000\t1000\t1000\t1000\n",
        )
        .expect("status schreiben");
        fs::write(pid_dir.join("cgroup"), "0::/user.slice/user-1000.slice\n")
            .expect("cgroup schreiben");
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);

        let actor = read_actor(&scope, dir.path(), 1000).expect("Actor muss auflösbar sein");
        assert_eq!(actor.uid, 1000);
        assert_eq!(actor.auid, None);
        assert_eq!(
            actor.cgroup.map(|c| c.as_str().to_owned()),
            Some("/user.slice/user-1000.slice".to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_read_actor_missing_cgroup_file_still_resolves_uid_with_none_cgroup() {
        let dir = tempdir().expect("tempdir");
        let pid_dir = dir.path().join("1000");
        fs::create_dir_all(&pid_dir).expect("pid-Verzeichnis anlegen");
        fs::write(pid_dir.join("status"), "Uid:\t1000\t1000\t1000\t1000\n")
            .expect("status schreiben");
        // Keine `cgroup`-Datei angelegt.
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);

        let actor = read_actor(&scope, dir.path(), 1000).expect("UID allein reicht für Actor");
        assert_eq!(actor.uid, 1000);
        assert_eq!(actor.cgroup, None);
    }

    #[test]
    fn test_read_actor_missing_status_file_returns_none() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("1000")).expect("pid-Verzeichnis anlegen");
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);

        assert_eq!(read_actor(&scope, dir.path(), 1000), None);
    }

    /// Scope-Dichtheit: `resolve_owners` durchsucht `root` gar nicht erst,
    /// wenn `root` außerhalb des übergebenen Bereichs liegt — auch dann
    /// nicht, wenn unter `root` tatsächlich auflösbare Daten liegen.
    #[cfg(unix)]
    #[test]
    fn test_resolve_owners_returns_empty_when_root_outside_scope() {
        let real_root = tempdir().expect("tempdir für die echte Wurzel");
        let other_root = tempdir().expect("tempdir außerhalb des Bereichs");
        write_socket_fd(real_root.path(), 1000, 3, 12345);

        let scope = ReadScope::from_roots([other_root.path().to_path_buf()]);
        let inodes: HashSet<u64> = [12345].into_iter().collect();
        let owners = resolve_owners(&scope, real_root.path(), &inodes);

        assert!(owners.is_empty());
    }
}
