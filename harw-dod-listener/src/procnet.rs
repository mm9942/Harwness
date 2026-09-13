//! Parser für die Kernel-Listener-Tabellen unter `/proc/net`.
//!
//! # Verantwortungsbereich
//! Liest `net/tcp`, `net/tcp6`, `net/udp` und `net/udp6` relativ zu einer
//! `/proc`-Wurzel und liefert für jede Zeile im Zustand `0A` (siehe
//! [`parse_listener_line`] für das Spaltenformat) einen [`ListenerRecord`]:
//! Port und Socket-Inode. Diese Datei kennt **keine** cgroup, keine PID und
//! keine Verbindungsziele — das ist Aufgabe von [`crate::owner`] bzw. bewusst
//! nirgends in dieser Crate, siehe Crate-Dokumentation für die
//! Berechtigungsgrenze.
//!
//! # Warum `st == "0A"` über alle vier Tabellen hinweg
//! `0A` ist der TCP-Zustandscode für `TCP_LISTEN`
//! (`include/net/tcp_states.h` im Kernel). `/proc/net/udp` und
//! `/proc/net/udp6` benutzen dieselbe Spaltenposition für einen numerisch
//! kompatiblen Zustandscode; ein UDP-Socket, der auf eingehenden Verkehr
//! wartet, ohne verbunden zu sein, erscheint dort nicht immer unter `0A` in
//! jeder Kernel-Version — diese Crate wertet dennoch einheitlich `0A` über
//! alle vier Quellen aus, wie in der Spezifikation dieses Knotens (AW2-14)
//! festgelegt, statt vier verschiedene Zustandsvokabulare zu pflegen.
//!
//! # Nebenläufigkeit
//! Zustandslose freie Funktionen; `Send + Sync`, von jedem Thread parallel
//! aufrufbar.
//!
//! # Fehler
//! [`harw_dod_cap::SensorError::MalformedSource`] für eine Zeile mit zu
//! wenigen Spalten oder einer nicht hexadezimal lesbaren Adress-/Inode-Form —
//! **nie** mit dem Zeileninhalt selbst in der Meldung, da
//! `MalformedSource` keine Nutzdaten trägt. Fehler beim Lesen einer Tabelle
//! selbst laufen über [`harw_dod_readfs::ReadFsError`] und werden von
//! [`collect_listeners`] auf [`harw_dod_cap::SensorError`] abgebildet; eine
//! auf diesem Host fehlende Tabelle (z. B. deaktiviertes IPv6) liefert dabei
//! keine Treffer, statt den gesamten Abruf scheitern zu lassen.
//!
//! # Examples
//! ```rust
//! use harw_dod_listener::procnet::parse_listener_line;
//!
//! let line = "   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 \
//!              00000000  1000        0 12345 1 0000000000000000 100 0 0 10 0";
//! let record = parse_listener_line(line).expect("Zeile ist wohlgeformt").expect("Zustand 0A");
//! assert_eq!(record.port, 8080);
//! assert_eq!(record.inode, 12345);
//! ```

use std::path::Path;

use harw_dod_cap::{ReadScope, SensorError};

/// Der hexadezimale Zustandscode einer `TCP_LISTEN`-Zeile (Kernel:
/// `include/net/tcp_states.h`), einheitlich über alle vier Tabellen
/// ausgewertet (siehe Modul-Dokumentation).
const LISTEN_STATE: &str = "0A";

/// Mindestzahl durch Leerraum getrennter Spalten, die eine Datenzeile aus
/// `/proc/net/{tcp,tcp6,udp,udp6}` tragen muss, um `local_address` (Spalte 1),
/// `st` (Spalte 3) und `inode` (Spalte 9) zu erreichen — siehe
/// [`parse_listener_line`] für die vollständige Spaltenaufschlüsselung.
const MIN_COLUMNS: usize = 10;

/// Ein offener Listener-Socket, wie er aus einer Zeile von
/// `/proc/net/{tcp,tcp6,udp,udp6}` gelesen wurde.
///
/// # Description
/// Trägt ausschließlich, was zur Meldung "dieser Port lauscht, dieser Prozess
/// gehört dazu" nötig ist. Kein Feld dieses Typs — und keiner seiner
/// Konsumenten in dieser Crate — trägt eine Ziel- oder Verbindungsadresse
/// (siehe Crate-Dokumentation, Abschnitt "Berechtigungsgrenze").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenerRecord {
    /// Der lokale Port, auf dem der Socket lauscht.
    pub port: u16,
    /// Die Kernel-Inode-Nummer des Sockets — der Schlüssel, über den
    /// [`crate::owner::resolve_owners`] den besitzenden Prozess findet.
    pub inode: u64,
}

