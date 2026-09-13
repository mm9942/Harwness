//! Lesebereich für Sensoren: ein Halbverband, der nur schrumpfen kann.
//!
//! # Verantwortungsbereich
//! [`ReadScope`] modelliert, welche Wurzelverzeichnisse ein Sensor lesen
//! darf. Wie `harw_sandbox::NetworkScope` (das Vorbild dieser Disziplin) gibt
//! es **kein** `add` und **keine** Vereinigung — nur [`ReadScope::intersection`].
//! Ein Bereich kann auf keinem Weg wachsen; das ist eine Typ-Eigenschaft
//! dieser API, keine Konvention, die ein Aufrufer versehentlich verletzen
//! könnte.
//!
//! # Sicherheitskritischer Pfad
//! [`ReadScope::open`] löst Symlinks **vor** der Bereichsprüfung auf, über
//! `std::path::Path::canonicalize`. Wer zuerst prüft und danach öffnet,
//! prüft nur den *Namen* — das *Ziel* eines Symlinks bleibt bis zum
//! `open()`-Syscall ungeprüft, und zwischen Prüfung und Öffnen kann sich das
//! Ziel sogar noch ändern (TOCTOU: time-of-check/time-of-use). Deshalb ist
//! die Reihenfolge hier kein Stilmittel, sondern die ganze Pointe der
//! Methode: erst auflösen, dann prüfen, dann öffnen — alle drei Schritte auf
//! demselben, bereits aufgelösten Pfad.
//!
//! # Exportierte Typen
//! [`ReadScope`].
//!
//! # Nebenläufigkeit
//! Reine, unveränderliche Daten (`BTreeSet<PathBuf>`) ohne innere
//! Veränderlichkeit: `Send + Sync`, beliebig teilbar. [`ReadScope::open`]
//! führt Betriebssystem-I/O aus (Canonicalize + Open); Fehler werden als
//! [`crate::error::SensorError`] zurückgegeben, nie als Panic.
//!
//! # Fehler
//! [`crate::error::SensorError::OutsideScope`] (Bereichsverletzung; nennt nie
//! das Ziel) und [`crate::error::SensorError::Io`] (Canonicalize-/Open-Fehler
//! des Betriebssystems).
//!
//! # Examples
//! ```rust
//! use harw_dod_cap::ReadScope;
//! use std::path::{Path, PathBuf};
//!
//! let scope = ReadScope::from_roots([PathBuf::from("/proc")]);
//! assert!(scope.allows(Path::new("/proc/stat")));
//! assert!(!scope.allows(Path::new("/etc/passwd")));
//! ```

use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

use crate::error::SensorError;

/// Ein Lesebereich: eine Menge erlaubter Wurzelverzeichnisse.
///
/// # Description
/// Halbverband wie `NetworkScope`: Es gibt [`Self::intersection`] (Meet),
/// aber bewusst kein `add` und keine Vereinigung (Join). Ein einmal gebauter
/// Bereich kann also nur enger werden, nie weiter — jede Kette von
/// Operationen auf einem `ReadScope` konvergiert höchstens gegen den leeren
/// Bereich, nie gegen einen größeren.
///
/// Die Wurzeln werden so gespeichert, wie sie übergeben wurden — [`Self::from_roots`]
/// führt keine Kanonisierung durch, weil Kanonisierung eine fehlbare
/// Dateisystemoperation ist und dieser Konstruktor unfehlbar bleibt. Wurzeln
/// sollten deshalb bereits kanonische, symlink-freie Verzeichnisse sein; die
/// eigentliche Sicherheitsprüfung gegen Symlinks im *Ziel*-Pfad übernimmt
/// ohnehin [`Self::open`], nicht die Konstruktion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadScope {
    roots: BTreeSet<PathBuf>,
}

