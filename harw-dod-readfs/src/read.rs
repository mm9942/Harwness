//! Getypte Leserfunktionen unter einem `ReadScope`.
//!
//! # Zweck
//! Die fünf Formen, in denen ein Sensor eine Quelle tatsächlich braucht:
//! rohen Text, Zeilen, die erste Zeile (der häufigste sysfs-Fall), eine
//! geparste Ganzzahl (der zweithäufigste Fall) und reihenfolgeerhaltende
//! Schlüssel-Wert-Paare (für `/proc/meminfo` und Verwandte).
//!
//! # Verantwortungsbereich
//! Jede Funktion nimmt `&harw_dod_cap::ReadScope` und `&std::path::Path` und
//! öffnet ausschließlich über [`harw_dod_cap::ReadScope::open`] — Symlink-
//! Auflösung und Bereichsprüfung passieren dort, **nicht** hier noch einmal.
//! Das schließt Alias-Wurzeln ([`harw_dod_cap::scope::AliasRoot`]) ein: ein
//! Bereich mit `AliasRoot::sysfs_class("/sys/class/thermal")` liest
//! `/sys/class/thermal/thermal_zone0/temp`, obwohl der Eintrag ein Symlink
//! nach `/sys/devices/…` ist (Befund F-005).
//! Diese Datei ruft **niemals** `std::fs` direkt auf.
//!
//! # Exportierte Typen
//! Keine Typen, nur freie Funktionen: [`read_to_string`], [`read_lines`],
//! [`read_first_line`], [`parse_i64`], [`parse_u64`], [`read_key_values`],
//! sowie die Konstante [`MAX_READ_BYTES`].
//!
//! # Nebenläufigkeit
//! Zustandslos; `Send + Sync` ohne innere Veränderlichkeit. Jeder Aufruf
//! öffnet und schließt sein eigenes `File`; parallele Aufrufe verschiedener
//! Threads stören sich nicht.
//!
//! # Fehler
//! [`crate::error::ReadFsError`] — inhaltsfrei, siehe dessen
//! Modul-Dokumentation.
//!
//! # Examples
//! ```rust,no_run
//! use std::path::Path;
//! use harw_dod_cap::ReadScope;
//!
//! let scope = ReadScope::from_roots([Path::new("/sys/class/thermal").to_path_buf()]);
//! let millidegrees = harw_dod_readfs::parse_i64(
//!     &scope,
//!     Path::new("/sys/class/thermal/thermal_zone0/temp"),
//! )?;
//! # Ok::<(), harw_dod_readfs::ReadFsError>(())
//! ```

use std::io::Read as _;
use std::path::Path;

use harw_dod_cap::{ReadScope, SensorError};

use crate::error::{ReadFsError, ReadFsResult};

/// Obergrenze für [`read_to_string`] (und damit für alle Funktionen dieses
/// Moduls, die darauf aufbauen), in Bytes: **1 MiB (`2^20` = 1 048 576
/// Bytes)**.
///
/// # Begründung
/// Eine sysfs-Datei trägt üblicherweise ein paar Bytes —
/// `/sys/class/thermal/thermal_zone0/temp` etwa sechs. Die größten
/// legitimen procfs-Quellen, für die diese Crate gebaut ist —
/// `/proc/meminfo` (rund 2 KiB), `/proc/net/dev` (einige KiB je
/// Netzwerkschnittstelle) und `/proc/stat` (auf einer Maschine mit
/// mehreren hundert CPU-Kernen noch im niedrigen zweistelligen
/// KiB-Bereich) — bleiben durchweg unter 100 KiB. 1 MiB liegt damit gut
/// das Zehn- bis Hundertfache über jeder erwarteten Quelle: großzügig
/// genug, um keine legitime Datei abzuschneiden, aber klein genug, dass ein
/// Sensor, der versehentlich auf eine beliebig große Datei zeigt (ein Log,
/// ein Gerätenode, statt eines sysfs-Attributs), mit **einer** begrenzten
/// Allokation scheitert statt unbegrenzten Speicher zu verbrauchen.
pub const MAX_READ_BYTES: u64 = 1 << 20;