/// Parst genau eine Zeile aus `/proc/net/{tcp,tcp6,udp,udp6}`.
///
/// # Description
/// Das Spaltenformat (Leerraum-getrennt, 0-basiert), unverändert seit
/// `net/ipv4/tcp_ipv4.c`s `get_tcp4_sock` im Kernel:
///
/// ```text
///   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
///    0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 12345 1 0000000000000000 100 0 0 10 0
/// ```
///
/// Spalte 0 ist die (hier ignorierte) fortlaufende Zeilennummer, Spalte 1
/// `local_address` in der Form `HEXADRESSE:HEXPORT`, Spalte 2 `rem_address`
/// (wird von dieser Funktion **nie gelesen** — siehe Crate-Dokumentation,
/// Berechtigungsgrenze), Spalte 3 `st` der hexadezimale Zustandscode, Spalte 9
/// `inode` die Kernel-Inode-Nummer des Sockets. Für IPv6 (`tcp6`/`udp6`) ist
/// `local_address` 32 statt 8 Hexzeichen lang (128- statt 32-Bit-Adresse);
/// das Format bleibt sonst identisch, weshalb dieselbe Funktion beide Formen
/// liest (siehe Modultests).
///
/// Die Kopfzeile jeder Tabelle (`  sl  local_address ...`) hat in ihrer
/// dritten Spalte den Literal-Text `st`, der nie `"0A"` entspricht — sie
/// durchläuft diese Funktion deshalb gefahrlos als „kein Listener" (`Ok(None)`),
/// ohne gesondert erkannt werden zu müssen.
///
/// # Arguments
/// - `line` (`&str`): eine einzelne Zeile, ohne Zeilenumbruch.
///
/// # Returns
/// `Ok(Some(record))`, wenn die Zeile im Zustand [`LISTEN_STATE`] ist;
/// `Ok(None)`, wenn sie eine andere Zeile ist (anderer Zustand oder
/// Kopfzeile).
///
/// # Errors
/// [`SensorError::MalformedSource`] — inhaltsfrei, ohne den Zeileninhalt in
/// der Meldung —, wenn die Zeile weniger als [`MIN_COLUMNS`] Spalten hat,
/// `local_address` kein `HEX:HEX`-Paar ist, oder Port bzw. Inode nicht als
/// Hexadezimal- bzw. Dezimalzahl lesbar sind.
///
/// # Examples
/// ```rust
/// use harw_dod_listener::procnet::parse_listener_line;
///
/// let established = "   1: 0100007F:0050 0100007F:C350 01 00000000:00000000 00:00000000 \
///                      00000000  1000        0 22222 1 0000000000000000 100 0 0 10 0";
/// assert!(parse_listener_line(established).unwrap().is_none());
/// ```
pub fn parse_listener_line(line: &str) -> Result<Option<ListenerRecord>, SensorError> {
    let columns: Vec<&str> = line.split_whitespace().collect();
    if columns.len() < MIN_COLUMNS {
        return Err(SensorError::MalformedSource);
    }

    if columns[3] != LISTEN_STATE {
        return Ok(None);
    }

    let port = parse_local_port(columns[1])?;
    let inode = columns[9]
        .parse::<u64>()
        .map_err(|_| SensorError::MalformedSource)?;

    Ok(Some(ListenerRecord { port, inode }))
}

/// Liest den Port aus der `local_address`-Spalte (`HEXADRESSE:HEXPORT`).
///
/// Liest **alle vier** Hexziffern des Portanteils über
/// [`u16::from_str_radix`] — ein Fehler, der nur die niederwertigen zwei
/// Ziffern läse, wäre für jeden Port unter 256 zufällig noch korrekt (die
/// oberen zwei Ziffern sind dort `00`) und fiele erst bei einem Port über 255
/// auf (siehe Modultests).
fn parse_local_port(local_address: &str) -> Result<u16, SensorError> {
    let (_address, port_hex) = local_address
        .split_once(':')
        .ok_or(SensorError::MalformedSource)?;
    u16::from_str_radix(port_hex, 16).map_err(|_| SensorError::MalformedSource)
}

