//! `ListenerSensor`: bindet Tabellen-Parsing ([`crate::procnet`]) und
//! Besitzer-Auflösung ([`crate::owner`]) an den `Sensor`-Vertrag.
//!
//! # Verantwortungsbereich
//! Einziger Ort dieser Crate, der [`harw_dod_signals::Sensor`] implementiert.
//! Orchestriert genau zwei Schritte je Abruf: Listener-Tabellen lesen
//! ([`crate::procnet::collect_listeners`]), dann — nur für die dabei
//! gefundenen Inodes — Besitzer auflösen
//! ([`crate::owner::resolve_owners`], [`crate::owner::read_actor`]). Enthält
//! selbst keine Parselogik für `/proc/net/*` oder `/proc/<pid>/*` — das ist
//! Sache der beiden genannten Module.
//!
//! # Kardinalität
//! Ein [`harw_dod_signals::SecurityEvent`] je aktuell offenem Listener-Socket
//! — der Port lebt im Ereignisrumpf
//! ([`harw_dod_signals::EventKind::ListenerOpened`]), nie als Label. Das ist
//! bewusst: die Menge der PIDs/Ports, auf denen Prozesse lauschen, ist über
//! die Zeit unbegrenzt (jeder neu gestartete Dienst kann einen neuen Port
//! wählen), und ein Label mit dieser Kardinalität in einer Zeitreihen-Metrik
//! würde die Zeitreihendatenbank sprengen. Dieser Sensor registriert deshalb
//! **keine** Metrik über `harw-observe`/`metrics!` (die Crate hängt bewusst
//! nicht von `harw-observe` ab) — ein Ereignisstrom mit Portangabe im Rumpf
//! trägt dieselbe Information ohne Kardinalitätsproblem, weil ein Ereignis
//! kein Zeitreihen-Label ist. [`SensorReading::samples`] bleibt bei diesem
//! Sensor deshalb immer leer.
//!
//! # Nebenläufigkeit
//! [`ListenerSensor`] ist `Send + Sync` (alle Felder sind es); [`Sensor::poll`]
//! nimmt `&self` und hält keinen Zustand über den Aufruf hinaus — konkurrierende
//! Aufrufe auf demselben Sensor sind sicher, auch wenn sie sich gegenseitig
//! verlangsamen (beide durchlaufen `/proc` unabhängig voneinander).
//!
//! # Fehler
//! [`harw_dod_cap::SensorError`], ausschließlich aus
//! [`crate::procnet::collect_listeners`] (siehe dortige Dokumentation). Ein
//! einzelner nicht auflösbarer Prozessbesitzer oder eine nicht lesbare
//! cgroup lässt den gesamten Abruf nie scheitern (siehe
//! [`crate::owner`]-Moduldokumentation).
//!
//! # Warum `harw_dod_fixtures::sensor_suite!` hier nicht greift
//! `sensor_suite!` verlangt `From<harw_dod_cap::SensorHandle<harw_dod_cap::Bound>>`
//! als Konstruktionsweg: das Harness baut selbst einen gebundenen Griff auf
//! `fixtures/<fall>/tree` und wandelt ihn per `From` in eine Sensor-Instanz
//! um. [`ListenerSensor`] nimmt aber nie einen bereits gebundenen Griff
//! entgegen — [`ListenerSensor::new`] erhält stattdessen `id` und `root`
//! roh und baut `ReadScope`/`SensorHandle` selbst daraus auf. Anders als bei
//! `harw-dod-workspace`s Drift-Sensor ist das keine Konfiguration, die aus
//! einem Griff strukturell nicht ableitbar wäre (`root` ließe sich aus
//! `handle.scope().roots()` zurückgewinnen, genau wie `harw-dod-gpu` seinen
//! Lesepfad zur Laufzeit aus `scope.roots()` ableitet) — dieser Sensor hat
//! schlicht nie eine `From<SensorHandle<Bound>>`-Konstruktion bekommen, und
//! die Testsuite unten (siehe `#[cfg(test)] mod tests`) prüft die sechs
//! Harness-Eigenschaften deshalb von Hand statt über das Makro.
//!
//! # Examples
//! ```rust,no_run
//! use std::path::PathBuf;
//! use harw_dod_listener::ListenerSensor;
//! use harw_dod_signals::Sensor;
//! use harw_types::SensorId;
//!
//! let sensor = ListenerSensor::new(SensorId::from_str("listener-0"), PathBuf::from("/proc"));
//! let reading = sensor.poll(jiff::Timestamp::UNIX_EPOCH)?;
//! assert!(reading.samples.is_empty());
//! # Ok::<(), harw_dod_cap::SensorError>(())
//! ```