/// Liest eine Datei vollständig als UTF-8-Zeichenkette, begrenzt durch
/// [`MAX_READ_BYTES`].
///
/// # Description
/// Öffnet `path` über [`ReadScope::open`]; liest höchstens
/// `MAX_READ_BYTES + 1` Bytes. Kommt mehr als `MAX_READ_BYTES` zurück,
/// bricht die Funktion mit [`ReadFsError::TooLarge`] ab, **ohne** die
/// tatsächliche Dateigröße zu ermitteln — das würde die überlange Datei ja
/// gerade vollständig einlesen.
///
/// # Arguments
/// - `scope` (`&ReadScope`): der Lesebereich, gegen den `path` geprüft wird.
/// - `path` (`&Path`): der zu lesende Pfad, unverändert wie übergeben.
///
/// # Returns
/// Der Dateiinhalt als `String`.
///
/// # Errors
/// - [`ReadFsError::Scope`]: `path` liegt außerhalb des Bereichs, die Quelle
///   ist nicht verfügbar, oder ein E/A-Fehler trat auf (auch: der Inhalt ist
///   kein gültiges UTF-8 — dann [`SensorError::MalformedSource`]).
/// - [`ReadFsError::TooLarge`]: der Inhalt überschreitet [`MAX_READ_BYTES`].
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_dod_cap::ReadScope;
///
/// let scope = ReadScope::from_roots([Path::new("/proc").to_path_buf()]);
/// let content = harw_dod_readfs::read_to_string(&scope, Path::new("/proc/meminfo"))?;
/// # Ok::<(), harw_dod_readfs::ReadFsError>(())
/// ```
pub fn read_to_string(scope: &ReadScope, path: &Path) -> ReadFsResult<String> {
    let mut file = scope.open(path)?;
    let mut buf = Vec::new();
    (&mut file)
        .take(MAX_READ_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(SensorError::from)?;

    if buf.len() as u64 > MAX_READ_BYTES {
        return Err(ReadFsError::TooLarge {
            limit: MAX_READ_BYTES,
        });
    }

    String::from_utf8(buf).map_err(|_| ReadFsError::from(SensorError::MalformedSource))
}

/// Liest eine Datei zeilenweise ein.
///
/// # Description
/// Baut auf [`read_to_string`] auf und teilt am Zeilenumbruch (ohne ihn im
/// Ergebnis zu behalten); unterliegt derselben Größengrenze
/// [`MAX_READ_BYTES`].
///
/// # Arguments
/// - `scope` (`&ReadScope`): der Lesebereich.
/// - `path` (`&Path`): der zu lesende Pfad.
///
/// # Returns
/// Die Zeilen der Datei, ohne Zeilenumbrüche, in Dateireihenfolge.
///
/// # Errors
/// Siehe [`read_to_string`].
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_dod_cap::ReadScope;
///
/// let scope = ReadScope::from_roots([Path::new("/proc").to_path_buf()]);
/// let lines = harw_dod_readfs::read_lines(&scope, Path::new("/proc/meminfo"))?;
/// # Ok::<(), harw_dod_readfs::ReadFsError>(())
/// ```
pub fn read_lines(scope: &ReadScope, path: &Path) -> ReadFsResult<Vec<String>> {
    let content = read_to_string(scope, path)?;
    Ok(content.lines().map(str::to_owned).collect())
}

/// Liest die erste Zeile einer Datei — der häufigste sysfs-Fall (z. B.
/// `/sys/class/thermal/thermal_zone0/temp`: eine Zahl gefolgt von einem
/// Zeilenumbruch).
///
/// # Description
/// Baut auf [`read_to_string`] auf. Eine leere Datei gilt als fehlerhafte
/// Quelle ([`SensorError::MalformedSource`]), nicht als leere Zeile — eine
/// sysfs-Datei, die nichts liefert, ist kein gültiger Messwert.
///
/// # Arguments
/// - `scope` (`&ReadScope`): der Lesebereich.
/// - `path` (`&Path`): der zu lesende Pfad.
///
/// # Returns
/// Die erste Zeile ohne Zeilenumbruch.
///
/// # Errors
/// Siehe [`read_to_string`]; zusätzlich [`ReadFsError::Scope`] mit
/// [`SensorError::MalformedSource`], wenn die Datei leer ist.
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_dod_cap::ReadScope;
///
/// let scope = ReadScope::from_roots([Path::new("/sys/class/thermal").to_path_buf()]);
/// let line = harw_dod_readfs::read_first_line(
///     &scope,
///     Path::new("/sys/class/thermal/thermal_zone0/temp"),
/// )?;
/// # Ok::<(), harw_dod_readfs::ReadFsError>(())
/// ```
pub fn read_first_line(scope: &ReadScope, path: &Path) -> ReadFsResult<String> {
    let content = read_to_string(scope, path)?;
    content
        .lines()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| ReadFsError::from(SensorError::MalformedSource))
}

