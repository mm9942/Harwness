//! Deutung eines aufgelösten Dateideskriptor-Ziels als Dateisystempfad.
//!
//! # Verantwortungsbereich
//! fanotify liefert pro Ereignis einen offenen Dateideskriptor auf die
//! betroffene Datei, keinen Pfad. Der Pfad entsteht erst, indem der
//! Dateideskriptor über sein `/proc/self/fd/<n>`-Symlink aufgelöst wird
//! (`man 5 proc`, Abschnitt zu `/proc/[pid]/fd/`) — eine Operation, die
//! einen tatsächlich offenen Deskriptor braucht und deshalb im
//! **Bindungsteil** (bei der laufenden fanotify-Sitzung) stattfindet, nicht
//! hier.
//!
//! Diese Datei übernimmt den Teil danach: das rohe Linkziel, das die
//! Bindung bereits gelesen hat, in einen Pfad zu deuten. Das ist eine reine
//! Zeichenkettenoperation ohne jede Dateisystem-I/O — deshalb liegt sie im
//! vollständig testbaren Formungsteil.
//!
//! # Die `(deleted)`-Falle
//! Zeigt der Deskriptor auf eine zwischenzeitlich entfernte Datei, hängt der
//! Kernel dem Linkziel den Zusatz ` (deleted)` an (`man 5 proc`). Ohne
//! Behandlung würde dieser Zusatz als Teil des Pfads erscheinen und jede
//! nachgelagerte Bereichsprüfung auf einem Pfad ausführen, den es so nicht
//! gibt. [`interpret_fd_target`] erkennt und entfernt genau diesen Zusatz.
//!
//! # Exportierte Elemente
//! [`interpret_fd_target`].
//!
//! # Nebenläufigkeit
//! Zustandslos, `Send + Sync`, ohne innere Veränderlichkeit und ohne I/O.
//!
//! # Fehler
//! Keine — die Funktion ist total: jede Eingabe liefert eine Ausgabe.
//!
//! # Examples
//! ```rust
//! use harw_dod_fsmon::fdpath::interpret_fd_target;
//!
//! assert_eq!(interpret_fd_target("/etc/passwd"), "/etc/passwd");
//! assert_eq!(interpret_fd_target("/tmp/x (deleted)"), "/tmp/x");
//! ```

/// Der vom Kernel angehängte Zusatz für gelöschte, aber noch offene Dateien.
const DELETED_SUFFIX: &str = " (deleted)";

/// Deutet das rohe Ziel eines `/proc/.../fd/<n>`-Symlinks als Dateisystempfad.
///
/// # Description
/// Entfernt den in der Moduldoku beschriebenen `(deleted)`-Zusatz, falls
/// vorhanden. Reine Zeichenkettenoperation ohne Dateisystemzugriff — das
/// eigentliche Lesen des Symlinks (`std::fs::read_link` auf
/// `/proc/self/fd/<n>`) geschieht im Bindungsteil, während der Deskriptor
/// noch offen ist, und liefert dieser Funktion nur das Ergebnis.
///
/// # Arguments
/// - `raw_target` (`&str`): das ungedeutete Linkziel, wie vom Bindungsteil
///   gelesen.
///
/// # Returns
/// Den gedeuteten Pfad als `String`. Enthielt `raw_target` keinen
/// `(deleted)`-Zusatz, ist das Ergebnis identisch zur Eingabe.
///
/// # Errors
/// Keine — die Funktion ist total: jede Eingabe liefert eine Ausgabe.
///
/// # Examples
/// ```rust
/// use harw_dod_fsmon::fdpath::interpret_fd_target;
///
/// assert_eq!(interpret_fd_target("/srv/data/report.csv"), "/srv/data/report.csv");
/// assert_eq!(
///     interpret_fd_target("/srv/data/report.csv (deleted)"),
///     "/srv/data/report.csv"
/// );
/// ```
#[must_use]
pub fn interpret_fd_target(raw_target: &str) -> String {
    raw_target
        .strip_suffix(DELETED_SUFFIX)
        .unwrap_or(raw_target)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::interpret_fd_target;

    #[test]
    fn test_interpret_fd_target_plain_path_is_unchanged() {
        assert_eq!(interpret_fd_target("/etc/passwd"), "/etc/passwd");
    }

    #[test]
    fn test_interpret_fd_target_strips_deleted_suffix() {
        assert_eq!(interpret_fd_target("/tmp/secret.txt (deleted)"), "/tmp/secret.txt");
    }

    #[test]
    fn test_interpret_fd_target_does_not_strip_partial_match() {
        // Eine Datei, die zufällig auf "(deleted)" endet, ohne das führende
        // Leerzeichen des echten Kernel-Zusatzes, bleibt unverändert.
        assert_eq!(interpret_fd_target("/tmp/not(deleted)"), "/tmp/not(deleted)");
    }
}
