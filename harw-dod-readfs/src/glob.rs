//! Glob-Abgleich relativ zum Lesebereich.
//!
//! # Zweck
//! Findet Pfade unter einem `ReadScope`, deren Name einem Muster mit `*`
//! und `?` entspricht — z. B. `sys/class/thermal/thermal_zone*/temp` für
//! alle Thermalzonen, oder `sys/class/net/*/statistics/rx_bytes` für alle
//! Netzwerkschnittstellen.
//!
//! # Verantwortungsbereich
//! Besitzt [`glob`] und den privaten Wildcard-Matcher `component_matches`.
//! Prüft Musterhygiene (kein absolutes Muster, kein `..`) **vor** jedem
//! Dateisystemzugriff und jeden Treffer **nach** dem Auflisten erneut gegen
//! [`harw_dod_cap::ReadScope::allows`].
//!
//! # Warum das Muster relativ ist, aber ab `/` durchsucht wird
//! [`harw_dod_cap::ReadScope`] legt seine Wurzeln nicht offen (keine
//! `roots()`-artige Methode in seinem Vertrag) — [`glob`] kann sie also
//! nicht kennen und nicht als Startpunkt verwenden. Die Suche beginnt daher
//! immer bei `/`; „relativ zum Bereich" bezieht sich auf die **Syntax** des
//! Musters, nicht auf einen zweiten, versteckten Startpunkt: ein Muster darf
//! sich nie selbst als absolut ausweisen (kein führendes `/`) und nie ein
//! `..`-Segment enthalten. Die eigentliche Bereichsgrenze wird ausschließlich
//! durch die abschließende, verbindliche `scope.allows`-Prüfung auf jedem
//! Treffer durchgesetzt (siehe unten) — die Syntaxprüfung ist zusätzliche
//! Absicherung, kein Ersatz dafür.
//!
//! # Warum keine `glob`-Abhängigkeit
//! Der Workspace verlangt Zurückhaltung bei neuen Abhängigkeiten und genau
//! eine Fassung je geteilter Abhängigkeit. Diese Crate braucht nur Abgleich
//! von `*` und `?` **auf einer einzelnen Pfadkomponente** — kein `**`, keine
//! Zeichenklassen, keine Ausschlussmuster. `harw-context/src/selector.rs`
//! (`segment_matches`) und `harw-plan/src/admission.rs` (`glob_matches`)
//! lösen im Workspace bereits genau dieses Teilproblem, mit demselben
//! klassischen Zwei-Zeiger-Algorithmus (`*`-Anker merken, bei Fehlschlag
//! zurückspringen). `component_matches` unten ist eine eigenständige Kopie
//! dieses ~25-Zeilen-Musters — eine neue Abhängigkeit dafür wäre nicht zu
//! rechtfertigen, und ein Re-Export aus `harw-context` würde eine
//! Abhängigkeit auf eine Crate ziehen, die mit Sensor-Lesezugriff nichts zu
//! tun hat.
//!
//! # Bewusste Ausnahme von „kein direkter `std::fs`-Zugriff"
//! `ReadScope` bietet kein Auflisten von Verzeichnisinhalten — nur `open`
//! und `allows`. Ein Glob **muss** aber Verzeichnisse auflisten können, um
//! `*`/`?` gegen tatsächliche Dateinamen abzugleichen. [`glob`] ruft dafür
//! ausdrücklich `std::fs::read_dir` auf Zwischen- und Zielverzeichnissen auf
//! — die einzige Stelle in dieser Crate, die das tut. Jeder auf diesem Weg
//! gefundene Pfad wird vor der Rückgabe zusätzlich über
//! `std::fs::canonicalize` aufgelöst (löst Symlinks auf) und mit dem
//! aufgelösten Ziel gegen [`harw_dod_cap::ReadScope::allows`] geprüft; ein
//! Treffer, dessen aufgelöstes Ziel außerhalb des Bereichs liegt — etwa weil
//! eine Zwischenkomponente ein Symlink nach außerhalb ist — erscheint
//! **nicht** im Ergebnis.
//!
//! # Exportierte Typen
//! Die freie Funktion [`glob`].
//!
//! # Nebenläufigkeit
//! Zustandslos; `Send + Sync`, von jedem Thread parallel aufrufbar. Jeder
//! Aufruf öffnet und schließt seine eigenen Verzeichnis-Handles.
//!
//! # Fehler
//! [`crate::error::ReadFsError::GlobPatternAbsolute`] und
//! [`crate::error::ReadFsError::GlobPatternTraversal`] für ein ungültiges
//! Muster. Ein Verzeichnis, das während der Suche nicht gelesen werden kann
//! (fehlt, keine Berechtigung), liefert an dieser Stelle einfach keine
//! Treffer — das ist kein Fehler des Glob-Aufrufs insgesamt.
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_cap::ReadScope;
//! use std::path::Path;
//!
//! let scope = ReadScope::from_roots([Path::new("/sys/class/thermal").to_path_buf()]);
//! let zones = harw_dod_readfs::glob::glob(&scope, "sys/class/thermal/thermal_zone*/temp")?;
//! # Ok::<(), harw_dod_readfs::ReadFsError>(())
//! ```