/// Liest die erste Zeile und parst sie als `i64` — der zweithäufigste
/// sysfs-Fall nach reinem Zeilentext.
///
/// # Description
/// Baut auf [`read_first_line`] auf; führendes und nachgestelltes
/// Leerzeichen (inklusive des Zeilenumbruchs) wird vor dem Parsen entfernt.
///
/// # Arguments
/// - `scope` (`&ReadScope`): der Lesebereich.
/// - `path` (`&Path`): der zu lesende Pfad.
///
/// # Returns
/// Der geparste Wert.
///
/// # Errors
/// Siehe [`read_first_line`]; zusätzlich [`ReadFsError::Scope`] mit
/// [`SensorError::MalformedSource`], wenn der Inhalt keine gültige
/// `i64`-Zahl ist. **Der nicht parsbare Inhalt erscheint nicht in der
/// Fehlermeldung.**
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_dod_cap::ReadScope;
///
/// let scope = ReadScope::from_roots([Path::new("/sys/class/thermal").to_path_buf()]);
/// let millidegrees = harw_dod_readfs::parse_i64(
///     &scope,
///     Path::new("/sys/class/thermal/thermal_zone0/temp"),
/// )?;
/// # Ok::<(), harw_dod_readfs::ReadFsError>(())
/// ```
pub fn parse_i64(scope: &ReadScope, path: &Path) -> ReadFsResult<i64> {
    let line = read_first_line(scope, path)?;
    line.trim()
        .parse::<i64>()
        .map_err(|_| ReadFsError::from(SensorError::MalformedSource))
}

/// Wie [`parse_i64`], aber für `u64` — für Zähler, die nie negativ werden
/// (z. B. `/sys/class/net/*/statistics/rx_bytes`).
///
/// # Description
/// Baut auf [`read_first_line`] auf; führendes und nachgestelltes
/// Leerzeichen (inklusive des Zeilenumbruchs) wird vor dem Parsen entfernt.
///
/// # Arguments
/// - `scope` (`&ReadScope`): der Lesebereich.
/// - `path` (`&Path`): der zu lesende Pfad.
///
/// # Returns
/// Der geparste Wert.
///
/// # Errors
/// Siehe [`parse_i64`].
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_dod_cap::ReadScope;
///
/// let scope =
///     ReadScope::from_roots([Path::new("/sys/class/net/eth0/statistics").to_path_buf()]);
/// let rx_bytes = harw_dod_readfs::parse_u64(
///     &scope,
///     Path::new("/sys/class/net/eth0/statistics/rx_bytes"),
/// )?;
/// # Ok::<(), harw_dod_readfs::ReadFsError>(())
/// ```
pub fn parse_u64(scope: &ReadScope, path: &Path) -> ReadFsResult<u64> {
    let line = read_first_line(scope, path)?;
    line.trim()
        .parse::<u64>()
        .map_err(|_| ReadFsError::from(SensorError::MalformedSource))
}

