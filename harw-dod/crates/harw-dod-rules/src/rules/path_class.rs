//! Gemeinsame, reine Pfad- und Zusammenfassungshilfen der Pfadregeln
//! [`super::ExecWritablePathRule`] und [`super::PersistenceWriteRule`]
//! (PL-100 WP1.6).
//!
//! # Verantwortungsbereich
//! - [`normalized_components`] und [`walk_components`]: zerlegen einen
//!   absoluten Ereignispfad **lexikalisch** in Komponenten (`.`/leere
//!   Komponenten fallen weg, `..` entfernt die vorige). Regeln führen kein
//!   I/O aus; Symlinks werden deshalb nicht gelesen. Einige **feste**
//!   Kernel- und FHS-Links löst der Gang aber an Ort und Stelle auf, also
//!   bevor ein folgendes `..` wirkt (siehe [`walk_components`]):
//!   `/proc/<pid|self|thread-self>/root` und
//!   `/proc/<pid>/task/<tid>/root` (Wurzel des Prozesses), `/dev/fd`,
//!   `/dev/stdin`, `/dev/stdout`, `/dev/stderr` (→ `/proc/self/fd/…`),
//!   `/var/run` (→ `/run`), `/var/lock` (→ `/run/lock`) und `/usr/tmp`
//!   (→ `/var/tmp`). Ein relativer Pfad hat keinen bekannten Ort und liefert
//!   `None`.
//! - [`home_relatives`]: die Reste eines Pfades unterhalb jedes passenden
//!   Home-Verzeichnisses — fest `/home/<name>/…`, `/root/…` und die
//!   ostree-Orte `/var/home/<name>/…` und `/var/roothome/…` (Fedora
//!   Silverblue/Kinoite u. a., wo `/home` und `/root` Symlinks dorthin sind
//!   und der Sensor den aufgelösten Pfad meldet), dazu jedes in der
//!   [`RuleConfig`] konfigurierte Home-Verzeichnis
//!   ([`RuleConfig::with_home_dir`], etwa `/home/<domäne>/<name>` oder ein
//!   Dienstkonto unter `/var/lib`).
//! - [`is_file_descriptor_path`]: ob ein Pfad über einen Datei-Deskriptor
//!   (`/proc/<pid>/fd/<n>`) statt über einen Ort im Dateisystem führt.
//! - [`is_process_cwd_path`]: ob ein Pfad über das Arbeitsverzeichnis eines
//!   Prozesses (`/proc/<pid>/cwd`) führt — ein Ort, den das Ereignis nicht
//!   verrät.
//! - [`summary_path`], [`stable_summary_path`] und [`actor_suffix`]: escapte,
//!   längenbegrenzte Teile einer Befund-Zusammenfassung. Pfade und `comm`
//!   sind angreiferkontrolliert und laufen immer durch
//!   `harw_untrusted_text::escape_line`. Die Zusammenfassung trägt bewusst
//!   **keine** pid: sie wechselt bei jedem Start desselben Programms, und eine
//!   Zusammenfassung soll für dasselbe Subjekt stabil bleiben
//!   (Deduplizierung).
//!
//! # Nebenläufigkeit
//! Freie, zustandslose Funktionen.
//!
//! # Fehler
//! Keine — alle Funktionen sind total.

use std::borrow::Cow;

use harw_dod_signals::{Actor, ProcessContext};
use harw_untrusted_text::escape_line;

use crate::rule::RuleConfig;

/// Höchstzahl Zeichen eines Pfades in einer Zusammenfassung (nach dem
/// Escapen, Kürzungsmarke eingerechnet).
pub(crate) const SUMMARY_PATH_MAX_CHARS: usize = 256;

// Länge des Metadaten-Hashes, den Cargo an Build-Artefakte hängt
// (`<name>-<16 Hexziffern>`).
const BUILD_HASH_HEX_DIGITS: usize = 16;

