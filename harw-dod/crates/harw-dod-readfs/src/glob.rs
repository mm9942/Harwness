//! Glob-Abgleich relativ zum Lesebereich.
//!
//! # Zweck
//! Findet Pfade unter einem `ReadScope`, deren Name einem Muster mit `*`
//! und `?` entspricht — z. B. `sys/class/thermal/thermal_zone*/temp` für
//! alle Thermalzonen, oder `sys/class/net/*/statistics/rx_bytes` für alle
//! Netzwerkschnittstellen.
//!
//! # Verantwortungsbereich
//! Besitzt [`glob`], die Grenzen [`MAX_GLOB_COMPONENTS`] und
//! [`MAX_GLOB_CANDIDATES`] und den privaten Wildcard-Matcher
//! `component_matches`. Prüft Musterhygiene (kein absolutes Muster, kein
//! `..`, Tiefengrenze) **vor** jedem Dateisystemzugriff, beschneidet die
//! Suche unterwegs auf Namen, die zu einer Bereichswurzel passen, und prüft
//! jeden Treffer abschließend über [`harw_dod_cap::ReadScope::resolve`] —
//! dieselbe Prüfung, die auch [`harw_dod_cap::ReadScope::open`] nutzt,
//! inklusive der Alias-Wurzeln für sysfs-Klassenverzeichnisse (Befund F-005).
//!
//! # Warum das Muster relativ ist, aber ab `/` durchsucht wird
//! Sensoren bauen das Muster aus `scope.roots().next()` plus einem Suffix;
//! „relativ“ bezieht sich auf die **Syntax** des Musters: es darf sich nie
//! selbst als absolut ausweisen (kein führendes `/`) und nie ein
//! `..`-Segment enthalten. Die Suche beginnt bei `/`, verfolgt aber nur
//! Kandidaten, die lexikalisch Vorfahr einer Anfragewurzel sind oder unter
//! einer liegen (komponentenweise, `Path::starts_with`). Verzeichnisse
//! unterhalb einer Wurzel werden erst gelistet, nachdem sie selbst
//! [`harw_dod_cap::ReadScope::resolve`] bestanden haben — eine Suche folgt
//! also keinem Symlink nach außerhalb und listet dort nichts.
//!
//! # Warum keine `glob`-Abhängigkeit
//! Diese Crate braucht nur Abgleich von `*` und `?` **auf einer einzelnen
//! Pfadkomponente** — kein `**`, keine Zeichenklassen.
//! `harw-context/src/selector.rs` (`segment_matches`) und
//! `harw-plan/src/admission.rs` (`glob_matches`) lösen im Workspace bereits
//! genau dieses Teilproblem mit demselben Zwei-Zeiger-Algorithmus;
//! `component_matches` unten ist eine eigenständige Kopie dieses
//! ~25-Zeilen-Musters.
//!
//! # Bewusste Ausnahme von „kein direkter `std::fs`-Zugriff"
//! `ReadScope` bietet kein Auflisten von Verzeichnisinhalten. [`glob`] ruft
//! dafür ausdrücklich `std::fs::read_dir` auf — die einzige Stelle in dieser
//! Crate, die das tut — und nur auf Verzeichnissen, die entweder
//! lexikalische Vorfahren einer Wurzel sind oder `resolve` bestanden haben.
//!
//! # Grenzen
//! [`MAX_GLOB_COMPONENTS`] begrenzt die Musterlänge, [`MAX_GLOB_CANDIDATES`]
//! die Zahl gleichzeitig verfolgter Kandidaten je Schritt. Beides verhindert,
//! dass ein Sensor über eine sysfs-Schleife (z. B. `…/subsystem/…`) oder ein
//! riesiges Verzeichnis unbegrenzt Arbeit erzeugt.
//!
//! # Exportierte Typen
//! Die freie Funktion [`glob`] und die Konstanten [`MAX_GLOB_COMPONENTS`],
//! [`MAX_GLOB_CANDIDATES`].
//!
//! # Nebenläufigkeit
//! Zustandslos; `Send + Sync`, von jedem Thread parallel aufrufbar. Jeder
//! Aufruf öffnet und schließt seine eigenen Verzeichnis-Handles.
//!
//! # Fehler
//! [`crate::error::ReadFsError::GlobPatternAbsolute`],
//! [`crate::error::ReadFsError::GlobPatternTraversal`] für ein ungültiges
//! Muster und [`crate::error::ReadFsError::GlobLimitExceeded`] bei
//! überschrittener Grenze. Ein Verzeichnis, das während der Suche nicht
//! gelesen werden kann, liefert an dieser Stelle einfach keine Treffer.
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_cap::scope::{AliasRoot, ReadScope};
//! use std::path::PathBuf;
//!
//! let thermal = AliasRoot::sysfs_class(PathBuf::from("/sys/class/thermal"))
//!     .map_err(|_| harw_dod_cap::SensorError::ToolFault)?;
//! let scope = ReadScope::from_roots_and_aliases(Vec::<PathBuf>::new(), [thermal]);
//! let zones = harw_dod_readfs::glob::glob(&scope, "sys/class/thermal/thermal_zone*/temp")?;
//! # Ok::<(), harw_dod_readfs::ReadFsError>(())
//! ```