/// Liest schlüsselwertartige Zeilen ein, reihenfolgeerhaltend — der Fall für
/// `/proc/meminfo` und verwandte procfs-Quellen.
///
/// # Description
/// Jede Zeile wird am **ersten** Vorkommen von `separator` geteilt;
/// Schlüssel und Wert werden je getrimmt. Zeilen ohne `separator` (z. B.
/// eine abschließende Leerzeile) werden übersprungen, nicht als Fehler
/// gewertet. Das Ergebnis ist ein `Vec`, keine `HashMap`: die Reihenfolge
/// ist bei manchen procfs-Dateien bedeutungstragend, und ein Sensor, der sie
/// braucht, soll sie nicht verloren haben.
///
/// # Arguments
/// - `scope` (`&ReadScope`): der Lesebereich.
/// - `path` (`&Path`): der zu lesende Pfad.
/// - `separator` (`char`): das Trennzeichen zwischen Schlüssel und Wert
///   (z. B. `':'` für `/proc/meminfo`).
///
/// # Returns
/// Die `(Schlüssel, Wert)`-Paare in Dateireihenfolge.
///
/// # Errors
/// Siehe [`read_to_string`].
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_dod_cap::ReadScope;
///
/// let scope = ReadScope::from_roots([Path::new("/proc").to_path_buf()]);
/// let pairs = harw_dod_readfs::read_key_values(&scope, Path::new("/proc/meminfo"), ':')?;
/// # Ok::<(), harw_dod_readfs::ReadFsError>(())
/// ```
pub fn read_key_values(
    scope: &ReadScope,
    path: &Path,
    separator: char,
) -> ReadFsResult<Vec<(String, String)>> {
    let content = read_to_string(scope, path)?;
    Ok(content
        .lines()
        .filter_map(|line| line.split_once(separator))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect())
}