/// Zerlegt einen absoluten Pfad lexikalisch in seine Komponenten.
///
/// # Description
/// Wie [`walk_components`] ohne Beobachter für verlassene Verzeichnisse.
///
/// # Arguments
/// - `path` (`&str`): der Pfad aus dem Ereignis.
///
/// # Returns
/// `Some(Komponenten)` für einen absoluten Pfad (`[]` für `/`), `None` für
/// einen relativen oder leeren Pfad.
pub(crate) fn normalized_components(path: &str) -> Option<Vec<&str>> {
    walk_components(path, |_| {})
}

/// Geht einen absoluten Pfad lexikalisch durch und meldet jedes Verzeichnis,
/// das er über `..` wieder verlässt.
///
/// # Description
/// `.` und leere Komponenten fallen weg, `..` entfernt die vorige Komponente
/// (an der Wurzel bleibt es bei `/`). Nach **jeder** neuen Komponente wird
/// ein fester Link am Ende des Stapels durch sein Ziel ersetzt (Liste in der
/// Moduldoku). So bleibt ein folgendes `..` dort, wo auch der Kernel landet:
/// `/proc/self/root/../tmp/x` ist `/tmp/x` (der Kernel bleibt an der Wurzel
/// des Prozesses), `/var/run/../x` ist `/x` (Elternteil von `/run`).
///
/// `on_departed` sieht den Komponentenstapel **vor** jedem wirksamen `..`.
/// Ein Programmstart über `/tmp/l/../../usr/bin/x` ist lexikalisch
/// `/usr/bin/x`; ist `/tmp/l` aber ein Symlink des Angreifers, landet der
/// Kernel ganz woanders. Wer prüft, ob ein Pfad durch einen beschreibbaren Ort
/// **führt**, prüft deshalb auch diese Stapel. Die Funktion legt dafür keine
/// Kopien an: der Speicherbedarf bleibt linear in der Pfadlänge.
///
/// # Arguments
/// - `path` (`&str`): der Pfad aus dem Ereignis.
/// - `on_departed` (`FnMut(&[&str])`): Beobachter für verlassene
///   Verzeichnisse (nie mit leerem Stapel aufgerufen).
///
/// # Returns
/// `Some(Komponenten)` für einen absoluten Pfad (`[]` für `/`), `None` für
/// einen relativen oder leeren Pfad.
pub(crate) fn walk_components<'p>(
    path: &'p str,
    mut on_departed: impl FnMut(&[&'p str]),
) -> Option<Vec<&'p str>> {
    if !path.starts_with('/') {
        return None;
    }
    let mut components: Vec<&'p str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if !components.is_empty() {
                    on_departed(&components);
                }
                components.pop();
            }
            other => {
                components.push(other);
                resolve_fixed_link(&mut components);
            }
        }
    }
    Some(components)
}

// Ziele der festen Links (siehe `resolve_fixed_link`).
const PROCESS_ROOT: &[&str] = &[];
const PROC_SELF_FD: &[&str] = &["proc", "self", "fd"];
const PROC_SELF_FD_0: &[&str] = &["proc", "self", "fd", "0"];
const PROC_SELF_FD_1: &[&str] = &["proc", "self", "fd", "1"];
const PROC_SELF_FD_2: &[&str] = &["proc", "self", "fd", "2"];
const RUN: &[&str] = &["run"];
const RUN_LOCK: &[&str] = &["run", "lock"];
const VAR_TMP: &[&str] = &["var", "tmp"];

// Ersetzt einen festen Link am Ende des Stapels durch sein Ziel (Liste in
// der Moduldoku). Ein Link greift nur, wenn der Stapel **genau** dieser Link
// ist — `/srv/proc/self/root` ist kein Link.
fn resolve_fixed_link(components: &mut Vec<&str>) {
    let target: Option<&'static [&'static str]> = match components.as_slice() {
        ["proc", process, "root"] if is_proc_process(process) => Some(PROCESS_ROOT),
        ["proc", process, "task", thread, "root"]
            if is_proc_process(process) && is_decimal(thread) =>
        {
            Some(PROCESS_ROOT)
        }
        ["dev", "fd"] => Some(PROC_SELF_FD),
        ["dev", "stdin"] => Some(PROC_SELF_FD_0),
        ["dev", "stdout"] => Some(PROC_SELF_FD_1),
        ["dev", "stderr"] => Some(PROC_SELF_FD_2),
        ["var", "run"] => Some(RUN),
        ["var", "lock"] => Some(RUN_LOCK),
        ["usr", "tmp"] => Some(VAR_TMP),
        _ => None,
    };
    if let Some(target) = target {
        components.clear();
        components.extend_from_slice(target);
    }
}