use std::path::{Path, PathBuf};

use harw_dod_cap::ReadScope;

use crate::error::{ReadFsError, ReadFsResult};

/// Höchstzahl nicht-leerer Komponenten eines Glob-Musters: **32**.
///
/// # Begründung
/// Die tiefsten Produktionsmuster (`sys/class/drm/card*/device/hwmon/hwmon*/temp1_input`)
/// haben 7 Komponenten, Fixture-Muster unter einem Checkout-Pfad rund 15.
/// 32 lässt großzügig Luft und begrenzt trotzdem sysfs-Schleifen.
pub const MAX_GLOB_COMPONENTS: usize = 32;

/// Höchstzahl gleichzeitig verfolgter Kandidaten je Suchschritt: **4096**.
///
/// # Begründung
/// `/sys/block` hat auf dem RPi 5 rund 30 Einträge, `/sys/class/drm` 8; ein
/// Server mit vielen Blockgeräten oder Schnittstellen bleibt im niedrigen
/// dreistelligen Bereich. Mehr als 4096 Treffer deuten auf ein falsch
/// gewähltes Muster oder einen ungeeigneten Baum hin.
pub const MAX_GLOB_CANDIDATES: usize = 4096;

/// Findet Pfade, die auf `pattern` passen und im Bereich `scope` liegen.
///
/// # Description
/// `pattern` wird an `/` in Komponenten zerlegt. Eine Komponente ohne `*`
/// oder `?` wird als Literal angehängt; eine Komponente mit `*`/`?` listet
/// das jeweilige Verzeichnis auf und behält Einträge, deren Name
/// `component_matches` erfüllt. Nach jedem Schritt bleiben nur Kandidaten,
/// die lexikalisch Vorfahr einer Anfragewurzel sind oder darunter liegen;
/// Zwischenkomponenten müssen zu einem Verzeichnis führen. Ein Verzeichnis
/// unterhalb einer Wurzel wird nur gelistet, wenn es
/// [`ReadScope::resolve`] besteht. Abschließend bleibt jeder Kandidat nur,
/// wenn [`ReadScope::resolve`] ihn zulässt (einfache Wurzel oder Alias-Regeln);
/// ein Kandidat, der sich nicht auflösen lässt, wird ausgelassen.
///
/// # Arguments
/// - `scope` (`&ReadScope`): der Lesebereich.
/// - `pattern` (`&str`): `/`-getrenntes, bereichsrelatives Muster mit `*`
///   und `?` je Pfadkomponente. Darf nicht mit `/` beginnen, keine
///   `..`-Komponente enthalten und höchstens [`MAX_GLOB_COMPONENTS`]
///   Komponenten haben.
///
/// # Returns
/// Die passenden, zugelassenen Pfade **so wie angefragt** (nicht kanonisch —
/// z. B. `/sys/class/thermal/thermal_zone0`, nicht das `/sys/devices`-Ziel),
/// **sortiert**.
///
/// # Errors
/// - [`ReadFsError::GlobPatternAbsolute`]: `pattern` beginnt mit `/`.
/// - [`ReadFsError::GlobPatternTraversal`]: `pattern` enthält `..`.
/// - [`ReadFsError::GlobLimitExceeded`]: mehr als [`MAX_GLOB_COMPONENTS`]
///   Komponenten oder mehr als [`MAX_GLOB_CANDIDATES`] Kandidaten in einem
///   Schritt.
///
/// # Concurrency
/// Zustandslos; von jedem Thread parallel aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use harw_dod_cap::ReadScope;
/// use std::path::Path;
///
/// let scope = ReadScope::from_roots([Path::new("/proc").to_path_buf()]);
/// let stats = harw_dod_readfs::glob::glob(&scope, "proc/*/stat")?;
/// assert!(stats.iter().all(|p| p.ends_with("stat")));
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
    if components.len() > MAX_GLOB_COMPONENTS {
        return Err(ReadFsError::GlobLimitExceeded {
            pattern: pattern.to_owned(),
            limit_name: "components",
            limit: MAX_GLOB_COMPONENTS,
        });
    }
    let last_index = components.len().saturating_sub(1);

    let mut candidates: Vec<PathBuf> = vec![PathBuf::from("/")];

    for (index, component) in components.iter().copied().enumerate() {
        let has_wildcard = component.contains('*') || component.contains('?');
        let mut next = Vec::new();

        for dir in &candidates {
            if has_wildcard {
                if !may_list(scope, dir) {
                    continue;
                }
                // Bewusste, dokumentierte Ausnahme von „kein direkter
                // std::fs-Zugriff": `ReadScope` bietet kein Auflisten.
                let Ok(entries) = std::fs::read_dir(dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    // Nicht-UTF-8-Namen können kein `&str`-Muster erfüllen.
                    let Some(name) = name.to_str() else {
                        continue;
                    };
                    if component_matches(component, name) {
                        push_related(scope, &mut next, entry.path(), pattern)?;
                    }
                }
            } else {
                push_related(scope, &mut next, dir.join(component), pattern)?;
            }
        }

        if index != last_index {
            // `is_dir` folgt Symlinks; ob das Ziel erlaubt ist, prüft
            // `may_list` vor dem Auflisten bzw. `resolve` am Ende.
            next.retain(|path| path.is_dir());
        }

        candidates = next;
    }

    let mut results: Vec<PathBuf> = candidates
        .into_iter()
        .filter(|candidate| scope.resolve(candidate).is_ok())
        .collect();
    results.sort();
    Ok(results)
}