use std::path::PathBuf;

use harw_dod_cap::ReadScope;

use crate::error::{ReadFsError, ReadFsResult};

/// Findet Pfade, die auf `pattern` passen und im Bereich `scope` liegen.
///
/// # Description
/// `pattern` wird an `/` in Komponenten zerlegt. Eine Komponente ohne `*`
/// oder `?` wird als Literal an den bisherigen Kandidatenpfad angehängt;
/// eine Komponente mit `*`/`?` listet das jeweilige Verzeichnis auf
/// (`std::fs::read_dir`, siehe Modul-Dokumentation) und behält nur
/// Einträge, deren Name `component_matches` erfüllt. Zwischenkomponenten
/// müssen zu einem Verzeichnis führen, um weiterverfolgt zu werden. Nach dem
/// Aufbau aller Kandidatenpfade wird jeder über `std::fs::canonicalize`
/// aufgelöst und mit [`ReadScope::allows`] geprüft; nur zulässige, aufgelöst
/// im Bereich liegende Treffer bleiben im Ergebnis. Ein Kandidat, der sich
/// nicht auflösen lässt (kaputter Symlink, ein während der Suche
/// verschwundener Eintrag), wird stillschweigend ausgelassen statt die
/// gesamte Suche scheitern zu lassen.
///
/// # Arguments
/// - `scope` (`&ReadScope`): der Lesebereich, gegen den jeder Treffer geprüft
///   wird.
/// - `pattern` (`&str`): `/`-getrenntes, bereichsrelatives Muster mit `*`
///   (beliebig viele Zeichen) und `?` (genau ein Zeichen) je
///   Pfadkomponente. Darf nicht mit `/` beginnen und keine `..`-Komponente
///   enthalten.
///
/// # Returns
/// Die passenden, im Bereich liegenden Pfade, **sortiert**.
/// Verzeichnisreihenfolge ist dateisystemabhängig; ein Sensor, der sich
/// darauf verließe, wäre nicht deterministisch — und Determinismus ist die
/// erste der sechs Fixture-Prüfungen.
///
/// # Errors
/// - [`ReadFsError::GlobPatternAbsolute`]: `pattern` beginnt mit `/`.
/// - [`ReadFsError::GlobPatternTraversal`]: `pattern` enthält eine
///   `..`-Komponente.
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use harw_dod_cap::ReadScope;
/// use std::path::Path;
///
/// let scope = ReadScope::from_roots([Path::new("/sys/class/thermal").to_path_buf()]);
/// let zones = harw_dod_readfs::glob::glob(&scope, "sys/class/thermal/thermal_zone*/temp")?;
/// assert!(zones.iter().all(|p| p.ends_with("temp")));
/// # Ok::<(), harw_dod_readfs::ReadFsError>(())
/// ```
pub fn glob(scope: &ReadScope, pattern: &str) -> ReadFsResult<Vec<PathBuf>> {
    if pattern.starts_with('/') {
        return Err(ReadFsError::GlobPatternAbsolute {
            pattern: pattern.to_owned(),
        });
    }
    if pattern.split('/').any(|segment| segment == "..") {
        return Err(ReadFsError::GlobPatternTraversal {
            pattern: pattern.to_owned(),
        });
    }

    let components: Vec<&str> = pattern
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let last_index = components.len().saturating_sub(1);

    let mut candidates: Vec<PathBuf> = vec![PathBuf::from("/")];

    for (index, component) in components.iter().copied().enumerate() {
        let has_wildcard = component.contains('*') || component.contains('?');
        let mut next = Vec::new();

        for dir in &candidates {
            if has_wildcard {
                // Bewusste, dokumentierte Ausnahme von „kein direkter
                // std::fs-Zugriff": `ReadScope` bietet kein Auflisten.
                // Jeder so gefundene Pfad wird unten vor der Rückgabe
                // erneut — aufgelöst — gegen `scope.allows` geprüft.
                let Ok(entries) = std::fs::read_dir(dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let Some(name) = name.to_str() else {
                        continue;
                    };
                    if component_matches(component, name) {
                        next.push(entry.path());
                    }
                }
            } else {
                next.push(dir.join(component));
            }
        }

        if index != last_index {
            // Zwischenkomponenten müssen zu einem Verzeichnis führen, um
            // weiterverfolgt zu werden. `is_dir` folgt Symlinks — das ist
            // hier erwünscht, die Sicherheitsprüfung erfolgt separat unten
            // über den aufgelösten Pfad.
            next.retain(|path| path.is_dir());
        }

        candidates = next;
    }

    let mut results = Vec::new();
    for candidate in candidates {
        let Ok(canonical) = std::fs::canonicalize(&candidate) else {
            continue;
        };
        if scope.allows(&canonical) {
            results.push(candidate);
        }
    }

    results.sort();
    Ok(results)
}