/// Bildet einen [`harw_dod_readfs::ReadFsError`] auf den inhaltsfreien
/// [`SensorError`] ab, den [`harw_dod_signals::Sensor::poll`] laut Vertrag
/// zurückgeben muss.
///
/// [`harw_dod_readfs::ReadFsError::Scope`] entpackt den bereits inhaltsfreien
/// `SensorError`, den es umschließt. Die drei übrigen Varianten
/// (`TooLarge`, `GlobPatternAbsolute`, `GlobPatternTraversal`) sind für diese
/// Crate praktisch unerreichbar — sie liest nie über
/// [`harw_dod_readfs::MAX_READ_BYTES`] hinaus (die vier Listener-Tabellen
/// bleiben weit darunter) und ruft `harw_dod_readfs::glob` nie auf —, bleiben
/// aber aus Erschöpfungsgründen als defensiver Fallback auf
/// [`SensorError::MalformedSource`] erhalten.
fn map_readfs_err(err: harw_dod_readfs::ReadFsError) -> SensorError {
    match err {
        harw_dod_readfs::ReadFsError::Scope(inner) => inner,
        harw_dod_readfs::ReadFsError::TooLarge { .. }
        | harw_dod_readfs::ReadFsError::GlobPatternAbsolute { .. }
        | harw_dod_readfs::ReadFsError::GlobPatternTraversal { .. } => {
            SensorError::MalformedSource
        }
    }
}

/// Liest eine einzelne Listener-Tabelle, `path` relativ zu `root`.
///
/// Eine auf diesem Host fehlende Tabelle (z. B. `net/tcp6` bei
/// deaktiviertem IPv6: `std::io::ErrorKind::NotFound`) liefert eine leere
/// Zeilenliste statt eines Fehlers — die Abwesenheit einer Protokollfamilie
/// ist eine Eigenschaft dieses Hosts, kein Sensorfehler. Jeder andere Fehler
/// (z. B. eine Bereichsverletzung) wird weitergereicht.
fn read_table(scope: &ReadScope, path: &Path) -> Result<Vec<String>, SensorError> {
    match harw_dod_readfs::read_lines(scope, path) {
        Ok(lines) => Ok(lines),
        Err(harw_dod_readfs::ReadFsError::Scope(SensorError::Io(io_err)))
            if io_err.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(Vec::new())
        }
        Err(other) => Err(map_readfs_err(other)),
    }
}

