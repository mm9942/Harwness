//! Parser für die Kernel-Listener-Tabellen unter `/proc/net`.
//!
//! # Verantwortungsbereich
//! Liest `net/tcp`, `net/tcp6`, `net/udp` und `net/udp6` relativ zu einer
//! `/proc`-Wurzel und liefert für jede Zeile im jeweils passenden
//! "lauschend"-Zustand (siehe [`TCP_LISTEN_STATE`]/[`UDP_UNCONNECTED_STATE`]
//! und [`parse_listener_line`] für das Spaltenformat) einen
//! [`ListenerRecord`]: Port und Socket-Inode. Diese Datei kennt **keine**
//! cgroup, keine PID und keine Verbindungsziele — das ist Aufgabe von
//! [`crate::owner`] bzw. bewusst nirgends in dieser Crate, siehe
//! Crate-Dokumentation für die Berechtigungsgrenze.
//!
//! # F-065-Nachtrag: TCP und UDP brauchen **unterschiedliche** Zustandscodes
//! Diese Datei behandelte `net/tcp`, `net/tcp6`, `net/udp` und `net/udp6`
//! ursprünglich einheitlich: nur der Zustandscode `0A` (`TCP_LISTEN`,
//! `include/net/tcp_states.h`) zählte als „lauschend". Für TCP stimmt das.
//! Für UDP nicht: der Kernel (`net/ipv4/udp.c`s `udp4_seq_show` /
//! `get_udp4_sock`) meldet für einen gebundenen, aber nicht per `connect()`
//! auf einen Peer festgelegten UDP-Socket — **genau der Fall, den ein
//! Administrator "dieser UDP-Port lauscht" nennen würde** — denselben
//! numerischen Code wie `TCP_CLOSE` (`07`), **nicht** `TCP_LISTEN` (`0A`).
//!
//! Belegt durch eine echte Erhebung auf diesem Raspberry Pi 5
//! (`harw-dod-fixtures/captures/rpi5-6.18/procnet.json`, Kernel
//! `6.18.34+rpt-rpi-2712`, 2026-09-13): **jeder** dort erfasste, tatsächlich
//! gebundene UDP-Socket (z. B. Zeile `sl 118`, Port `0xA2A9`; Zeile `sl 694`,
//! Port `0x14E9`) trägt den Zustandscode `07`. Keine Zeile in der erfassten
//! `/proc/net/udp`-Tabelle trägt `0A`. Vor dieser Korrektur las diese Datei
//! `net/udp`/`net/udp6` zwar strukturell (die vier Tabellen wurden alle
//! durchlaufen), aber der `0A`-Filter hätte **keinen einzigen** realen
//! UDP-Listener je bestehen lassen — die UDP-Unterstützung war vorhanden,
//! aber funktionslos. [`collect_listeners`] wertet TCP-Tabellen deshalb
//! jetzt gegen [`TCP_LISTEN_STATE`] (`0A`) und UDP-Tabellen gegen
//! [`UDP_UNCONNECTED_STATE`] (`07`) aus.
//!
//! # Nebenläufigkeit
//! Zustandslose freie Funktionen; `Send + Sync`, von jedem Thread parallel
//! aufrufbar.
//!
//! # Fehler
//! [`harw_dod_cap::SensorError::MalformedSource`] für eine Zeile mit zu
//! wenigen Spalten oder einer nicht hexadezimal lesbaren Adress-/Inode-Form —
//! **nie** mit dem Zeileninhalt selbst in der Meldung, da
//! `MalformedSource` keine Nutzdaten trägt. [`read_table`] liest jede Tabelle
//! zeilenweise direkt über [`harw_dod_cap::ReadScope::open`] (siehe dortige
//! Dokumentation für den erschöpfenden Fehlervergleich); eine auf diesem
//! Host fehlende Tabelle (z. B. deaktiviertes IPv6) liefert dabei keine
//! Treffer, statt den gesamten Abruf scheitern zu lassen.
//!
//! # F-065: warum diese Datei nicht mehr über `harw_dod_readfs::read_lines` liest
//! Vor dieser Korrektur las [`read_table`] jede Tabelle über
//! `harw_dod_readfs::read_lines`, das den gesamten Dateiinhalt zunächst als
//! **eine** `String` puffert, begrenzt durch dessen pauschale
//! `harw_dod_readfs::MAX_READ_BYTES`-Grenze von 1 MiB. Laut Register-Befund
//! F-065 reichen rund 7000 gleichzeitig offene Sockets — für einen
//! unprivilegierten lokalen Nutzer ohne besondere Rechte leicht erreichbar
//! (`socket()`+`bind()`+`listen()` bis zum eigenen `ulimit`) —, um
//! `/proc/net/tcp` über diese Grenze zu treiben. Das Ergebnis war nicht
//! „diese eine Tabelle liefert weniger Treffer", sondern
//! `ReadFsError::TooLarge` → der **gesamte** Abruf dieses Sensors scheiterte
//! mit `SensorError::MalformedSource`.
//!
//! [`read_table`] öffnet die Tabelle deshalb direkt über
//! [`harw_dod_cap::ReadScope::open`] und liest sie zeilenweise über
//! `std::io::BufRead`, mit zwei eigenen, deutlich großzügigeren Grenzen
//! ([`MAX_TABLE_BYTES`], [`MAX_TABLE_LINES`]) statt der einen pauschalen
//! 1-MiB-Grenze für beliebige sysfs-/procfs-Inhalte. Ein sehr geschäftiger,
//! aber legitimer Host bleibt darunter; wird eine der beiden Grenzen
//! dennoch erreicht, bricht diese Funktion **nicht** den Abruf ab, sondern
//! wertet die bis dahin gelesenen Zeilen aus — ein Sensor, der unter Last
//! degradiert meldet, statt vollständig zu verstummen.
//!
//! # Examples
//! ```rust
//! use harw_dod_listener::procnet::{parse_listener_line, TCP_LISTEN_STATE};
//!
//! let line = "   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 \
//!              00000000  1000        0 12345 1 0000000000000000 100 0 0 10 0";
//! let record = parse_listener_line(line, TCP_LISTEN_STATE)
//!     .expect("Zeile ist wohlgeformt")
//!     .expect("Zustand 0A");
//! assert_eq!(record.port, 8080);
//! assert_eq!(record.inode, 12345);
//! ```