/// Klassischer Wildcard-Abgleich auf einer einzelnen Pfadkomponente: `*`
/// steht für beliebig viele Zeichen, `?` für genau eines, jedes andere
/// Zeichen ist ein Literal.
///
/// Eigenständige Kopie desselben Zwei-Zeiger-Algorithmus wie
/// `segment_matches` in `harw-context/src/selector.rs` (dort wiederum an
/// `harw_plan::admission::glob_matches` angelehnt) — siehe die Begründung in
/// der Modul-Dokumentation, warum das keine neue Abhängigkeit rechtfertigt.
fn component_matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let mut star_index: Option<usize> = None;
    let mut match_index = 0usize;

    while ni < name.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == name[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star_index = Some(pi);
            match_index = ni;
            pi += 1;
        } else if let Some(star) = star_index {
            pi = star + 1;
            match_index += 1;
            ni = match_index;
        } else {
            return false;
        }
    }

    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }

    pi == pattern.len()
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use harw_dod_cap::ReadScope;
    use tempfile::tempdir;

    use super::*;

    /// Baut das Muster, das `dir` unter dem simulierten Wurzel-Startpunkt
    /// `/` adressiert — siehe Modul-Dokumentation zur Suche ab `/`.
    fn pattern_for(dir: &std::path::Path, suffix: &str) -> String {
        let relative = dir
            .strip_prefix("/")
            .expect("Testverzeichnisse liegen unter /");
        format!("{}/{suffix}", relative.display())
    }

    #[test]
    fn test_glob_matches_are_sorted() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("c.txt"), "").expect("write");
        fs::write(dir.path().join("a.txt"), "").expect("write");
        fs::write(dir.path().join("b.txt"), "").expect("write");
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);

        let pattern = pattern_for(dir.path(), "*.txt");
        let results = glob(&scope, &pattern).expect("glob muss gelingen");

        let mut expected = vec![
            dir.path().join("a.txt"),
            dir.path().join("b.txt"),
            dir.path().join("c.txt"),
        ];
        expected.sort();
        assert_eq!(results, expected);
    }

    #[test]
    fn test_glob_rejects_absolute_pattern() {
        let dir = tempdir().expect("tempdir");
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);

        let err = glob(&scope, "/etc/passwd").expect_err("absolutes Muster muss scheitern");
        assert!(matches!(err, ReadFsError::GlobPatternAbsolute { .. }));
    }

    #[test]
    fn test_glob_rejects_pattern_with_parent_traversal() {
        let dir = tempdir().expect("tempdir");
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);

        let err =
            glob(&scope, "sub/../etc/passwd").expect_err("Muster mit '..' muss scheitern");
        assert!(matches!(err, ReadFsError::GlobPatternTraversal { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn test_glob_excludes_symlink_pointing_outside_scope() {
        let inside = tempdir().expect("tempdir");
        let outside = tempdir().expect("tempdir");
        fs::write(outside.path().join("secret.txt"), "geheim").expect("write");
        symlink(outside.path(), inside.path().join("link")).expect("symlink");

        let scope = ReadScope::from_roots([inside.path().to_path_buf()]);
        let pattern = pattern_for(inside.path(), "link/*.txt");

        let results = glob(&scope, &pattern).expect("glob muss gelingen");
        assert!(
            results.is_empty(),
            "ein Symlink aus dem Bereich heraus darf keinen Treffer liefern: {results:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_glob_includes_symlink_pointing_inside_scope() {
        let dir = tempdir().expect("tempdir");
        let real = dir.path().join("real");
        fs::create_dir(&real).expect("create_dir");
        fs::write(real.join("value.txt"), "42").expect("write");
        symlink(&real, dir.path().join("link")).expect("symlink");

        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);
        let pattern = pattern_for(dir.path(), "link/*.txt");

        let results = glob(&scope, &pattern).expect("glob muss gelingen");
        assert_eq!(results, vec![dir.path().join("link").join("value.txt")]);
    }
}