impl ReadScope {
    /// Baut einen Bereich aus Wurzelpfaden.
    ///
    /// # Arguments
    /// - `roots` (`impl IntoIterator<Item = PathBuf>`): erlaubte
    ///   Wurzelverzeichnisse. Duplikate fallen durch die Mengensemantik weg;
    ///   eine leere Eingabe liefert den leeren Bereich, der keinen einzigen
    ///   Pfad erlaubt.
    ///
    /// # Returns
    /// Einen `ReadScope`, dessen [`Self::allows`] genau die Pfade akzeptiert,
    /// die unterhalb einer der übergebenen Wurzeln liegen.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::ReadScope;
    /// use std::path::PathBuf;
    ///
    /// let scope = ReadScope::from_roots([PathBuf::from("/sys/class/thermal")]);
    /// assert!(scope.allows(std::path::Path::new("/sys/class/thermal/thermal_zone0")));
    /// ```
    #[must_use]
    pub fn from_roots(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            roots: roots.into_iter().collect(),
        }
    }

    /// Schnitt zweier Bereiche. Es gibt bewusst keine Vereinigung.
    ///
    /// # Description
    /// Exakte Mengenoperation über die gespeicherten Wurzeln, keine
    /// Neuberechnung überlappender Verzeichnisse — analog zu
    /// `NetworkScope::intersection`, das ebenfalls eine exakte
    /// Mengenoperation über normalisierte Einträge ist, keine
    /// Suffix-Auswertung. Zwei Bereiche mit unterschiedlichen, aber
    /// ineinander verschachtelten Wurzeln (z. B. `/a/b` und `/a`) erzeugen
    /// deshalb bewusst einen konservativeren Schnitt als die theoretisch
    /// größtmögliche gemeinsame Menge: ein Verhalten, das in einem
    /// Berechtigungsmodell die sichere Richtung ist.
    ///
    /// # Arguments
    /// - `other` (`&ReadScope`): der zweite Bereich.
    ///
    /// # Returns
    /// Einen `ReadScope`, dessen erlaubte Pfadmenge Teilmenge sowohl von
    /// `self` als auch von `other` ist. Kommutativ und idempotent.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::ReadScope;
    /// use std::path::PathBuf;
    ///
    /// let a = ReadScope::from_roots([PathBuf::from("/proc"), PathBuf::from("/sys")]);
    /// let b = ReadScope::from_roots([PathBuf::from("/sys"), PathBuf::from("/etc")]);
    /// let ab = a.intersection(&b);
    ///
    /// assert!(ab.allows(std::path::Path::new("/sys/class/thermal")));
    /// assert!(!ab.allows(std::path::Path::new("/proc/stat")));
    /// ```
    /// Die Wurzeln dieses Bereichs.
    ///
    /// # Description
    /// Ein lesender Zugriff, damit ein Aufrufer, der das Dateisystem
    /// **durchlaufen** muss, weiß, wo er anfangen darf. `allows` beantwortet
    /// „liegt dieser Pfad drin", nicht „welche Pfade gibt es" — und ein
    /// Verzeichnisdurchlauf, der bei `/` beginnt und erst am Ende prüft,
    /// liest unterwegs Verzeichnisse, die ihn nichts angehen.
    ///
    /// Der Rückgabewert ist geliehen und unveränderlich: ein Bereich kann
    /// auch über diesen Weg nicht wachsen.
    ///
    /// # Returns
    /// Ein Iterator über die Wurzelpfade, in stabiler Ordnung.
    pub fn roots(&self) -> impl Iterator<Item = &Path> {
        self.roots.iter().map(PathBuf::as_path)
    }

    pub fn intersection(&self, other: &Self) -> Self {
        Self {
            roots: self.roots.intersection(&other.roots).cloned().collect(),
        }
    }

    /// Liegt `path` im Bereich?
    ///
    /// # Description
    /// Erlaubt ist ein exakter Treffer auf einer Wurzel oder ein Pfad
    /// *unterhalb* einer Wurzel, wobei `Path::starts_with` ausschließlich
    /// vollständige Pfadkomponenten vergleicht: `/srv/data` erlaubt
    /// `/srv/data/x`, aber **nicht** `/srv/database`, weil die Komponente
    /// `data` und die Komponente `database` unterschiedliche Zeichenketten
    /// sind, selbst wenn `data` ein Byte-Präfix von `database` ist. Ohne
    /// diese Verzeichnisgrenze könnte jede Wurzel durch ein bloß ähnlich
    /// benanntes Geschwisterverzeichnis mit-freigegeben werden — derselbe
    /// Fehlertyp wie eine Domain-Prüfung ohne Punktgrenze.
    ///
    /// Diese Methode führt keine Dateisystem-I/O aus und löst keine Symlinks
    /// auf; sie ist eine reine, syntaktische Prüfung gegen die gespeicherten
    /// Wurzeln. Die sicherheitskritische, symlink-sichere Prüfung eines
    /// tatsächlichen Zugriffs übernimmt [`Self::open`].
    ///
    /// # Arguments
    /// - `path` (`&std::path::Path`): der zu prüfende Pfad.
    ///
    /// # Returns
    /// `true`, wenn `path` unter einer der gespeicherten Wurzeln liegt.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::ReadScope;
    /// use std::path::{Path, PathBuf};
    ///
    /// let scope = ReadScope::from_roots([PathBuf::from("/srv/data")]);
    /// assert!(scope.allows(Path::new("/srv/data/report.json")));
    /// assert!(!scope.allows(Path::new("/srv/database/report.json")));
    /// ```
    #[must_use]
    pub fn allows(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| path.starts_with(root))
    }

    /// Öffnet `path` **nach** Auflösung aller Symlinks.
    ///
    /// # Description
    /// Die Reihenfolge ist der ganze Punkt: erst auflösen
    /// (`Path::canonicalize`), dann prüfen ([`Self::allows`] auf dem
    /// aufgelösten Pfad), dann öffnen — und zwar denselben aufgelösten Pfad,
    /// der auch geprüft wurde. Wer zuerst den unaufgelösten Namen prüft und
    /// erst danach öffnet, prüft nur die Fassade: ein Symlink innerhalb des
    /// Bereichs kann auf ein beliebiges Ziel außerhalb zeigen, und zwischen
    /// Prüfung und Öffnen kann sich das Ziel sogar noch ändern
    /// (time-of-check/time-of-use). Indem diese Methode ausschließlich mit
    /// dem bereits aufgelösten Pfad weiterarbeitet, gibt es kein Zeitfenster
    /// mehr, in dem Prüfung und Öffnung auf unterschiedliche Dateien
    /// zugreifen könnten.
    ///
    /// # Arguments
    /// - `path` (`&std::path::Path`): der angeforderte Pfad, vor der
    ///   Auflösung.
    ///
    /// # Returns
    /// Die geöffnete, schreibgeschützt semantisch gemeinte [`std::fs::File`]-
    /// Handle auf den aufgelösten Pfad.
    ///
    /// # Errors
    /// - [`SensorError::OutsideScope`], wenn das aufgelöste Ziel außerhalb
    ///   des Bereichs liegt. Der Fehler nennt **nie** den Zielpfad.
    /// - [`SensorError::Io`], wenn die Auflösung (`canonicalize`) oder das
    ///   Öffnen selbst mit einem Betriebssystem-Fehler scheitert (z. B. Pfad
    ///   existiert nicht, keine Leseberechtigung).
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_dod_cap::ReadScope;
    /// use std::path::PathBuf;
    ///
    /// let scope = ReadScope::from_roots([PathBuf::from("/proc")]);
    /// let file = scope.open(std::path::Path::new("/proc/stat"))?;
    /// drop(file);
    /// # Ok::<(), harw_dod_cap::error::SensorError>(())
    /// ```
    pub fn open(&self, path: &Path) -> Result<File, SensorError> {
        let canonical = path
            .canonicalize()
            .map_err(SensorError::Io)?;

        if !self.allows(&canonical) {
            return Err(SensorError::OutsideScope);
        }

        File::open(&canonical).map_err(SensorError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::ReadScope;
    use crate::error::SensorError;
    use std::path::{Path, PathBuf};

    #[test]
    fn test_intersection_is_commutative() {
        let a = ReadScope::from_roots([PathBuf::from("/proc"), PathBuf::from("/sys")]);
        let b = ReadScope::from_roots([PathBuf::from("/sys"), PathBuf::from("/etc")]);
        assert_eq!(a.intersection(&b), b.intersection(&a));
    }

    #[test]
    fn test_intersection_is_idempotent() {
        let a = ReadScope::from_roots([PathBuf::from("/proc"), PathBuf::from("/sys")]);
        assert_eq!(a.intersection(&a), a);
    }

    #[test]
    fn test_intersection_result_is_subset_of_both_inputs() {
        let a = ReadScope::from_roots([PathBuf::from("/proc"), PathBuf::from("/sys")]);
        let b = ReadScope::from_roots([PathBuf::from("/sys"), PathBuf::from("/etc")]);
        let ab = a.intersection(&b);

        // Der gemeinsame Anteil bleibt erlaubt ...
        assert!(ab.allows(Path::new("/sys/class/thermal")));
        // ... und ist auch für jede einzelne Ausgangsmenge erlaubt (Teilmenge).
        assert!(a.allows(Path::new("/sys/class/thermal")));
        assert!(b.allows(Path::new("/sys/class/thermal")));

        // Was nur in einer der beiden Mengen stand, ist im Schnitt verboten.
        assert!(!ab.allows(Path::new("/proc/stat")));
        assert!(!ab.allows(Path::new("/etc/passwd")));
    }

    #[test]
    fn test_allows_path_in_scope() {
        let scope = ReadScope::from_roots([PathBuf::from("/srv/data")]);
        assert!(scope.allows(Path::new("/srv/data/report.json")));
    }

    #[test]
    fn test_allows_path_outside_scope() {
        let scope = ReadScope::from_roots([PathBuf::from("/srv/data")]);
        assert!(!scope.allows(Path::new("/etc/passwd")));
    }

    #[test]
    fn test_allows_rejects_prefix_without_directory_boundary() {
        let scope = ReadScope::from_roots([PathBuf::from("/srv/data")]);
        // "/srv/database" teilt den Byte-Präfix "/srv/data" mit der Wurzel,
        // liegt aber in einem anderen Verzeichnis: derselbe Fehlertyp wie
        // eine Domain-Prüfung ohne Punktgrenze.
        assert!(!scope.allows(Path::new("/srv/database/report.json")));
        assert!(!scope.allows(Path::new("/srv/database")));
    }

    #[test]
    fn test_allows_exact_root_match() {
        let scope = ReadScope::from_roots([PathBuf::from("/srv/data")]);
        assert!(scope.allows(Path::new("/srv/data")));
    }

    #[test]
    fn test_open_reads_file_inside_scope() {
        use std::io::Read;

        let dir = tempfile::tempdir().expect("tempdir for scope root");
        let file_path = dir.path().join("data.txt");
        std::fs::write(&file_path, b"hello").expect("write fixture file");

        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);
        let mut file = scope
            .open(&file_path)
            .expect("open must succeed for a file inside the scope");

        let mut contents = String::new();
        file.read_to_string(&mut contents).expect("read fixture file");
        assert_eq!(contents, "hello");
    }

    #[cfg(unix)]
    #[test]
    fn test_open_symlink_outside_scope_returns_outside_scope_without_naming_target() {
        use std::os::unix::fs::symlink;

        let inside = tempfile::tempdir().expect("tempdir as scope root");
        let outside = tempfile::tempdir().expect("tempdir outside the scope");
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, b"top secret").expect("write secret fixture file");

        let link = inside.path().join("link-to-secret");
        symlink(&secret, &link).expect("create symlink pointing outside the scope");

        let scope = ReadScope::from_roots([inside.path().to_path_buf()]);
        let err = scope
            .open(&link)
            .expect_err("a symlink resolving outside the scope must be rejected");

        assert!(matches!(err, SensorError::OutsideScope));

        let message = err.to_string();
        assert_eq!(message, "path resolves outside the sensor read scope");
        let outside_display = outside.path().to_string_lossy().into_owned();
        let secret_display = secret.to_string_lossy().into_owned();
        assert!(!message.contains("secret"));
        assert!(!message.contains(outside_display.as_str()));
        assert!(!message.contains(secret_display.as_str()));
    }
}