use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use harw_dod_cap::{ReadScope, SensorError};

/// Obergrenze für die **Gesamtzahl an Bytes**, die aus einer einzelnen
/// `/proc/net/{tcp,tcp6,udp,udp6}`-Tabelle gelesen werden (F-065). Deutlich
/// höher als die frühere pauschale 1-MiB-Grenze aus
/// `harw_dod_readfs::read_lines`, aber weiterhin endlich: kein Sensor liest
/// je unbegrenzt viele Bytes von einer einzelnen Quelle. 16 MiB reichen für
/// weit über 100 000 gleichzeitig offene Sockets (eine Zeile ist rund
/// 150 Byte lang, siehe [`parse_listener_line`]s Format-Dokumentation) — ein
/// Vielfaches der ~7000 Sockets, mit denen F-065 den vorherigen Sensor zum
/// vollständigen Ausfall bringen konnte.
const MAX_TABLE_BYTES: u64 = 16 << 20;

/// Obergrenze für die **Zeilenzahl**, die aus einer einzelnen Tabelle
/// ausgewertet wird (F-065). Ergänzt [`MAX_TABLE_BYTES`]: verhindert, dass
/// eine Tabelle mit vielen sehr kurzen Zeilen die CPU-Zeit dieses Sensors
/// unbegrenzt beansprucht, selbst wenn die Byte-Grenze (noch) nicht erreicht
/// ist. Wird eine der beiden Grenzen erreicht, bricht [`read_table`] nicht
/// ab — es wertet die bis dahin gelesenen Zeilen aus.
const MAX_TABLE_LINES: usize = 200_000;

/// Der hexadezimale Zustandscode einer `TCP_LISTEN`-Zeile in
/// `/proc/net/{tcp,tcp6}` (Kernel: `include/net/tcp_states.h`).
pub const TCP_LISTEN_STATE: &str = "0A";