/// Liest alle vier Listener-Tabellen unter `root` und liefert die Vereinigung
/// ihrer `0A`-Treffer.
///
/// # Description
/// Liest `root.join("net/tcp")`, `root.join("net/tcp6")`,
/// `root.join("net/udp")` und `root.join("net/udp6")`, in dieser Reihenfolge,
/// über [`harw_dod_readfs::read_lines`], und parst jede Zeile über
/// [`parse_listener_line`]. `root` ist in Produktion `/proc`; ein Test setzt
/// hier die Wurzel eines Fixture-Baums (siehe [`crate::sensor`]-Tests).
///
/// # Arguments
/// - `scope` (`&ReadScope`): der Lesebereich, gegen den jede Tabelle geprüft
///   wird.
/// - `root` (`&Path`): die `/proc`-Wurzel (oder ihr Fixture-Äquivalent).
///
/// # Returns
/// Alle gefundenen [`ListenerRecord`]s, in Lesereihenfolge der vier Tabellen
/// und, innerhalb jeder Tabelle, in Dateireihenfolge.
///
/// # Errors
/// [`SensorError`], wenn eine vorhandene Tabelle nicht gelesen werden kann
/// (außerhalb des Bereichs, E/A-Fehler abseits „nicht vorhanden") oder eine
/// ihrer Zeilen fehlerhaft ist (siehe [`parse_listener_line`]).
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
pub fn collect_listeners(scope: &ReadScope, root: &Path) -> Result<Vec<ListenerRecord>, SensorError> {
    let mut records = Vec::new();
    for relative in ["net/tcp", "net/tcp6", "net/udp", "net/udp6"] {
        let path = root.join(relative);
        let lines = read_table(scope, &path)?;
        for line in &lines {
            if let Some(record) = parse_listener_line(line)? {
                records.push(record);
            }
        }
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Baut eine realistische Datenzeile mit wählbarem Zustand, lokalem Port
    /// und Inode; die übrigen Spalten sind feste, plausible Platzhalter.
    fn data_line(state: &str, local_port_hex: &str, inode: u64) -> String {
        format!(
            "   0: 0100007F:{local_port_hex} 00000000:0000 {state} 00000000:00000000 \
             00:00000000 00000000  1000        0 {inode} 1 0000000000000000 100 0 0 10 0"
        )
    }

    const HEADER: &str =
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";

    #[test]
    fn test_parse_listener_line_state_0a_is_recognized_as_listener() {
        let line = data_line("0A", "0050", 12345);
        let record = parse_listener_line(&line)
            .expect("wohlgeformte Zeile")
            .expect("Zustand 0A muss als Listener erkannt werden");
        assert_eq!(record.port, 80);
        assert_eq!(record.inode, 12345);
    }

    #[test]
    fn test_parse_listener_line_other_state_is_not_a_listener() {
        let line = data_line("01", "0050", 12345);
        assert_eq!(
            parse_listener_line(&line).expect("wohlgeformte Zeile"),
            None,
            "nur Zustand 0A darf als Listener zählen"
        );
    }

    /// Port über 255 prüft, dass alle vier Hexziffern gelesen werden — ein
    /// Fehler, der nur zwei Ziffern läse, wäre für Ports unter 256 zufällig
    /// noch korrekt (siehe Doku von `parse_local_port`).
    #[test]
    fn test_parse_listener_line_reads_port_above_255_correctly() {
        let line = data_line("0A", "1F90", 99);
        let record = parse_listener_line(&line)
            .expect("wohlgeformte Zeile")
            .expect("Zustand 0A");
        assert_eq!(record.port, 8080);
    }

    #[test]
    fn test_parse_listener_line_works_for_ipv6_style_address() {
        let line = "   0: 00000000000000000000000000000000:1F90 \
                     00000000000000000000000000000000:0000 0A 00000000:00000000 \
                     00:00000000 00000000  1000        0 54321 1 0000000000000000 100 0 0 10 0";
        let record = parse_listener_line(line)
            .expect("wohlgeformte IPv6-Zeile")
            .expect("Zustand 0A");
        assert_eq!(record.port, 8080);
        assert_eq!(record.inode, 54321);
    }

    #[test]
    fn test_parse_listener_line_header_row_is_not_a_listener_and_not_an_error() {
        assert_eq!(
            parse_listener_line(HEADER).expect("Kopfzeile hat genug Spalten"),
            None
        );
    }

    #[test]
    fn test_parse_listener_line_too_few_columns_is_malformed_without_line_content_in_message() {
        let short_line = "   0: 0100007F:0050 00000000:0000 0A";
        let err = parse_listener_line(short_line).expect_err("zu wenige Spalten muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
        let message = err.to_string();
        assert!(
            !message.contains("0100007F"),
            "die Fehlermeldung darf den Zeileninhalt nicht enthalten: {message}"
        );
    }

    #[test]
    fn test_parse_listener_line_non_hex_port_is_malformed() {
        let line = data_line("0A", "ZZZZ", 1);
        let err = parse_listener_line(&line).expect_err("nicht-hexadezimaler Port muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
    }

    /// Scope-Dichtheit: `collect_listeners` liest niemals außerhalb des
    /// übergebenen `ReadScope`, selbst wenn `root` auf ein tatsächlich
    /// existierendes Verzeichnis zeigt.
    #[test]
    fn test_collect_listeners_rejects_root_outside_scope() {
        let real_root = tempfile::tempdir().expect("tempdir für die echte Wurzel");
        let other_root = tempfile::tempdir().expect("tempdir außerhalb des Bereichs");
        std::fs::create_dir_all(real_root.path().join("net")).expect("net-Verzeichnis anlegen");
        std::fs::write(
            real_root.path().join("net").join("tcp"),
            data_line("0A", "0050", 1),
        )
        .expect("net/tcp schreiben");

        let scope = ReadScope::from_roots([other_root.path().to_path_buf()]);
        let err = collect_listeners(&scope, real_root.path())
            .expect_err("eine Wurzel außerhalb des Bereichs muss scheitern");
        assert!(matches!(err, SensorError::OutsideScope));
    }
}