use std::collections::HashSet;
use std::path::PathBuf;

use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
use harw_dod_signals::{EventKind, SecurityEvent, Sensor, SensorReading};
use harw_types::SensorId;
use jiff::Timestamp;

use crate::owner;
use crate::procnet;

/// Sensor für offene Listener-Sockets mit cgroup-Bezug (Knoten AW2-14).
///
/// # Description
/// Meldet, *dass* auf einem Port gelauscht wird und, falls auflösbar, zu
/// welcher cgroup der lauschende Prozess gehört — siehe Crate-Dokumentation
/// für die Berechtigungsgrenze (keine Verbindungsinhalte, keine Ziele).
#[derive(Debug)]
pub struct ListenerSensor {
    handle: SensorHandle<Bound>,
    /// Die `/proc`-Wurzel, aus der Netz-Tabellen, Prozess-Deskriptoren und
    /// Prozess-Metadaten gelesen werden. In Produktion `/proc`; Tests setzen
    /// hier die Wurzel eines Fixture-Baums mit derselben Unterstruktur
    /// (`net/tcp`, `<pid>/fd`, `<pid>/status`, `<pid>/cgroup`).
    root: PathBuf,
}

impl ListenerSensor {
    /// Baut den Sensor, dessen Lesebereich exakt auf `root` beschränkt ist.
    ///
    /// # Description
    /// Bindet einen [`SensorHandle`] mit [`Capability::ReadProcNet`] an einen
    /// [`ReadScope`], dessen einzige Wurzel `root` ist. In Produktion ist
    /// `root` `/proc`: dieser Sensor braucht — anders als es der Name der
    /// Fähigkeit nahelegt — mehr als nur `/proc/net`, nämlich auch
    /// `/proc/<pid>/fd`, `/proc/<pid>/status` und `/proc/<pid>/cgroup`, um die
    /// in Knoten AW2-14 verlangte Besitzer- und cgroup-Auflösung
    /// durchzuführen (siehe [`crate::owner`]). [`Capability::probe`] bleibt
    /// rein informativ und wird von keiner Stelle dieser Crate zur
    /// Zugriffsentscheidung herangezogen — das leistet ausschließlich der
    /// hier gebundene [`ReadScope`].
    ///
    /// # Arguments
    /// - `id` (`SensorId`): Kennung dieses Sensors.
    /// - `root` (`PathBuf`): die `/proc`-Wurzel (oder ihr Fixture-Äquivalent).
    ///
    /// # Returns
    /// Einen einsatzbereiten `ListenerSensor`.
    ///
    /// # Examples
    /// ```rust
    /// use std::path::PathBuf;
    /// use harw_dod_listener::ListenerSensor;
    /// use harw_types::SensorId;
    ///
    /// let _sensor = ListenerSensor::new(SensorId::from_str("listener-0"), PathBuf::from("/proc"));
    /// ```
    #[must_use]
    pub fn new(id: SensorId, root: PathBuf) -> Self {
        let scope = ReadScope::from_roots([root.clone()]);
        let handle = SensorHandle::new(id, Capability::ReadProcNet).bind(scope);
        Self { handle, root }
    }
}