// `self`, `thread-self` oder eine dezimale pid.
fn is_proc_process(segment: &str) -> bool {
    segment == "self" || segment == "thread-self" || is_decimal(segment)
}

// Eine nicht-leere Folge von ASCII-Ziffern.
fn is_decimal(segment: &str) -> bool {
    !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit())
}

/// Ob ein normalisierter Pfad über einen Datei-Deskriptor statt über einen
/// Ort im Dateisystem führt.
///
/// # Description
/// `/proc/<pid|self|thread-self>/fd/<n>[/…]` und
/// `/proc/<pid>/task/<tid>/fd/<n>[/…]` (`/dev/fd/<n>` ist nach
/// [`walk_components`] bereits `/proc/self/fd/<n>`). So startet
/// `fexecve`/`execveat(…, AT_EMPTY_PATH)` ein Programm, etwa aus einem
/// `memfd` — der Ort der Datei ist dem Ereignis nicht zu entnehmen.
///
/// # Arguments
/// - `components` (`&[&str]`): Ergebnis von [`walk_components`].
///
/// # Returns
/// `true` für einen Pfad an oder unter einem Deskriptor-Link.
pub(crate) fn is_file_descriptor_path(components: &[&str]) -> bool {
    match components {
        ["proc", process, "fd", fd, ..] => is_proc_process(process) && is_decimal(fd),
        ["proc", process, "task", thread, "fd", fd, ..] => {
            is_proc_process(process) && is_decimal(thread) && is_decimal(fd)
        }
        _ => false,
    }
}

/// Ob ein normalisierter Pfad über das Arbeitsverzeichnis eines Prozesses
/// führt.
///
/// # Description
/// `/proc/<pid|self|thread-self>/cwd[/…]` und
/// `/proc/<pid>/task/<tid>/cwd[/…]`. Der Kernel folgt dem Link in das
/// Arbeitsverzeichnis des Prozesses, das im Ereignis nicht steht; auch ein
/// folgendes `..` landet dort und nicht in `/proc/<pid>`. So umginge
/// `chdir("/tmp"); execve("/proc/self/cwd/payload")` ohne relativen Pfad jede
/// Ortsprüfung. Wer prüft, ob ein Pfad durch einen solchen Link **führt**,
/// prüft die über `..` verlassenen Stapel aus [`walk_components`] mit.
///
/// # Arguments
/// - `components` (`&[&str]`): Ergebnis von [`walk_components`] oder ein
///   dort gemeldeter, verlassener Stapel.
///
/// # Returns
/// `true` für den Link selbst und jeden Pfad darunter.
pub(crate) fn is_process_cwd_path(components: &[&str]) -> bool {
    match components {
        ["proc", process, "cwd", ..] => is_proc_process(process),
        ["proc", process, "task", thread, "cwd", ..] => {
            is_proc_process(process) && is_decimal(thread)
        }
        _ => false,
    }
}