/// Der hexadezimale Zustandscode eines gebundenen, nicht per `connect()`
/// festgelegten UDP-Sockets in `/proc/net/{udp,udp6}` — des UDP-Äquivalents
/// eines TCP-Listeners. Siehe Moduldokumentation, Abschnitt
/// „F-065-Nachtrag", für die Kernel-Quelle und den Nachweis anhand einer
/// echten Erhebung.
pub const UDP_UNCONNECTED_STATE: &str = "07";

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
/// dritten Spalte den Literal-Text `st`, der nie einem der beiden
/// "lauschend"-Zustandscodes entspricht — sie durchläuft diese Funktion
/// deshalb gefahrlos als „kein Listener" (`Ok(None)`), ohne gesondert
/// erkannt werden zu müssen.
///
/// # Arguments
/// - `line` (`&str`): eine einzelne Zeile, ohne Zeilenumbruch.
/// - `expected_state` (`&str`): der Zustandscode, der für diese Tabelle als
///   "lauschend" zählt — [`TCP_LISTEN_STATE`] für `net/tcp`/`net/tcp6`,
///   [`UDP_UNCONNECTED_STATE`] für `net/udp`/`net/udp6` (siehe
///   Moduldokumentation, Abschnitt „F-065-Nachtrag", für die Begründung,
///   warum diese beiden Tabellenpaare unterschiedliche Codes brauchen).
///
/// # Returns
/// `Ok(Some(record))`, wenn die Zeile im Zustand `expected_state` ist;
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
/// use harw_dod_listener::procnet::{parse_listener_line, TCP_LISTEN_STATE};
///
/// let established = "   1: 0100007F:0050 0100007F:C350 01 00000000:00000000 00:00000000 \
///                      00000000  1000        0 22222 1 0000000000000000 100 0 0 10 0";
/// assert!(parse_listener_line(established, TCP_LISTEN_STATE).unwrap().is_none());
/// ```
pub fn parse_listener_line(
    line: &str,
    expected_state: &str,
) -> Result<Option<ListenerRecord>, SensorError> {
    let columns: Vec<&str> = line.split_whitespace().collect();
    if columns.len() < MIN_COLUMNS {
        return Err(SensorError::MalformedSource);
    }

    if columns[3] != expected_state {
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

/// Liest eine einzelne Listener-Tabelle, `path` relativ zu `root`, zeilenweise.
///
/// # Description
/// Öffnet `path` über [`harw_dod_cap::ReadScope::open`] (Bereichsprüfung und
/// Symlink-Auflösung passieren dort, nicht hier) und liest sie danach über
/// `std::io::BufRead::lines`, begrenzt durch [`MAX_TABLE_BYTES`] (über
/// `Read::take`) und [`MAX_TABLE_LINES`] — siehe Moduldokumentation,
/// Abschnitt „F-065" für die Begründung, warum diese Datei dafür nicht mehr
/// `harw_dod_readfs::read_lines` verwendet. Eine auf diesem Host fehlende
/// Tabelle (z. B. `net/tcp6` bei deaktiviertem IPv6:
/// `std::io::ErrorKind::NotFound`) liefert eine leere Zeilenliste statt
/// eines Fehlers — die Abwesenheit einer Protokollfamilie ist eine
/// Eigenschaft dieses Hosts, kein Sensorfehler. Jeder andere Fehler beim
/// Öffnen (z. B. eine Bereichsverletzung) wird weitergereicht; ein E/A-Fehler
/// **während** des zeilenweisen Lesens (z. B. ungültiges UTF-8 in einer
/// Zeile) wird über [`SensorError::from`] auf [`SensorError::Io`] abgebildet.
///
/// # Arguments
/// - `scope` (`&ReadScope`): der Lesebereich, gegen den `path` geprüft wird.
/// - `path` (`&Path`): die zu lesende Tabelle.
///
/// # Returns
/// Die gelesenen Zeilen, ohne Zeilenumbrüche, in Dateireihenfolge, gekappt
/// bei [`MAX_TABLE_LINES`] Zeilen bzw. [`MAX_TABLE_BYTES`] Bytes.
///
/// # Errors
/// [`SensorError`], wenn das Öffnen (außer „nicht vorhanden") oder das
/// zeilenweise Lesen selbst scheitert.
fn read_table(scope: &ReadScope, path: &Path) -> Result<Vec<String>, SensorError> {
    let file = match scope.open(path) {
        Ok(file) => file,
        Err(SensorError::Io(io_err)) if io_err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(other) => return Err(other),
    };

    let limited = file.take(MAX_TABLE_BYTES);
    let reader = BufReader::new(limited);

    let mut lines = Vec::new();
    for line in reader.lines() {
        if lines.len() >= MAX_TABLE_LINES {
            break;
        }
        lines.push(line.map_err(SensorError::from)?);
    }
    Ok(lines)
}

/// Liest alle vier Listener-Tabellen unter `root` und liefert die Vereinigung
/// ihrer "lauschend"-Treffer.
///
/// # Description
/// Liest `root.join("net/tcp")` und `root.join("net/tcp6")` gegen
/// [`TCP_LISTEN_STATE`], sowie `root.join("net/udp")` und
/// `root.join("net/udp6")` gegen [`UDP_UNCONNECTED_STATE`] (siehe
/// Moduldokumentation, Abschnitt „F-065-Nachtrag", für die Begründung der
/// unterschiedlichen Codes), in dieser Reihenfolge, über [`read_table`], und
/// parst jede Zeile über [`parse_listener_line`]. `root` ist in Produktion
/// `/proc`; ein Test setzt hier die Wurzel eines Fixture-Baums (siehe
/// [`crate::sensor`]-Tests).
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
pub fn collect_listeners(
    scope: &ReadScope,
    root: &Path,
) -> Result<Vec<ListenerRecord>, SensorError> {
    let mut records = Vec::new();
    for (relative, expected_state) in [
        ("net/tcp", TCP_LISTEN_STATE),
        ("net/tcp6", TCP_LISTEN_STATE),
        ("net/udp", UDP_UNCONNECTED_STATE),
        ("net/udp6", UDP_UNCONNECTED_STATE),
    ] {
        let path = root.join(relative);
        let lines = read_table(scope, &path)?;
        for line in &lines {
            if let Some(record) = parse_listener_line(line, expected_state)? {
                records.push(record);
            }
        }
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Baut eine realistische Datenzeile mit wählbarem Zustand, lokalem Port
    /// und Inode; die übrigen Spalten sind feste, plausible Platzhalter.
    fn data_line(state: &str, local_port_hex: &str, inode: u64) -> String {
        format!(
            "   0: 0100007F:{local_port_hex} 00000000:0000 {state} 00000000:00000000 \
             00:00000000 00000000  1000        0 {inode} 1 0000000000000000 100 0 0 10 0"
        )
    }

    const HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";

    #[test]
    fn test_parse_listener_line_state_0a_is_recognized_as_listener() -> TestResult {
        let line = data_line("0A", "0050", 12345);
        let record = parse_listener_line(&line, TCP_LISTEN_STATE)
            .map_err(ctx("wohlgeformte Zeile"))?
            .ok_or(TestError::Missing(
                "Zustand 0A muss als Listener erkannt werden",
            ))?;
        assert_eq!(record.port, 80);
        assert_eq!(record.inode, 12345);
        Ok(())
    }

    #[test]
    fn test_parse_listener_line_other_state_is_not_a_listener() -> TestResult {
        let line = data_line("01", "0050", 12345);
        assert_eq!(
            parse_listener_line(&line, TCP_LISTEN_STATE).map_err(ctx("wohlgeformte Zeile"))?,
            None,
            "nur Zustand 0A darf für eine TCP-Tabelle als Listener zählen"
        );
        Ok(())
    }

    /// Port über 255 prüft, dass alle vier Hexziffern gelesen werden — ein
    /// Fehler, der nur zwei Ziffern läse, wäre für Ports unter 256 zufällig
    /// noch korrekt (siehe Doku von `parse_local_port`).
    #[test]
    fn test_parse_listener_line_reads_port_above_255_correctly() -> TestResult {
        let line = data_line("0A", "1F90", 99);
        let record = parse_listener_line(&line, TCP_LISTEN_STATE)
            .map_err(ctx("wohlgeformte Zeile"))?
            .ok_or(TestError::Missing("Zustand 0A"))?;
        assert_eq!(record.port, 8080);
        Ok(())
    }

    #[test]
    fn test_parse_listener_line_works_for_ipv6_style_address() -> TestResult {
        let line = "   0: 00000000000000000000000000000000:1F90 \
                     00000000000000000000000000000000:0000 0A 00000000:00000000 \
                     00:00000000 00000000  1000        0 54321 1 0000000000000000 100 0 0 10 0";
        let record = parse_listener_line(line, TCP_LISTEN_STATE)
            .map_err(ctx("wohlgeformte IPv6-Zeile"))?
            .ok_or(TestError::Missing("Zustand 0A"))?;
        assert_eq!(record.port, 8080);
        assert_eq!(record.inode, 54321);
        Ok(())
    }

    #[test]
    fn test_parse_listener_line_header_row_is_not_a_listener_and_not_an_error() -> TestResult {
        assert_eq!(
            parse_listener_line(HEADER, TCP_LISTEN_STATE)
                .map_err(ctx("Kopfzeile hat genug Spalten"))?,
            None
        );
        Ok(())
    }

    #[test]
    fn test_parse_listener_line_too_few_columns_is_malformed_without_line_content_in_message()
    -> TestResult {
        let short_line = "   0: 0100007F:0050 00000000:0000 0A";
        let Err(err) = parse_listener_line(short_line, TCP_LISTEN_STATE) else {
            return Err(TestError::Unexpected(
                "zu wenige Spalten muss scheitern".into(),
            ));
        };
        assert!(matches!(err, SensorError::MalformedSource));
        let message = err.to_string();
        assert!(
            !message.contains("0100007F"),
            "die Fehlermeldung darf den Zeileninhalt nicht enthalten: {message}"
        );
        Ok(())
    }

    #[test]
    fn test_parse_listener_line_non_hex_port_is_malformed() -> TestResult {
        let line = data_line("0A", "ZZZZ", 1);
        let Err(err) = parse_listener_line(&line, TCP_LISTEN_STATE) else {
            return Err(TestError::Unexpected(
                "nicht-hexadezimaler Port muss scheitern".into(),
            ));
        };
        assert!(matches!(err, SensorError::MalformedSource));
        Ok(())
    }

    // --- F-065-Nachtrag: UDP braucht `07`, nicht `0A` -----------------------

    /// Kernbeleg des Nachtrags: dieselbe Zeilenform, die für TCP als
    /// Listener zählt (`0A`), darf für eine UDP-Tabelle **nicht** zählen —
    /// UDP-Tabellen werten `07` aus (siehe Moduldokumentation).
    #[test]
    fn test_parse_listener_line_tcp_listen_state_is_not_udp_unconnected_state() -> TestResult {
        let line = data_line("0A", "0050", 1);
        assert_eq!(
            parse_listener_line(&line, UDP_UNCONNECTED_STATE).map_err(ctx("wohlgeformte Zeile"))?,
            None,
            "der TCP-Zustandscode darf für eine UDP-Tabelle nichts treffen"
        );
        Ok(())
    }

    /// Der eigentliche Regressionsbeleg: ein Zustandscode `07`, wie er in
    /// der echten Pi-Erhebung für jeden gebundenen UDP-Socket auftritt, wird
    /// als Listener erkannt, wenn er gegen [`UDP_UNCONNECTED_STATE`] geprüft
    /// wird.
    #[test]
    fn test_parse_listener_line_udp_state_07_is_recognized_as_listener() -> TestResult {
        let line = data_line("07", "A2A9", 11029);
        let record = parse_listener_line(&line, UDP_UNCONNECTED_STATE)
            .map_err(ctx("wohlgeformte Zeile"))?
            .ok_or(TestError::Missing(
                "Zustand 07 muss für UDP als Listener erkannt werden",
            ))?;
        assert_eq!(record.port, 0xA2A9);
        assert_eq!(record.inode, 11029);
        Ok(())
    }

    /// End-to-End über `collect_listeners` mit den beiden realen
    /// `/proc/net/udp`-Zeilen aus `harw-dod-fixtures/captures/rpi5-6.18/procnet.json`
    /// (Kernel `6.18.34+rpt-rpi-2712`) — der eigentliche Regressionstest für
    /// den F-065-Nachtrag: vor dieser Korrektur hätte keine der beiden
    /// Zeilen je einen Treffer erzeugt.
    #[test]
    fn test_collect_listeners_recognizes_real_udp_capture_lines() -> TestResult {
        const HEADER_UDP: &str = "   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops";
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let net_dir = dir.path().join("net");
        std::fs::create_dir_all(&net_dir).map_err(ctx("net-Verzeichnis anlegen"))?;
        // Wörtlich aus `harw-dod-fixtures/captures/rpi5-6.18/procnet.json`
        // übernommen (zwei gebundene, unverbundene UDP-Sockets, Zustand `07`).
        let content = format!(
            "{HEADER_UDP}\n\
               118: 00000000:A2A9 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 11029 2 00000000e611d66c 0\n\
               694: 00000000:14E9 00000000:0000 07 00000000:00000000 00:00000000 00000000   101        0 7036 2 0000000047b7b231 0\n"
        );
        std::fs::write(net_dir.join("udp"), content).map_err(ctx("net/udp schreiben"))?;

        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);
        let records = collect_listeners(&scope, dir.path()).map_err(ctx("poll muss gelingen"))?;

        let ports: std::collections::BTreeSet<u16> =
            records.iter().map(|record| record.port).collect();
        assert!(
            ports.contains(&0xA2A9),
            "Port aus `sl 118` muss erkannt werden"
        );
        assert!(
            ports.contains(&0x14E9),
            "Port aus `sl 694` muss erkannt werden"
        );
        Ok(())
    }

    // --- F-065: Zeilen-/Gesamtlimit statt Abbruch der gesamten Tabelle ------

    /// Kernbeleg: eine Tabelle mit weit mehr Zeilen, als F-065s ~7000
    /// Sockets brauchten, um die frühere 1-MiB-Grenze zu überschreiten,
    /// lässt `read_table` nicht scheitern — sie kappt bei [`MAX_TABLE_LINES`].
    #[test]
    fn test_read_table_caps_at_max_lines_without_erroring_f065() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let net_dir = dir.path().join("net");
        std::fs::create_dir_all(&net_dir).map_err(ctx("net-Verzeichnis anlegen"))?;
        let content = "x\n".repeat(MAX_TABLE_LINES + 10);
        std::fs::write(net_dir.join("tcp"), content).map_err(ctx("net/tcp schreiben"))?;
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);

        let lines = read_table(&scope, &net_dir.join("tcp")).map_err(ctx(
            "eine sehr lange Tabelle darf den Abruf nicht scheitern lassen",
        ))?;

        assert_eq!(
            lines.len(),
            MAX_TABLE_LINES,
            "muss bei der Zeilengrenze kappen, nicht scheitern"
        );
        Ok(())
    }

    /// Symmetrischer Beleg für die Byte-Grenze: wenige, aber sehr lange
    /// Zeilen (insgesamt deutlich über [`MAX_TABLE_BYTES`]) lassen den
    /// Abruf ebenfalls nicht scheitern.
    #[test]
    fn test_read_table_caps_at_max_bytes_without_erroring_f065() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let net_dir = dir.path().join("net");
        std::fs::create_dir_all(&net_dir).map_err(ctx("net-Verzeichnis anlegen"))?;
        let long_line = "y".repeat(4 * 1024 * 1024); // 4 MiB je Zeile
        let mut content = String::new();
        for _ in 0..5 {
            content.push_str(&long_line);
            content.push('\n');
        }
        // 5 * 4 MiB = 20 MiB, deutlich über MAX_TABLE_BYTES (16 MiB).
        std::fs::write(net_dir.join("tcp"), &content).map_err(ctx("net/tcp schreiben"))?;
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);

        let lines = read_table(&scope, &net_dir.join("tcp")).map_err(ctx(
            "eine sehr große Tabelle darf den Abruf nicht scheitern lassen",
        ))?;

        let total_bytes: usize = lines.iter().map(String::len).sum();
        assert!(
            (total_bytes as u64) <= MAX_TABLE_BYTES,
            "gelesene Gesamtbytes dürfen die Grenze nicht überschreiten: {total_bytes}"
        );
        assert!(
            lines.len() < 5,
            "mindestens eine der fünf 4-MiB-Zeilen darf nicht mehr ankommen"
        );
        Ok(())
    }

    #[test]
    fn test_read_table_missing_file_yields_empty_lines_not_an_error() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);

        let lines = read_table(&scope, &dir.path().join("net").join("tcp6"))
            .map_err(ctx("eine fehlende Tabelle darf nicht scheitern"))?;
        assert!(lines.is_empty());
        Ok(())
    }

    /// Scope-Dichtheit: `collect_listeners` liest niemals außerhalb des
    /// übergebenen `ReadScope`, selbst wenn `root` auf ein tatsächlich
    /// existierendes Verzeichnis zeigt.
    #[test]
    fn test_collect_listeners_rejects_root_outside_scope() -> TestResult {
        let real_root = tempfile::tempdir().map_err(ctx("tempdir für die echte Wurzel"))?;
        let other_root = tempfile::tempdir().map_err(ctx("tempdir außerhalb des Bereichs"))?;
        std::fs::create_dir_all(real_root.path().join("net"))
            .map_err(ctx("net-Verzeichnis anlegen"))?;
        std::fs::write(
            real_root.path().join("net").join("tcp"),
            data_line("0A", "0050", 1),
        )
        .map_err(ctx("net/tcp schreiben"))?;

        let scope = ReadScope::from_roots([other_root.path().to_path_buf()]);
        let Err(err) = collect_listeners(&scope, real_root.path()) else {
            return Err(TestError::Unexpected(
                "eine Wurzel außerhalb des Bereichs muss scheitern".into(),
            ));
        };
        assert!(matches!(err, SensorError::OutsideScope));
        Ok(())
    }
}