/// Liest eine Datei und zerlegt jede Zeile an Leerraum in Felder.
///
/// # Description
/// Der zweithäufigste procfs-Fall nach [`read_key_values`], und der, den das
/// Sensor-Ableitungsmakro **nicht** abdeckt: eine Datei mit vielen Werten in
/// Spalten. `/proc/stat`, `/proc/net/dev` und `/proc/diskstats` sind alle von
/// dieser Form:
///
/// ```text
/// cpu  2255 34 2290 22625563 6290 127 456 0 0 0
/// cpu0 1132 34 1441 11311718 3675 127 438 0 0 0
/// ```
///
/// **Die Zeilenreihenfolge bleibt erhalten**, und die Feldzahl je Zeile wird
/// **nicht** geprüft. Beides ist Absicht: neuere Kernel hängen an solche
/// Zeilen zusätzliche Spalten an, und ein Leser, der auf eine feste Feldzahl
/// besteht, scheitert nach einem Kernel-Update an einer Datei, die völlig in
/// Ordnung ist. Die Prüfung, ob genug Spalten da sind, gehört zum Aufrufer,
/// der weiß, welche er braucht.
///
/// Leere Zeilen werden verworfen — sie tragen keine Felder und würden dem
/// Aufrufer nur einen leeren Vektor zum Aussortieren geben.
///
/// # Arguments
/// - `scope` (`&ReadScope`): der Lesebereich. Der Zugriff läuft über
///   [`read_to_string`] und damit über `ReadScope::open`.
/// - `path` (`&Path`): die zu lesende Datei.
///
/// # Returns
/// Je Zeile ein Vektor ihrer Felder, in Datei- und Spaltenreihenfolge.
///
/// # Errors
/// Siehe [`read_to_string`] — insbesondere [`ReadFsError`] für einen Pfad
/// außerhalb des Bereichs und für eine Datei über der Größengrenze.
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_dod_cap::ReadScope;
///
/// let scope = ReadScope::from_roots([Path::new("/proc").to_path_buf()]);
/// let rows = harw_dod_readfs::read_line_fields(&scope, Path::new("/proc/stat"))?;
/// // Die Aggregatzeile steht zuerst; ihr zweites Feld ist die `user`-Zeit.
/// let user = rows.first().and_then(|row| row.get(1));
/// # Ok::<(), harw_dod_readfs::ReadFsError>(())
/// ```
pub fn read_line_fields(scope: &ReadScope, path: &Path) -> ReadFsResult<Vec<Vec<String>>> {
    let content = read_to_string(scope, path)?;
    Ok(content
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(std::borrow::ToOwned::to_owned)
                .collect::<Vec<String>>()
        })
        .filter(|fields| !fields.is_empty())
        .collect())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use harw_dod_cap::{ReadScope, SensorError};
    use tempfile::tempdir;

    use super::*;

    /// Baut einen Bereich, der genau `dir` umfasst.
    fn scope_for(dir: &Path) -> ReadScope {
        ReadScope::from_roots([dir.to_path_buf()])
    }

    #[test]
    fn test_read_line_fields_splits_columns_and_keeps_line_order() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("stat");
        fs::write(&file, "cpu  2255 34 2290\ncpu0 1132 34 1441\n").expect("write");
        let scope = scope_for(dir.path());

        let rows = read_line_fields(&scope, &file).expect("read_line_fields muss gelingen");

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], "cpu");
        // Der Wert aus der Mitte, nicht der erste: nur so prüft der Test die
        // Spaltenzuordnung wirklich.
        assert_eq!(rows[0][2], "34");
        assert_eq!(rows[1][0], "cpu0");
    }

    #[test]
    fn test_read_line_fields_accepts_extra_columns() {
        // Neuere Kernel hängen Spalten an. Ein Leser, der auf eine feste
        // Feldzahl besteht, scheitert nach einem Update an einer Datei, die
        // völlig in Ordnung ist.
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("stat");
        fs::write(&file, "cpu 1 2 3 4 5 6 7 8 9 10 11 12\n").expect("write");
        let scope = scope_for(dir.path());

        let rows = read_line_fields(&scope, &file).expect("read_line_fields muss gelingen");

        assert_eq!(rows[0].len(), 13);
    }

    #[test]
    fn test_read_line_fields_drops_empty_lines() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("stat");
        fs::write(&file, "a 1\n\n   \nb 2\n").expect("write");
        let scope = scope_for(dir.path());

        let rows = read_line_fields(&scope, &file).expect("read_line_fields muss gelingen");

        assert_eq!(rows.len(), 2, "Leerzeilen tragen keine Felder");
    }

    #[test]
    fn test_read_line_fields_outside_scope_rejected() {
        let inside = tempdir().expect("tempdir");
        let outside = tempdir().expect("tempdir");
        let file = outside.path().join("stat");
        fs::write(&file, "cpu 1\n").expect("write");
        let scope = scope_for(inside.path());

        assert!(matches!(
            read_line_fields(&scope, &file),
            Err(ReadFsError::Scope(SensorError::OutsideScope))
        ));
    }

    #[test]
    fn test_read_to_string_success() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("value");
        fs::write(&file, "hallo\n").expect("write");
        let scope = scope_for(dir.path());

        let content = read_to_string(&scope, &file).expect("read_to_string muss gelingen");
        assert_eq!(content, "hallo\n");
    }

    #[test]
    fn test_read_to_string_outside_scope_rejected() {
        let inside = tempdir().expect("tempdir");
        let outside = tempdir().expect("tempdir");
        let file = outside.path().join("value");
        fs::write(&file, "hallo\n").expect("write");
        let scope = scope_for(inside.path());

        let err =
            read_to_string(&scope, &file).expect_err("außerhalb des Bereichs muss scheitern");
        assert!(matches!(err, ReadFsError::Scope(SensorError::OutsideScope)));
    }

    #[test]
    fn test_read_to_string_missing_file_rejected() {
        let dir = tempdir().expect("tempdir");
        let missing = dir.path().join("fehlt");
        let scope = scope_for(dir.path());

        let err = read_to_string(&scope, &missing).expect_err("fehlende Datei muss scheitern");
        assert!(matches!(err, ReadFsError::Scope(_)));
    }

    #[test]
    fn test_read_to_string_over_limit_rejected() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("gross");
        let oversized = vec![b'x'; (MAX_READ_BYTES + 1) as usize];
        fs::write(&file, &oversized).expect("write");
        let scope = scope_for(dir.path());

        let err =
            read_to_string(&scope, &file).expect_err("Datei über der Grenze muss scheitern");
        assert!(matches!(
            err,
            ReadFsError::TooLarge { limit } if limit == MAX_READ_BYTES
        ));
    }

    #[test]
    fn test_read_lines_success() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("lines");
        fs::write(&file, "a\nb\nc\n").expect("write");
        let scope = scope_for(dir.path());

        let lines = read_lines(&scope, &file).expect("read_lines muss gelingen");
        assert_eq!(lines, vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]);
    }

    #[test]
    fn test_read_lines_outside_scope_rejected() {
        let inside = tempdir().expect("tempdir");
        let outside = tempdir().expect("tempdir");
        let file = outside.path().join("lines");
        fs::write(&file, "a\n").expect("write");
        let scope = scope_for(inside.path());

        let err = read_lines(&scope, &file).expect_err("außerhalb des Bereichs muss scheitern");
        assert!(matches!(err, ReadFsError::Scope(SensorError::OutsideScope)));
    }

    #[test]
    fn test_read_lines_missing_file_rejected() {
        let dir = tempdir().expect("tempdir");
        let missing = dir.path().join("fehlt");
        let scope = scope_for(dir.path());

        let err = read_lines(&scope, &missing).expect_err("fehlende Datei muss scheitern");
        assert!(matches!(err, ReadFsError::Scope(_)));
    }

    #[test]
    fn test_read_first_line_success() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("temp");
        fs::write(&file, "45000\n").expect("write");
        let scope = scope_for(dir.path());

        let line = read_first_line(&scope, &file).expect("read_first_line muss gelingen");
        assert_eq!(line, "45000");
    }

    #[test]
    fn test_read_first_line_outside_scope_rejected() {
        let inside = tempdir().expect("tempdir");
        let outside = tempdir().expect("tempdir");
        let file = outside.path().join("temp");
        fs::write(&file, "45000\n").expect("write");
        let scope = scope_for(inside.path());

        let err =
            read_first_line(&scope, &file).expect_err("außerhalb des Bereichs muss scheitern");
        assert!(matches!(err, ReadFsError::Scope(SensorError::OutsideScope)));
    }

    #[test]
    fn test_read_first_line_missing_file_rejected() {
        let dir = tempdir().expect("tempdir");
        let missing = dir.path().join("fehlt");
        let scope = scope_for(dir.path());

        let err = read_first_line(&scope, &missing).expect_err("fehlende Datei muss scheitern");
        assert!(matches!(err, ReadFsError::Scope(_)));
    }

    #[test]
    fn test_read_first_line_empty_file_is_malformed() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("leer");
        fs::write(&file, "").expect("write");
        let scope = scope_for(dir.path());

        let err = read_first_line(&scope, &file).expect_err("leere Datei muss scheitern");
        assert!(matches!(
            err,
            ReadFsError::Scope(SensorError::MalformedSource)
        ));
    }

    #[test]
    fn test_parse_i64_valid_content() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("temp");
        fs::write(&file, "42\n").expect("write");
        let scope = scope_for(dir.path());

        let value = parse_i64(&scope, &file).expect("parse_i64 muss gelingen");
        assert_eq!(value, 42);
    }

    #[test]
    fn test_parse_i64_outside_scope_rejected() {
        let inside = tempdir().expect("tempdir");
        let outside = tempdir().expect("tempdir");
        let file = outside.path().join("temp");
        fs::write(&file, "42\n").expect("write");
        let scope = scope_for(inside.path());

        let err = parse_i64(&scope, &file).expect_err("außerhalb des Bereichs muss scheitern");
        assert!(matches!(err, ReadFsError::Scope(SensorError::OutsideScope)));
    }

    #[test]
    fn test_parse_i64_missing_file_rejected() {
        let dir = tempdir().expect("tempdir");
        let missing = dir.path().join("fehlt");
        let scope = scope_for(dir.path());

        let err = parse_i64(&scope, &missing).expect_err("fehlende Datei muss scheitern");
        assert!(matches!(err, ReadFsError::Scope(_)));
    }

    /// Die Zusage der Crate: nicht parsbarer Inhalt löst `MalformedSource`
    /// aus, und der Inhalt selbst erscheint nirgends in der Meldung.
    #[test]
    fn test_parse_i64_malformed_content_does_not_leak_content_in_message() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("temp");
        fs::write(&file, "nicht-zahl\n").expect("write");
        let scope = scope_for(dir.path());

        let err = parse_i64(&scope, &file).expect_err("nicht-numerischer Inhalt muss scheitern");
        assert!(matches!(
            err,
            ReadFsError::Scope(SensorError::MalformedSource)
        ));
        let message = err.to_string();
        assert!(
            !message.contains("nicht-zahl"),
            "die Fehlermeldung darf den gelesenen Inhalt nicht enthalten: {message}"
        );
    }

    #[test]
    fn test_parse_u64_valid_content() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("rx_bytes");
        fs::write(&file, "18446744073709551615\n").expect("write");
        let scope = scope_for(dir.path());

        let value = parse_u64(&scope, &file).expect("parse_u64 muss gelingen");
        assert_eq!(value, u64::MAX);
    }

    #[test]
    fn test_parse_u64_outside_scope_rejected() {
        let inside = tempdir().expect("tempdir");
        let outside = tempdir().expect("tempdir");
        let file = outside.path().join("rx_bytes");
        fs::write(&file, "1\n").expect("write");
        let scope = scope_for(inside.path());

        let err = parse_u64(&scope, &file).expect_err("außerhalb des Bereichs muss scheitern");
        assert!(matches!(err, ReadFsError::Scope(SensorError::OutsideScope)));
    }

    #[test]
    fn test_parse_u64_missing_file_rejected() {
        let dir = tempdir().expect("tempdir");
        let missing = dir.path().join("fehlt");
        let scope = scope_for(dir.path());

        let err = parse_u64(&scope, &missing).expect_err("fehlende Datei muss scheitern");
        assert!(matches!(err, ReadFsError::Scope(_)));
    }

    #[test]
    fn test_parse_u64_malformed_content_does_not_leak_content_in_message() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("rx_bytes");
        fs::write(&file, "nicht-zahl\n").expect("write");
        let scope = scope_for(dir.path());

        let err = parse_u64(&scope, &file).expect_err("nicht-numerischer Inhalt muss scheitern");
        let message = err.to_string();
        assert!(
            !message.contains("nicht-zahl"),
            "die Fehlermeldung darf den gelesenen Inhalt nicht enthalten: {message}"
        );
    }

    #[test]
    fn test_read_key_values_preserves_order() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("meminfo");
        fs::write(&file, "MemTotal:       16384 kB\nMemFree:        2048 kB\n")
            .expect("write");
        let scope = scope_for(dir.path());

        let pairs = read_key_values(&scope, &file, ':').expect("read_key_values muss gelingen");
        assert_eq!(
            pairs,
            vec![
                ("MemTotal".to_owned(), "16384 kB".to_owned()),
                ("MemFree".to_owned(), "2048 kB".to_owned()),
            ]
        );
    }

    #[test]
    fn test_read_key_values_outside_scope_rejected() {
        let inside = tempdir().expect("tempdir");
        let outside = tempdir().expect("tempdir");
        let file = outside.path().join("meminfo");
        fs::write(&file, "MemTotal: 1 kB\n").expect("write");
        let scope = scope_for(inside.path());

        let err = read_key_values(&scope, &file, ':')
            .expect_err("außerhalb des Bereichs muss scheitern");
        assert!(matches!(err, ReadFsError::Scope(SensorError::OutsideScope)));
    }

    #[test]
    fn test_read_key_values_missing_file_rejected() {
        let dir = tempdir().expect("tempdir");
        let missing = dir.path().join("fehlt");
        let scope = scope_for(dir.path());

        let err =
            read_key_values(&scope, &missing, ':').expect_err("fehlende Datei muss scheitern");
        assert!(matches!(err, ReadFsError::Scope(_)));
    }

    #[test]
    fn test_read_key_values_skips_lines_without_separator() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("mixed");
        fs::write(&file, "a: 1\n\nb: 2\n").expect("write");
        let scope = scope_for(dir.path());

        let pairs = read_key_values(&scope, &file, ':').expect("read_key_values muss gelingen");
        assert_eq!(
            pairs,
            vec![
                ("a".to_owned(), "1".to_owned()),
                ("b".to_owned(), "2".to_owned()),
            ]
        );
    }

    /// Nachgebauter sysfs-Ausschnitt mit echtem Symlink
    /// `sys/class/thermal/thermal_zone0 -> ../../devices/virtual/thermal/thermal_zone0`
    /// (strukturgleich zum RPi 5) plus einem Alias-Bereich darauf.
    #[cfg(unix)]
    fn alias_thermal_tree(base: &Path) -> (ReadScope, std::path::PathBuf) {
        use harw_dod_cap::scope::AliasRoot;
        use std::os::unix::fs::symlink;

        let zone = base.join("sys/devices/virtual/thermal/thermal_zone0");
        fs::create_dir_all(&zone).expect("create zone");
        fs::write(zone.join("temp"), "48150\n").expect("write temp");
        let class = base.join("sys/class/thermal");
        fs::create_dir_all(&class).expect("create class");
        symlink(
            "../../devices/virtual/thermal/thermal_zone0",
            class.join("thermal_zone0"),
        )
        .expect("class symlink");

        let alias = AliasRoot::new(class.clone(), base.join("sys/devices")).expect("alias root");
        let scope = ReadScope::from_roots_and_aliases(Vec::<std::path::PathBuf>::new(), [alias]);
        (scope, class)
    }

    #[cfg(unix)]
    #[test]
    fn test_parse_i64_through_alias_root_reads_sysfs_class_symlink() {
        let dir = tempdir().expect("tempdir");
        let base = dir.path().canonicalize().expect("canonical tempdir");
        let (scope, class) = alias_thermal_tree(&base);

        let value = parse_i64(&scope, &class.join("thermal_zone0/temp"))
            .expect("Alias-Wurzel muss den sysfs-Klassen-Symlink lesen");
        assert_eq!(value, 48150);
    }

    #[cfg(unix)]
    #[test]
    fn test_parse_i64_plain_class_root_rejects_sysfs_symlink() {
        let dir = tempdir().expect("tempdir");
        let base = dir.path().canonicalize().expect("canonical tempdir");
        let (_, class) = alias_thermal_tree(&base);
        let plain = scope_for(&class);

        let err = parse_i64(&plain, &class.join("thermal_zone0/temp"))
            .expect_err("ohne Alias-Wurzel bleibt das Ziel außerhalb");
        assert!(matches!(err, ReadFsError::Scope(SensorError::OutsideScope)));
    }

    #[cfg(unix)]
    #[test]
    fn test_read_to_string_alias_rejects_parent_dir_escape() {
        let dir = tempdir().expect("tempdir");
        let base = dir.path().canonicalize().expect("canonical tempdir");
        let (scope, class) = alias_thermal_tree(&base);
        fs::write(base.join("secret"), "geheim\n").expect("write secret");

        let err = read_to_string(&scope, &class.join("thermal_zone0/../../../../secret"))
            .expect_err("'..' muss abgelehnt werden");
        assert!(matches!(err, ReadFsError::Scope(SensorError::OutsideScope)));
    }
}