/// Die Reste eines Pfades unterhalb jedes passenden Home-Verzeichnisses.
///
/// # Description
/// Geprüft werden zuerst die in `config` konfigurierten Home-Verzeichnisse
/// (in deren Reihenfolge), dann die festen Orte `/home/<name>`, `/root`,
/// `/var/home/<name>` und `/var/roothome` (siehe Moduldoku). Ein Pfad kann
/// unter mehreren liegen (`/home/corp/alice/.bashrc` ist `alice/.bashrc`
/// unter `/home/corp` **und** `.bashrc` unter einem konfigurierten
/// `/home/corp/alice`); der Aufrufer prüft jeden Rest.
///
/// # Arguments
/// - `components` (`&[&str]`): Ergebnis von [`normalized_components`].
/// - `config` (`Option<&RuleConfig>`): liefert die konfigurierten
///   Home-Verzeichnisse; `None` heißt nur die festen Orte.
///
/// # Returns
/// Für jedes passende Home-Verzeichnis die Komponenten darunter (ein leerer
/// Rest heißt: der Pfad **ist** das Home-Verzeichnis). Leer, wenn der Pfad
/// unter keinem liegt.
pub(crate) fn home_relatives<'s, 'c>(
    components: &'s [&'c str],
    config: Option<&RuleConfig>,
) -> Vec<&'s [&'c str]> {
    let mut rests: Vec<&'s [&'c str]> = config
        .into_iter()
        .flat_map(|config| config.home_dirs().iter())
        .filter_map(move |home| {
            let home = normalized_components(home)?;
            if components.starts_with(&home) {
                components.get(home.len()..)
            } else {
                None
            }
        })
        .collect();
    match components {
        // `_`, nicht `_name`: jede Alternative muss dieselben Namen binden.
        ["home", _, rest @ ..]
        | ["root", rest @ ..]
        | ["var", "home", _, rest @ ..]
        | ["var", "roothome", rest @ ..] => rests.push(rest),
        _ => {}
    }
    rests
}

/// Ein Pfad als escapter, längenbegrenzter Teil einer Zusammenfassung.
///
/// # Arguments
/// - `path` (`&str`): der Pfad, wie ihn das Ereignis trägt (nicht
///   normalisiert — die Zusammenfassung zeigt, was beobachtet wurde).
///
/// # Returns
/// Einzeilig escapt, höchstens [`SUMMARY_PATH_MAX_CHARS`] Zeichen.
pub(crate) fn summary_path(path: &str) -> String {
    escape_line(path, SUMMARY_PATH_MAX_CHARS)
}

/// Wie [`summary_path`], aber mit ersetzten Build-Hashes.
///
/// # Description
/// Jede Pfadkomponente der Form `<name>-<16 kleine Hexziffern>` (Cargos
/// Metadaten-Hash, z. B. `target/debug/deps/harw_x-0123456789abcdef`) wird
/// zu `<name>-<hash>`. Zwei Builds desselben Testprogramms ergeben so
/// dieselbe Zusammenfassung — das Subjekt bleibt für die Deduplizierung
/// stabil. Der Rest des Pfades bleibt, wie beobachtet.
///
/// # Arguments
/// - `path` (`&str`): der Pfad, wie ihn das Ereignis trägt.
///
/// # Returns
/// Einzeilig escapt, höchstens [`SUMMARY_PATH_MAX_CHARS`] Zeichen.
pub(crate) fn stable_summary_path(path: &str) -> String {
    let mut stable = String::with_capacity(path.len());
    for (index, component) in path.split('/').enumerate() {
        if index > 0 {
            stable.push('/');
        }
        stable.push_str(&without_build_hash(component));
    }
    escape_line(&stable, SUMMARY_PATH_MAX_CHARS)
}

// `<name>-<16 kleine Hexziffern>` → `<name>-<hash>`; jede andere Komponente
// bleibt unverändert.
fn without_build_hash(component: &str) -> Cow<'_, str> {
    let Some((stem, hash)) = component.rsplit_once('-') else {
        return Cow::Borrowed(component);
    };
    let is_build_hash = hash.len() == BUILD_HASH_HEX_DIGITS
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if stem.is_empty() || !is_build_hash {
        return Cow::Borrowed(component);
    }
    Cow::Owned(format!("{stem}-<hash>"))
}