// Hängt `path` an `next`, wenn der Name lexikalisch zu einer Anfragewurzel
// passt (Vorfahr oder darunter); scheitert, sobald `MAX_GLOB_CANDIDATES`
// überschritten würde.
fn push_related(
    scope: &ReadScope,
    next: &mut Vec<PathBuf>,
    path: PathBuf,
    pattern: &str,
) -> ReadFsResult<()> {
    let related = scope
        .roots()
        .any(|root| root.starts_with(&path) || path.starts_with(root));
    if !related {
        return Ok(());
    }
    if next.len() >= MAX_GLOB_CANDIDATES {
        return Err(ReadFsError::GlobLimitExceeded {
            pattern: pattern.to_owned(),
            limit_name: "candidates",
            limit: MAX_GLOB_CANDIDATES,
        });
    }
    next.push(path);
    Ok(())
}

// Ein Verzeichnis darf gelistet werden, wenn es ein echter lexikalischer
// Vorfahr einer Anfragewurzel ist (nötig, um die Wurzel zu erreichen; die
// Einträge werden sofort per `push_related` beschnitten) oder wenn es selbst
// `ReadScope::resolve` besteht (kein Listen hinter einem Symlink nach außen).
fn may_list(scope: &ReadScope, dir: &Path) -> bool {
    let strict_ancestor = scope
        .roots()
        .any(|root| root != dir && root.starts_with(dir));
    strict_ancestor || scope.resolve(dir).is_ok()
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
    #[cfg(unix)]
    use harw_dod_cap::scope::AliasRoot;
    use tempfile::tempdir;

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Baut das Muster, das `dir` unter dem simulierten Wurzel-Startpunkt
    /// `/` adressiert — siehe Modul-Dokumentation zur Suche ab `/`.
    fn pattern_for(dir: &std::path::Path, suffix: &str) -> TestResult<String> {
        let relative = dir
            .strip_prefix("/")
            .map_err(ctx("Testverzeichnisse liegen unter /"))?;
        Ok(format!("{}/{suffix}", relative.display()))
    }

    #[test]
    fn test_glob_matches_are_sorted() -> TestResult {
        let dir = tempdir().map_err(ctx("tempdir"))?;
        fs::write(dir.path().join("c.txt"), "").map_err(ctx("write"))?;
        fs::write(dir.path().join("a.txt"), "").map_err(ctx("write"))?;
        fs::write(dir.path().join("b.txt"), "").map_err(ctx("write"))?;
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);

        let pattern = pattern_for(dir.path(), "*.txt")?;
        let results = glob(&scope, &pattern).map_err(ctx("glob muss gelingen"))?;

        let mut expected = vec![
            dir.path().join("a.txt"),
            dir.path().join("b.txt"),
            dir.path().join("c.txt"),
        ];
        expected.sort();
        assert_eq!(results, expected);
        Ok(())
    }

    #[test]
    fn test_glob_rejects_absolute_pattern() -> TestResult {
        let dir = tempdir().map_err(ctx("tempdir"))?;
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);

        let Err(err) = glob(&scope, "/etc/passwd") else {
            return Err(TestError::Unexpected(
                "absolutes Muster muss scheitern".into(),
            ));
        };
        assert!(matches!(err, ReadFsError::GlobPatternAbsolute { .. }));
        Ok(())
    }

    #[test]
    fn test_glob_rejects_pattern_with_parent_traversal() -> TestResult {
        let dir = tempdir().map_err(ctx("tempdir"))?;
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);

        let Err(err) = glob(&scope, "sub/../etc/passwd") else {
            return Err(TestError::Unexpected(
                "Muster mit '..' muss scheitern".into(),
            ));
        };
        assert!(matches!(err, ReadFsError::GlobPatternTraversal { .. }));
        Ok(())
    }

    #[test]
    fn test_glob_rejects_pattern_over_component_limit() -> TestResult {
        let dir = tempdir().map_err(ctx("tempdir"))?;
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);
        let pattern = vec!["a"; MAX_GLOB_COMPONENTS + 1].join("/");

        let Err(err) = glob(&scope, &pattern) else {
            return Err(TestError::Unexpected(
                "zu tiefes Muster muss scheitern".into(),
            ));
        };
        assert!(matches!(
            err,
            ReadFsError::GlobLimitExceeded { limit_name: "components", limit, .. }
                if limit == MAX_GLOB_COMPONENTS
        ));
        Ok(())
    }

    #[test]
    fn test_glob_rejects_more_candidates_than_limit() -> TestResult {
        let dir = tempdir().map_err(ctx("tempdir"))?;
        for i in 0..=MAX_GLOB_CANDIDATES {
            fs::write(dir.path().join(format!("f{i}")), "").map_err(ctx("write"))?;
        }
        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);
        let pattern = pattern_for(dir.path(), "f*")?;

        let Err(err) = glob(&scope, &pattern) else {
            return Err(TestError::Unexpected(
                "zu viele Kandidaten müssen scheitern".into(),
            ));
        };
        assert!(matches!(
            err,
            ReadFsError::GlobLimitExceeded { limit_name: "candidates", limit, .. }
                if limit == MAX_GLOB_CANDIDATES
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_glob_excludes_symlink_pointing_outside_scope() -> TestResult {
        let inside = tempdir().map_err(ctx("tempdir"))?;
        let outside = tempdir().map_err(ctx("tempdir"))?;
        fs::write(outside.path().join("secret.txt"), "geheim").map_err(ctx("write"))?;
        symlink(outside.path(), inside.path().join("link")).map_err(ctx("symlink"))?;

        let scope = ReadScope::from_roots([inside.path().to_path_buf()]);
        let pattern = pattern_for(inside.path(), "link/*.txt")?;

        let results = glob(&scope, &pattern).map_err(ctx("glob muss gelingen"))?;
        assert!(
            results.is_empty(),
            "ein Symlink aus dem Bereich heraus darf keinen Treffer liefern: {results:?}"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_glob_includes_symlink_pointing_inside_scope() -> TestResult {
        let dir = tempdir().map_err(ctx("tempdir"))?;
        let real = dir.path().join("real");
        fs::create_dir(&real).map_err(ctx("create_dir"))?;
        fs::write(real.join("value.txt"), "42").map_err(ctx("write"))?;
        symlink(&real, dir.path().join("link")).map_err(ctx("symlink"))?;

        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);
        let pattern = pattern_for(dir.path(), "link/*.txt")?;

        let results = glob(&scope, &pattern).map_err(ctx("glob muss gelingen"))?;
        assert_eq!(results, vec![dir.path().join("link").join("value.txt")]);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_glob_prunes_sibling_names_outside_root() -> TestResult {
        let dir = tempdir().map_err(ctx("tempdir"))?;
        let root = dir.path().join("a");
        fs::create_dir(&root).map_err(ctx("create_dir"))?;
        fs::write(root.join("x.txt"), "1").map_err(ctx("write"))?;
        // `b` zeigt kanonisch in die Wurzel, liegt aber namentlich daneben.
        symlink(&root, dir.path().join("b")).map_err(ctx("symlink"))?;

        let scope = ReadScope::from_roots([root.clone()]);
        let pattern = pattern_for(dir.path(), "*/x.txt")?;

        let results = glob(&scope, &pattern).map_err(ctx("glob muss gelingen"))?;
        assert_eq!(results, vec![root.join("x.txt")]);
        Ok(())
    }

    /// Nachgebauter sysfs-Baum mit echten Symlinks (class -> devices),
    /// strukturgleich zu `/sys/class/thermal` auf dem RPi 5.
    #[cfg(unix)]
    fn fake_sysfs(base: &std::path::Path) -> TestResult {
        let zones = base.join("sys/devices/virtual/thermal");
        for zone in ["thermal_zone0", "thermal_zone1"] {
            fs::create_dir_all(zones.join(zone)).map_err(ctx("create zone"))?;
            fs::write(zones.join(zone).join("temp"), "42000\n").map_err(ctx("write temp"))?;
        }
        let class = base.join("sys/class/thermal");
        fs::create_dir_all(&class).map_err(ctx("create class"))?;
        symlink(
            "../../devices/virtual/thermal/thermal_zone0",
            class.join("thermal_zone0"),
        )
        .map_err(ctx("symlink zone0"))?;
        symlink(
            "../../devices/virtual/thermal/thermal_zone1",
            class.join("thermal_zone1"),
        )
        .map_err(ctx("symlink zone1"))?;
        fs::create_dir_all(base.join("outside")).map_err(ctx("create outside"))?;
        fs::write(base.join("outside/temp"), "1\n").map_err(ctx("write outside"))?;
        Ok(())
    }

    #[cfg(unix)]
    fn thermal_alias_scope(base: &std::path::Path) -> TestResult<ReadScope> {
        let alias = AliasRoot::new(base.join("sys/class/thermal"), base.join("sys/devices"))
            .map_err(ctx("valid alias root"))?;
        Ok(ReadScope::from_roots_and_aliases(
            Vec::<PathBuf>::new(),
            [alias],
        ))
    }

    #[cfg(unix)]
    #[test]
    fn test_glob_alias_root_finds_class_symlinks_regression_f005() -> TestResult {
        let dir = tempdir().map_err(ctx("tempdir"))?;
        let base = dir
            .path()
            .canonicalize()
            .map_err(ctx("canonical tempdir"))?;
        fake_sysfs(&base)?;
        let class = base.join("sys/class/thermal");

        let plain = ReadScope::from_roots([class.clone()]);
        let pattern = pattern_for(&class, "thermal_zone*/temp")?;
        let plain_results = glob(&plain, &pattern).map_err(ctx("glob muss gelingen"))?;
        assert!(
            plain_results.is_empty(),
            "ohne Alias-Wurzel bleibt sysfs unsichtbar (Befund F-005)"
        );

        let alias_scope = thermal_alias_scope(&base)?;
        let results = glob(&alias_scope, &pattern).map_err(ctx("glob muss gelingen"))?;
        assert_eq!(
            results,
            vec![
                class.join("thermal_zone0/temp"),
                class.join("thermal_zone1/temp"),
            ]
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_glob_alias_root_excludes_entry_pointing_outside() -> TestResult {
        let dir = tempdir().map_err(ctx("tempdir"))?;
        let base = dir
            .path()
            .canonicalize()
            .map_err(ctx("canonical tempdir"))?;
        fake_sysfs(&base)?;
        let class = base.join("sys/class/thermal");
        symlink("../../../outside", class.join("thermal_zone9")).map_err(ctx("evil symlink"))?;

        let pattern = pattern_for(&class, "thermal_zone*/temp")?;
        let alias_scope = thermal_alias_scope(&base)?;
        let results = glob(&alias_scope, &pattern).map_err(ctx("glob muss gelingen"))?;
        assert!(!results.contains(&class.join("thermal_zone9/temp")));
        assert_eq!(results.len(), 2);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_glob_alias_root_excludes_double_symlink() -> TestResult {
        let dir = tempdir().map_err(ctx("tempdir"))?;
        let base = dir
            .path()
            .canonicalize()
            .map_err(ctx("canonical tempdir"))?;
        fake_sysfs(&base)?;
        let class = base.join("sys/class/thermal");
        symlink(
            "virtual/thermal/thermal_zone0",
            base.join("sys/devices/hop"),
        )
        .map_err(ctx("second-level symlink"))?;
        symlink("../../devices/hop", class.join("thermal_zone5"))
            .map_err(ctx("first-level symlink"))?;

        let pattern = pattern_for(&class, "thermal_zone*/temp")?;
        let alias_scope = thermal_alias_scope(&base)?;
        let results = glob(&alias_scope, &pattern).map_err(ctx("glob muss gelingen"))?;
        assert!(!results.contains(&class.join("thermal_zone5/temp")));
        assert_eq!(results.len(), 2);
        Ok(())
    }
}