impl Sensor for ListenerSensor {
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    /// Liest die vier Listener-Tabellen einmal und löst für die dabei
    /// gefundenen Sockets ihre Besitzer auf — siehe Moduldokumentation für
    /// das Kostenmodell und die Kardinalitätsbegründung.
    ///
    /// # Errors
    /// [`SensorError`], wenn [`crate::procnet::collect_listeners`] scheitert.
    /// Ein einzelner nicht auflösbarer Besitzer lässt diesen Abruf nie
    /// scheitern.
    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let scope = self.handle.scope();
        let listeners = procnet::collect_listeners(scope, &self.root)?;

        let inodes: HashSet<u64> = listeners.iter().map(|record| record.inode).collect();
        let owners = owner::resolve_owners(scope, &self.root, &inodes);

        let events: Vec<SecurityEvent> = listeners
            .into_iter()
            .map(|record| {
                let actor = owners
                    .get(&record.inode)
                    .and_then(|&pid| owner::read_actor(scope, &self.root, pid));
                SecurityEvent {
                    sensor: self.handle.id().clone(),
                    observed_at: now,
                    actor,
                    kind: EventKind::ListenerOpened { port: record.port },
                }
            })
            .collect();

        Ok(SensorReading {
            samples: Vec::new(),
            events,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::path::Path;

    use tempfile::tempdir;

    use super::*;

    const HEADER: &str =
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";

    /// Schreibt `root/net/tcp` mit Kopfzeile plus den übergebenen Datenzeilen.
    fn write_tcp_table(root: &Path, lines: &[String]) {
        let net_dir = root.join("net");
        fs::create_dir_all(&net_dir).expect("net-Verzeichnis anlegen");
        let mut content = String::from(HEADER);
        content.push('\n');
        for line in lines {
            content.push_str(line);
            content.push('\n');
        }
        fs::write(net_dir.join("tcp"), content).expect("net/tcp schreiben");
    }

    /// Eine Datenzeile mit wählbarem lokalem Port, entfernter Adresse und
    /// Inode, immer im Zustand `0A` (Listener).
    fn listen_line(local_port_hex: &str, rem_address: &str, inode: u64) -> String {
        format!(
            "   0: 0100007F:{local_port_hex} {rem_address} 0A 00000000:00000000 \
             00:00000000 00000000  1000        0 {inode} 1 0000000000000000 100 0 0 10 0"
        )
    }

    fn sensor_for(root: &Path) -> ListenerSensor {
        ListenerSensor::new(SensorId::from_str("listener-test"), root.to_path_buf())
    }

    #[cfg(unix)]
    fn write_socket_fd(root: &Path, pid: u32, fd: u32, inode: u64) {
        let fd_dir = root.join(pid.to_string()).join("fd");
        fs::create_dir_all(&fd_dir).expect("fd-Verzeichnis anlegen");
        symlink(format!("socket:[{inode}]"), fd_dir.join(fd.to_string()))
            .expect("Socket-Symlink anlegen");
    }

    fn write_status(root: &Path, pid: u32, uid: u32) {
        let pid_dir = root.join(pid.to_string());
        fs::create_dir_all(&pid_dir).expect("pid-Verzeichnis anlegen");
        fs::write(
            pid_dir.join("status"),
            format!("Name:\tfixture\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
        )
        .expect("status schreiben");
    }

    fn write_cgroup(root: &Path, pid: u32, path: &str) {
        fs::create_dir_all(root.join(pid.to_string())).expect("pid-Verzeichnis anlegen");
        fs::write(root.join(pid.to_string()).join("cgroup"), format!("0::{path}\n"))
            .expect("cgroup schreiben");
    }

    #[cfg(unix)]
    #[test]
    fn test_poll_reports_listener_with_resolvable_owner_and_cgroup() {
        let dir = tempdir().expect("tempdir");
        write_tcp_table(dir.path(), &[listen_line("1F90", "00000000:0000", 111)]);
        write_socket_fd(dir.path(), 1000, 3, 111);
        write_status(dir.path(), 1000, 1000);
        write_cgroup(dir.path(), 1000, "/user.slice/foo.scope");

        let sensor = sensor_for(dir.path());
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("poll muss gelingen");

        assert!(reading.samples.is_empty(), "dieser Sensor meldet keine Samples");
        assert_eq!(reading.events.len(), 1);
        let event = &reading.events[0];
        assert_eq!(event.kind, EventKind::ListenerOpened { port: 8080 });
        let actor = event.actor.as_ref().expect("Besitzer muss auflösbar sein");
        assert_eq!(actor.uid, 1000);
        assert_eq!(actor.auid, None);
        assert_eq!(
            actor.cgroup.as_ref().map(|c| c.as_str()),
            Some("/user.slice/foo.scope")
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_poll_listener_with_owner_but_no_cgroup_file_reports_cgroup_none() {
        let dir = tempdir().expect("tempdir");
        write_tcp_table(dir.path(), &[listen_line("0050", "00000000:0000", 222)]);
        write_socket_fd(dir.path(), 1000, 3, 222);
        write_status(dir.path(), 1000, 1000);
        // Keine `cgroup`-Datei — der Besitzer ist auflösbar, seine cgroup
        // nicht.

        let sensor = sensor_for(dir.path());
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("poll muss gelingen");

        assert_eq!(reading.events.len(), 1);
        let actor = reading.events[0]
            .actor
            .as_ref()
            .expect("UID allein reicht für einen Actor");
        assert_eq!(actor.uid, 1000);
        assert_eq!(actor.cgroup, None);
    }

    #[cfg(unix)]
    #[test]
    fn test_poll_listener_with_unreadable_owner_process_is_still_reported() {
        let dir = tempdir().expect("tempdir");
        write_tcp_table(dir.path(), &[listen_line("01BB", "00000000:0000", 333)]);
        // `2000` existiert, aber ohne lesbares `fd` — simuliert einen
        // Prozess eines fremden Benutzers. Er darf den Abruf nicht scheitern
        // lassen, und der Listener muss trotzdem gemeldet werden, ohne
        // Besitzer.
        fs::create_dir_all(dir.path().join("2000")).expect("pid-Verzeichnis anlegen");

        let sensor = sensor_for(dir.path());
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("ein unlesbarer Fremdprozess darf den Abruf nicht scheitern lassen");

        assert_eq!(reading.events.len(), 1);
        assert_eq!(
            reading.events[0].kind,
            EventKind::ListenerOpened { port: 443 }
        );
        assert_eq!(
            reading.events[0].actor, None,
            "ohne auflösbaren Besitzer gibt es keinen Actor"
        );
    }

    /// Belegt die Berechtigungsgrenze: kein Feld des gemeldeten Ergebnisses
    /// enthält die entfernte Adresse aus der Quellzeile, geprüft über die
    /// materialisierte (`Debug`-)Darstellung des Ergebnisses. `SecurityEvent`
    /// implementiert `serde::Serialize`, aber `SensorReading` selbst nicht;
    /// die `Debug`-Textform prüft dieselbe Eigenschaft — kein Feld trägt die
    /// Zieladresse —, ohne eine zusätzliche, in der Spezifikation dieser
    /// Crate nicht vorgesehene `serde_json`-Abhängigkeit einzuführen.
    #[cfg(unix)]
    #[test]
    fn test_poll_emitted_result_contains_no_destination_address() {
        let dir = tempdir().expect("tempdir");
        // Eine offensichtlich erfundene, gut erkennbare "entfernte Adresse" —
        // für eine echte LISTEN-Zeile untypisch, aber genau deshalb ein
        // scharfer Test: sie darf unter keinen Umständen im Ergebnis
        // auftauchen.
        write_tcp_table(dir.path(), &[listen_line("1F90", "0A0A0A01:01BB", 444)]);
        write_socket_fd(dir.path(), 1000, 3, 444);
        write_status(dir.path(), 1000, 1000);
        write_cgroup(dir.path(), 1000, "/user.slice/foo.scope");

        let sensor = sensor_for(dir.path());
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("poll muss gelingen");

        let materialized = format!("{:?}", reading.events);
        assert!(
            !materialized.contains("0A0A0A01"),
            "die entfernte Adresse darf im Ergebnis nicht auftauchen: {materialized}"
        );
        assert!(
            !materialized.contains("01BB"),
            "der entfernte Port darf im Ergebnis nicht auftauchen: {materialized}"
        );
    }

    #[test]
    fn test_poll_missing_ipv6_and_udp_tables_yield_no_error() {
        let dir = tempdir().expect("tempdir");
        // Nur `net/tcp` existiert — `net/tcp6`, `net/udp`, `net/udp6` fehlen
        // (z. B. deaktiviertes IPv6). Das darf den Abruf nicht scheitern
        // lassen (siehe `crate::procnet::read_table`).
        write_tcp_table(dir.path(), &[]);

        let sensor = sensor_for(dir.path());
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("fehlende Tabellen dürfen den Abruf nicht scheitern lassen");
        assert!(reading.events.is_empty());
    }

    /// Determinismus: derselbe Fixture-Baum und dieselbe injizierte Zeit
    /// liefern bei jedem Abruf dasselbe Ergebnis — dieser Sensor liest nie
    /// die Systemuhr selbst (siehe `harw_dod_signals::sensor`-Moduldokumentation).
    #[cfg(unix)]
    #[test]
    fn test_poll_is_deterministic_for_same_fixture_and_timestamp() {
        let dir = tempdir().expect("tempdir");
        write_tcp_table(dir.path(), &[listen_line("1F90", "00000000:0000", 111)]);
        write_socket_fd(dir.path(), 1000, 3, 111);
        write_status(dir.path(), 1000, 1000);
        write_cgroup(dir.path(), 1000, "/user.slice/foo.scope");

        let sensor = sensor_for(dir.path());
        let first = sensor.poll(Timestamp::UNIX_EPOCH).expect("erster Abruf");
        let second = sensor.poll(Timestamp::UNIX_EPOCH).expect("zweiter Abruf");

        assert_eq!(first, second);
    }

    /// Kardinalität: mehrere gleichzeitig offene Listener erzeugen genau je
    /// ein Ereignis — keine Duplikate, keine zusätzlichen Einträge.
    #[test]
    fn test_poll_reports_one_event_per_listener_with_no_duplicates() {
        let dir = tempdir().expect("tempdir");
        write_tcp_table(
            dir.path(),
            &[
                listen_line("0050", "00000000:0000", 1),
                listen_line("01BB", "00000000:0000", 2),
                listen_line("1F90", "00000000:0000", 3),
            ],
        );

        let sensor = sensor_for(dir.path());
        let reading = sensor.poll(Timestamp::UNIX_EPOCH).expect("poll muss gelingen");

        assert_eq!(reading.events.len(), 3, "genau ein Ereignis je Listener");
        let mut ports: Vec<u16> = reading
            .events
            .iter()
            .map(|event| match event.kind {
                EventKind::ListenerOpened { port } => port,
                _ => panic!("unerwartete EventKind-Variante in einem Listener-Sensor-Ergebnis"),
            })
            .collect();
        ports.sort_unstable();
        assert_eq!(ports, vec![80, 443, 8080]);
    }

    /// Fehlerfall auf Abrufebene: eine fehlerhafte Zeile in einer Tabelle
    /// lässt den gesamten Abruf scheitern, statt sie stillschweigend zu
    /// überspringen — konsistent mit `parse_listener_line`s
    /// `MalformedSource`-Vertrag.
    #[test]
    fn test_poll_fails_when_a_table_line_is_malformed() {
        let dir = tempdir().expect("tempdir");
        // Absichtlich zu wenige Spalten (fehlt: st, Zwischenspalten, inode).
        write_tcp_table(dir.path(), &["   0: 0100007F:0050 00000000:0000".to_owned()]);

        let sensor = sensor_for(dir.path());
        let err = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect_err("eine fehlerhafte Zeile muss den Abruf scheitern lassen");
        assert!(matches!(err, SensorError::MalformedSource));
    }
}