/// Der stabile Akteurs-Zusatz einer Zusammenfassung.
///
/// # Arguments
/// - `actor` (`Option<&Actor>`): der Akteur des Ereignisses.
///
/// # Returns
/// `""` ohne Akteur; sonst ` (uid <uid>)` bzw., wenn der Prozesskontext
/// einen Namen trägt, ` (uid <uid>, comm '<comm>')` mit escaptem, auf
/// [`ProcessContext::COMM_MAX_CHARS`] Zeichen begrenztem `comm`. Keine pid
/// (siehe Moduldoku).
pub(crate) fn actor_suffix(actor: Option<&Actor>) -> String {
    let Some(actor) = actor else {
        return String::new();
    };
    match actor
        .process
        .as_ref()
        .and_then(|process| process.comm.as_deref())
    {
        Some(comm) => format!(
            " (uid {}, comm '{}')",
            actor.uid,
            escape_line(comm, ProcessContext::COMM_MAX_CHARS)
        ),
        None => format!(" (uid {})", actor.uid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalized_components_resolves_dots_lexically() {
        assert_eq!(normalized_components("/tmp/x"), Some(vec!["tmp", "x"]));
        assert_eq!(
            normalized_components("//usr/./bin/../../tmp//x"),
            Some(vec!["tmp", "x"])
        );
        assert_eq!(
            normalized_components("/../../tmp/x"),
            Some(vec!["tmp", "x"])
        );
        assert_eq!(normalized_components("/"), Some(Vec::new()));
    }

    #[test]
    fn test_relative_or_empty_paths_have_no_location() {
        assert_eq!(normalized_components("./payload"), None);
        assert_eq!(normalized_components("payload"), None);
        assert_eq!(normalized_components("../tmp/x"), None);
        assert_eq!(normalized_components(""), None);
    }

    #[test]
    fn test_proc_root_prefix_is_resolved() {
        assert_eq!(
            normalized_components("/proc/self/root/tmp/x"),
            Some(vec!["tmp", "x"])
        );
        assert_eq!(
            normalized_components("/proc/4242/root/proc/thread-self/root/dev/shm/x"),
            Some(vec!["dev", "shm", "x"])
        );
        assert_eq!(
            normalized_components("/proc/4242/task/4243/root/tmp/x"),
            Some(vec!["tmp", "x"])
        );
        // Kein pid-Segment: bleibt unverändert.
        assert_eq!(
            normalized_components("/proc/sys/root/x"),
            Some(vec!["proc", "sys", "root", "x"])
        );
        // Nur am Anfang des Stapels ein Link.
        assert_eq!(
            normalized_components("/srv/proc/self/root/x"),
            Some(vec!["srv", "proc", "self", "root", "x"])
        );
    }

    /// Review-Befund: die Prozesswurzel wurde erst nach `..` entfernt, sodass
    /// `/proc/self/root/../tmp/x` zu `proc/self/tmp/x` wurde. Der Kernel
    /// bleibt an der Wurzel des Prozesses; die Auflösung muss es auch.
    #[test]
    fn test_parent_of_proc_root_stays_at_the_root() {
        assert_eq!(
            normalized_components("/proc/self/root/../tmp/x"),
            Some(vec!["tmp", "x"])
        );
        assert_eq!(
            normalized_components("/proc/1/root/../../../dev/shm/x"),
            Some(vec!["dev", "shm", "x"])
        );
        // Eine Prozesswurzel mitten im Pfad setzt ebenfalls zurück.
        assert_eq!(
            normalized_components("/usr/../proc/self/root/../etc/ld.so.preload"),
            Some(vec!["etc", "ld.so.preload"])
        );
    }

    #[test]
    fn test_fhs_compatibility_links_are_resolved() {
        assert_eq!(
            normalized_components("/var/run/user/1000/x"),
            Some(vec!["run", "user", "1000", "x"])
        );
        assert_eq!(
            normalized_components("/var/run/shm/x"),
            Some(vec!["run", "shm", "x"])
        );
        assert_eq!(
            normalized_components("/var/lock/x"),
            Some(vec!["run", "lock", "x"])
        );
        assert_eq!(
            normalized_components("/usr/tmp/x"),
            Some(vec!["var", "tmp", "x"])
        );
        // `..` hinter einem aufgelösten Link führt zum Elternteil des Ziels.
        assert_eq!(normalized_components("/var/run/../x"), Some(vec!["x"]));
        // `/var/runner` ist kein Link.
        assert_eq!(
            normalized_components("/var/runner/x"),
            Some(vec!["var", "runner", "x"])
        );
    }

    #[test]
    fn test_dev_fd_aliases_resolve_to_proc_self_fd() {
        assert_eq!(
            normalized_components("/dev/fd/3"),
            Some(vec!["proc", "self", "fd", "3"])
        );
        assert_eq!(
            normalized_components("/dev/stdin"),
            Some(vec!["proc", "self", "fd", "0"])
        );
    }

    #[test]
    fn test_walk_reports_every_departed_directory() {
        let mut departed: Vec<Vec<String>> = Vec::new();
        let components = walk_components("/tmp/l/../../usr/bin/x", |stack| {
            departed.push(stack.iter().map(|part| (*part).to_owned()).collect());
        });
        assert_eq!(components, Some(vec!["usr", "bin", "x"]));
        assert_eq!(
            departed,
            vec![
                vec!["tmp".to_owned(), "l".to_owned()],
                vec!["tmp".to_owned()],
            ]
        );

        // `..` an der Wurzel verlässt kein Verzeichnis.
        let mut calls = 0_usize;
        let at_root = walk_components("/../x", |_| calls = calls.saturating_add(1));
        assert_eq!(at_root, Some(vec!["x"]));
        assert_eq!(calls, 0);
    }

    #[test]
    fn test_file_descriptor_paths_are_recognized() {
        for path in [
            "/dev/fd/3",
            "/dev/fd/3/x",
            "/proc/self/fd/7",
            "/proc/thread-self/fd/7",
            "/proc/4242/fd/0",
            "/proc/4242/task/4243/fd/9",
            "/dev/stdin",
        ] {
            let components = normalized_components(path).unwrap_or_default();
            assert!(is_file_descriptor_path(&components), "{path}");
        }
        for path in [
            "/proc/self/exe",
            "/proc/self/fd",
            "/proc/self/fdinfo/3",
            "/proc/sys/fd/3",
            "/proc/self/fd/x",
            "/srv/proc/self/fd/3",
        ] {
            let components = normalized_components(path).unwrap_or_default();
            assert!(!is_file_descriptor_path(&components), "{path}");
        }
    }

    #[test]
    fn test_home_relatives_covers_home_and_root() {
        let bashrc: &[&str] = &[".bashrc"];
        let ssh: &[&str] = &[".ssh"];
        let empty: &[&str] = &[];
        assert_eq!(
            home_relatives(&["home", "alice", ".bashrc"], None),
            vec![bashrc]
        );
        assert_eq!(home_relatives(&["root", ".ssh"], None), vec![ssh]);
        assert_eq!(home_relatives(&["home", "alice"], None), vec![empty]);
        assert!(home_relatives(&["home"], None).is_empty());
        assert!(home_relatives(&["etc", "profile"], None).is_empty());
    }

    /// Review-Befund (Runde 2): auf ostree-Hosts (Fedora Silverblue,
    /// Kinoite, Bluefin, Bazzite) sind `/home` und `/root` Symlinks nach
    /// `/var/home` bzw. `/var/roothome`, und der Sensor meldet den
    /// aufgelösten Pfad.
    #[test]
    fn test_home_relatives_covers_ostree_homes() {
        let bashrc: &[&str] = &[".bashrc"];
        let autostart: &[&str] = &[".config", "autostart", "x.desktop"];
        assert_eq!(
            home_relatives(&["var", "home", "alice", ".bashrc"], None),
            vec![bashrc]
        );
        assert_eq!(
            home_relatives(
                &["var", "roothome", ".config", "autostart", "x.desktop"],
                None
            ),
            vec![autostart]
        );
        assert!(home_relatives(&["var", "homes", "alice", ".bashrc"], None).is_empty());
        assert!(home_relatives(&["var", "home"], None).is_empty());
    }

    /// Konfigurierte Home-Verzeichnisse (verschachtelte/AD-Layouts,
    /// Dienstkonten) kommen zuerst; die festen Orte bleiben zusätzlich.
    #[test]
    fn test_home_relatives_uses_configured_home_dirs() -> crate::test_support::TestResult {
        use crate::test_support::ctx;
        use std::path::Path;

        let config = RuleConfig::new()
            .with_home_dir(Path::new("/home/corp/alice"))
            .map_err(ctx("AD-Home"))?
            .with_home_dir(Path::new("/srv/git"))
            .map_err(ctx("Dienstkonto"))?;
        let nested = ["home", "corp", "alice", ".bashrc"];
        let bashrc: &[&str] = &[".bashrc"];
        let below_corp: &[&str] = &["alice", ".bashrc"];
        assert_eq!(
            home_relatives(&nested, Some(&config)),
            vec![bashrc, below_corp]
        );
        let hooks: &[&str] = &["hooks", "x"];
        assert_eq!(
            home_relatives(&["srv", "git", "hooks", "x"], Some(&config)),
            vec![hooks]
        );
        // Nachbarverzeichnisse mit gleichem Präfix zählen nicht.
        assert!(home_relatives(&["srv", "gitx", "hooks"], Some(&config)).is_empty());
        Ok(())
    }

    #[test]
    fn test_process_cwd_paths_are_recognized() {
        for path in [
            "/proc/self/cwd",
            "/proc/self/cwd/x",
            "/proc/thread-self/cwd/x",
            "/proc/4242/cwd/x",
            "/proc/4242/task/4243/cwd/x",
            "/proc/self/root/proc/self/cwd/x",
        ] {
            let components = normalized_components(path).unwrap_or_default();
            assert!(is_process_cwd_path(&components), "{path}");
        }
        for path in [
            "/proc/self/exe",
            "/proc/sys/cwd/x",
            "/proc/4242/task/x/cwd/y",
            "/srv/proc/self/cwd/x",
            "/proc/self/cwdx/y",
        ] {
            let components = normalized_components(path).unwrap_or_default();
            assert!(!is_process_cwd_path(&components), "{path}");
        }
    }

    #[test]
    fn test_summary_path_escapes_control_and_bidi_characters() {
        let shown = summary_path("/tmp/a\u{202e}b\nc");
        assert_eq!(shown, "/tmp/a\\u{202e}b\\u{000a}c");
        assert!(summary_path(&"x".repeat(1_000)).chars().count() <= SUMMARY_PATH_MAX_CHARS);
    }

    #[test]
    fn test_stable_summary_path_replaces_build_hashes_only() {
        assert_eq!(
            stable_summary_path("/ws/target/debug/deps/harw_x-0123456789abcdef"),
            "/ws/target/debug/deps/harw_x-<hash>"
        );
        assert_eq!(
            stable_summary_path(
                "/ws/target/debug/build/foo-sys-fedcba9876543210/build-script-build"
            ),
            "/ws/target/debug/build/foo-sys-<hash>/build-script-build"
        );
        // Kein Hash: falsche Länge, Großbuchstaben, kein Stamm.
        for path in [
            "/ws/bin/tool-0123456789abcde",
            "/ws/bin/tool-0123456789ABCDEF",
            "/ws/bin/-0123456789abcdef",
            "/ws/bin/tool",
        ] {
            assert_eq!(stable_summary_path(path), path);
        }
        // Escapen bleibt wirksam.
        assert_eq!(
            stable_summary_path("/ws/a\u{202e}-0123456789abcdef"),
            "/ws/a\\u{202e}-<hash>"
        );
    }

    #[test]
    fn test_actor_suffix_is_stable_and_escaped() {
        assert_eq!(actor_suffix(None), "");
        let bare = Actor {
            uid: 1000,
            auid: None,
            cgroup: None,
            process: ProcessContext::from_pid(4242),
        };
        assert_eq!(actor_suffix(Some(&bare)), " (uid 1000)");
        let named = Actor {
            process: ProcessContext::from_pid(4242).map(|process| ProcessContext {
                comm: Some("ev\u{200b}il".to_owned()),
                ..process
            }),
            ..bare
        };
        let suffix = actor_suffix(Some(&named));
        assert_eq!(suffix, " (uid 1000, comm 'ev\\u{200b}il')");
        assert!(!suffix.contains("4242"), "keine pid in der Zusammenfassung");
    }
}
