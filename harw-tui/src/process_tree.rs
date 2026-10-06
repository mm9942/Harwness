//! Prozessbaum-Abbruch für den harten Kill (doppeltes Ctrl+C).
//!
//! # Verantwortung
//! Dieses Modul beendet **alle** Nachkommen eines Prozesses sofort und ohne
//! Kooperation der Betroffenen — Shell-Kommandos samt ihrer Enkel, MCP-Server,
//! Browser-Treiber, Sandbox-Prozesse und die Prozesse job-gebundener
//! Kind-Agenten. Es ist die Prozessebene des festen „2× Ctrl+C = Not-Aus"
//! (siehe [`crate::hard_kill`]); die kooperative Ebene (`CancelToken`) liegt
//! daneben, nicht darin.
//!
//! # Warum ein Baum-Scan und keine Registrierung an jeder Spawn-Stelle
//! Der Baum wird aus dem Betriebssystem gelesen (`/proc`, hilfsweise `ps`),
//! nicht aus einem Register. Dadurch trifft der Kill auch Prozesse, die eine
//! Spawn-Stelle nie angemeldet hat, und kann nicht hinter dem Code
//! zurückbleiben, der neue Werkzeuge hinzufügt.
//!
//! # Ablauf von [`kill_tree`]
//! 1. **Einfrieren**: alle Nachkommen erhalten `SIGSTOP`, bis ein erneuter Scan
//!    keine neuen findet. Ein gestoppter Prozess kann nicht mehr forken — so
//!    entkommt kein Kind, indem sein Elternprozess zuerst stirbt und es
//!    dadurch an `init` umgehängt wird.
//! 2. **Beenden**: alle eingefrorenen Prozesse erhalten `SIGKILL`.
//! 3. **Nachkehren**: bis zum Zeitbudget wird erneut gescannt und alles
//!    Übriggebliebene erschossen (Prozesse, die zwischen Scan und Stopp
//!    geforkt haben).
//!
//! # Identität (kein Signal an die falsche PID)
//! Unter Linux öffnet jedes Ziel einen `pidfd`, **danach** wird die Startzeit
//! (`/proc/<pid>/stat`, Feld 22) mit der Momentaufnahme verglichen; das Signal
//! geht über den `pidfd`. Eine wiederverwendete PID wird so nie getroffen —
//! dasselbe Muster wie in `harw-killer`. Ohne `pidfd` (alter Kernel, Seccomp)
//! gilt die Startzeitprüfung unmittelbar vor `kill(2)`.
//!
//! # Verwaiste Nachkommen
//! Ein Prozess, dessen Elternprozess schon gestorben ist, hängt an `init` und
//! ist **kein** Nachkomme mehr. [`Census`] hält deshalb lebende Nachkommen
//! samt Startzeit fest, solange ihre Eltern noch leben; [`kill_tree`] nimmt
//! sie als zusätzliche Wurzeln, sobald sie verwaist sind.
//!
//! # Ausnahmen
//! `protected` nennt Prozesse, deren **gesamter Teilbaum** verschont bleibt
//! (die ablösbaren Hintergrund-Jobs der Nutzerin; ihr Überleben ist durch den
//! Beenden-Vertrag vorgesehen). Der Wurzelprozess selbst und PID 1 werden nie
//! signalisiert.
//!
//! # Plattform
//! Linux: `/proc` + `pidfd`. Andere Unix-Systeme: `ps -A -o pid=,ppid=,stat=`
//! und `kill(2)`; mangels Startzeit gibt es dort keine Identitätsprüfung und
//! kein [`Census`]. Nicht-Unix: [`kill_tree`] ist ein No-op.
//!
//! # Nebenläufigkeit
//! Alles hier ist synchron und blockierend, ohne Tokio-Laufzeit — der Aufruf
//! kommt aus dem Eingabe-Thread und muss auch dann funktionieren, wenn die
//! Async-Laufzeit hängt. Es gibt keine globalen Sperren.
//!
//! # Fehler
//! Die öffentlichen Funktionen sind infallibel: Scan- und Signalfehler werden
//! gezählt ([`TreeReport`]) und protokolliert, nie propagiert. Ein Prozess,
//! den wir nicht signalisieren dürfen (z. B. ein per `sudo` gestarteter
//! root-Prozess), zählt als `denied`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

/// Wie oft [`kill_tree`] höchstens einfriert, bevor es zum Beenden übergeht.
///
/// Jede Runde findet nur noch Prozesse, die zwischen zwei Scans entstanden
/// sind; mehr als eine Handvoll Runden bedeutet eine Fork-Bombe, die das
/// Zeitbudget ohnehin abschneiden würde.
const MAX_FREEZE_ROUNDS: u32 = 8;

/// Pause zwischen zwei Nachkehr-Scans.
const SWEEP_PAUSE: Duration = Duration::from_millis(2);

/// Ein Prozess aus einer Momentaufnahme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ProcEntry {
    /// Prozess-ID.
    pub(crate) pid: u32,
    /// Elternprozess zum Zeitpunkt der Momentaufnahme.
    pub(crate) ppid: u32,
    /// Startzeit in Clock-Ticks seit Boot (`/proc/<pid>/stat`, Feld 22);
    /// `0`, wenn die Quelle sie nicht liefert (`ps`).
    pub(crate) start_ticks: u64,
    /// Zombie oder tot: bereits beendet, wartet nur auf das Einsammeln.
    pub(crate) zombie: bool,
}

impl ProcEntry {
    /// Ob die Identität (PID **und** Startzeit) prüfbar ist.
    fn has_identity(&self) -> bool {
        self.start_ticks != 0
    }
}

/// Ergebnis eines [`kill_tree`]-Laufs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TreeReport {
    /// Prozesse, die `SIGKILL` erhalten haben.
    pub(crate) killed: usize,
    /// Prozesse, die wir nicht signalisieren durften (`EPERM`).
    pub(crate) denied: usize,
    /// Einfrier-Runden bis zum stabilen Baum.
    pub(crate) rounds: u32,
    /// Ob das Zeitbudget vor dem leeren Baum ablief.
    pub(crate) budget_exhausted: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Momentaufnahme
// ─────────────────────────────────────────────────────────────────────────────

/// Liest die Prozesstabelle: `/proc`, ersatzweise `ps`.
///
/// # Errors
/// Nur wenn **beide** Quellen scheitern.
pub(crate) fn snapshot() -> std::io::Result<Vec<ProcEntry>> {
    match snapshot_proc() {
        Ok(entries) if !entries.is_empty() => Ok(entries),
        _ => snapshot_ps(),
    }
}

/// Liest `/proc/<pid>/stat` aller numerischen Verzeichnisse.
///
/// Ein Prozess, der zwischen `read_dir` und `read` verschwindet, wird still
/// übersprungen.
fn snapshot_proc() -> std::io::Result<Vec<ProcEntry>> {
    let mut entries = Vec::new();
    for dirent in std::fs::read_dir("/proc")? {
        let Ok(dirent) = dirent else { continue };
        let name = dirent.file_name();
        let Some(pid) = name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(dirent.path().join("stat")) else {
            continue;
        };
        if let Some(entry) = parse_stat(pid, &text) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// Zerlegt eine `/proc/<pid>/stat`-Zeile.
///
/// Der Programmname steht in Klammern und darf selbst Leerzeichen und
/// Klammern enthalten; deshalb trennt die **letzte** schließende Klammer ab.
/// Nach ihr folgen Zustand (Feld 3), Elternprozess (Feld 4) und — 17 Felder
/// später — die Startzeit (Feld 22).
pub(crate) fn parse_stat(pid: u32, text: &str) -> Option<ProcEntry> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    if close <= open || text[..open].trim().parse::<u32>().ok()? != pid {
        return None;
    }
    let mut fields = text[close + 1..].split_whitespace();
    let state = fields.next()?;
    let ppid = fields.next()?.parse().ok()?;
    let start_ticks = fields.nth(17)?.parse().ok()?;
    Some(ProcEntry {
        pid,
        ppid,
        start_ticks,
        zombie: matches!(state, "Z" | "X" | "x"),
    })
}

/// Ersatzquelle ohne `/proc`: `ps -A -o pid=,ppid=,stat=` (Linux-procps und BSD).
fn snapshot_ps() -> std::io::Result<Vec<ProcEntry>> {
    let output = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,stat="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()?;
    Ok(parse_ps(&String::from_utf8_lossy(&output.stdout)))
}

/// Zerlegt die Ausgabe von `ps -A -o pid=,ppid=,stat=`.
pub(crate) fn parse_ps(text: &str) -> Vec<ProcEntry> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            let stat = fields.next().unwrap_or("");
            Some(ProcEntry {
                pid,
                ppid,
                start_ticks: 0,
                zombie: stat.starts_with('Z'),
            })
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Baum
// ─────────────────────────────────────────────────────────────────────────────

/// Alle lebenden Nachkommen von `roots` (ohne die Wurzeln selbst).
///
/// Der Teilbaum unterhalb jedes Prozesses aus `protected` wird nicht betreten
/// und taucht nicht auf; ebenso nie PID 0/1 oder ein Zombie. Zyklen (durch
/// eine inkonsistente Momentaufnahme) werden abgefangen. Reine Funktion.
pub(crate) fn descendants(
    entries: &[ProcEntry],
    roots: &[u32],
    protected: &HashSet<u32>,
) -> Vec<ProcEntry> {
    let mut children: HashMap<u32, Vec<&ProcEntry>> = HashMap::new();
    for entry in entries {
        children.entry(entry.ppid).or_default().push(entry);
    }
    let mut seen: HashSet<u32> = roots.iter().copied().collect();
    let mut queue: VecDeque<u32> = roots.iter().copied().collect();
    let mut found = Vec::new();
    while let Some(parent) = queue.pop_front() {
        for child in children.get(&parent).into_iter().flatten() {
            if child.pid <= 1 || protected.contains(&child.pid) || !seen.insert(child.pid) {
                continue;
            }
            queue.push_back(child.pid);
            if !child.zombie {
                found.push(**child);
            }
        }
    }
    found
}

// ─────────────────────────────────────────────────────────────────────────────
// Census: verwaiste Nachkommen nicht verlieren
// ─────────────────────────────────────────────────────────────────────────────

/// Lebende Nachkommen mit ihrer Identität, festgehalten **bevor** ihre
/// Elternprozesse sterben.
///
/// Das erste Ctrl+C bricht den Turn kooperativ ab; `shell.exec` beendet dabei
/// nur die Shell selbst. Deren Kinder (`cargo`, `rustc` …) hängen danach an
/// `init` und wären für einen reinen Baum-Scan unsichtbar. Wer vor dem
/// Abbruch eine Momentaufnahme festhält, kann sie später gezielt (PID **und**
/// Startzeit) wiederfinden und samt ihrem Teilbaum beenden.
#[derive(Debug, Default, Clone)]
pub(crate) struct Census {
    seen: HashSet<(u32, u64)>,
}

impl Census {
    /// Hält alle aktuellen Nachkommen von `root` fest (ohne `protected`).
    ///
    /// Prozesse ohne prüfbare Identität (`ps`-Quelle) werden übergangen: eine
    /// PID allein beweist nichts.
    pub(crate) fn note(&mut self, root: u32, protected: &HashSet<u32>) {
        let Ok(entries) = snapshot() else { return };
        self.note_from(&entries, root, protected);
    }

    /// Wie [`Census::note`], aber auf einer fertigen Momentaufnahme.
    pub(crate) fn note_from(&mut self, entries: &[ProcEntry], root: u32, protected: &HashSet<u32>) {
        self.seen.extend(
            descendants(entries, &[root], protected)
                .into_iter()
                .filter(ProcEntry::has_identity)
                .map(|entry| (entry.pid, entry.start_ticks)),
        );
    }

    /// Die festgehaltenen Prozesse, die in `entries` noch leben (gleiche PID
    /// **und** Startzeit), außer `protected`.
    pub(crate) fn survivors(&self, entries: &[ProcEntry], protected: &HashSet<u32>) -> Vec<u32> {
        entries
            .iter()
            .filter(|entry| {
                !entry.zombie
                    && !protected.contains(&entry.pid)
                    && self.seen.contains(&(entry.pid, entry.start_ticks))
            })
            .map(|entry| entry.pid)
            .collect()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Signale
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(unix)]
mod signal {
    //! Identitätsgeprüftes Signalisieren (siehe Moduldoku, „Identität").

    use super::ProcEntry;
    use rustix::io::Errno;
    use rustix::process::{Pid, Signal, kill_process};

    /// Ein Ziel mit gehaltener Identität.
    pub(super) struct Target {
        pub(super) pid: u32,
        handle: Handle,
    }

    enum Handle {
        #[cfg(target_os = "linux")]
        PidFd(std::os::fd::OwnedFd),
        Pid(Pid),
    }

    /// Ergebnis eines Signalversuchs.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Sent {
        /// Zugestellt.
        Delivered,
        /// Prozess existiert nicht mehr.
        Gone,
        /// Keine Berechtigung (`EPERM`).
        Denied,
    }

    /// Öffnet das Ziel und prüft seine Identität; `None`, wenn es schon weg ist
    /// oder nicht mehr dasselbe.
    pub(super) fn open(entry: &ProcEntry) -> Option<Target> {
        let pid = Pid::from_raw(i32::try_from(entry.pid).ok()?)?;
        #[cfg(target_os = "linux")]
        match rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()) {
            Ok(fd) => {
                // Erst öffnen, dann prüfen: Der `pidfd` bindet an die Instanz,
                // die zum Öffnungszeitpunkt unter dieser PID lief.
                return same_instance(entry).then_some(Target {
                    pid: entry.pid,
                    handle: Handle::PidFd(fd),
                });
            }
            Err(Errno::SRCH) => return None,
            // ENOSYS (alter Kernel), EPERM (Seccomp) …: PID-Weg unten.
            Err(_) => {}
        }
        same_instance(entry).then_some(Target {
            pid: entry.pid,
            handle: Handle::Pid(pid),
        })
    }

    /// Ob unter `entry.pid` noch derselbe, lebende Prozess läuft.
    fn same_instance(entry: &ProcEntry) -> bool {
        if !entry.has_identity() {
            // Quelle ohne Startzeit (`ps`): nichts zu vergleichen.
            return true;
        }
        std::fs::read_to_string(format!("/proc/{}/stat", entry.pid))
            .ok()
            .and_then(|text| super::parse_stat(entry.pid, &text))
            .is_some_and(|now| now.start_ticks == entry.start_ticks && !now.zombie)
    }

    /// Sendet `sig` an das Ziel.
    pub(super) fn send(target: &Target, sig: Signal) -> Sent {
        let result = match &target.handle {
            #[cfg(target_os = "linux")]
            Handle::PidFd(fd) => rustix::process::pidfd_send_signal(fd, sig),
            Handle::Pid(pid) => kill_process(*pid, sig),
        };
        match result {
            Ok(()) => Sent::Delivered,
            Err(Errno::SRCH) => Sent::Gone,
            Err(Errno::PERM) => Sent::Denied,
            Err(error) => {
                tracing::debug!(pid = target.pid, %error, "tui.hard_kill.signal_failed");
                Sent::Gone
            }
        }
    }

    pub(super) const STOP: Signal = Signal::STOP;
    pub(super) const KILL: Signal = Signal::KILL;
}

// ─────────────────────────────────────────────────────────────────────────────
// Kill
// ─────────────────────────────────────────────────────────────────────────────

/// Beendet alle Nachkommen von `root` (und die verwaisten Prozesse aus
/// `census`) per `SIGKILL`, ohne `root` selbst und ohne die Teilbäume unter
/// `protected`.
///
/// # Argumente
/// - `root`: Wurzel des Baums, in der Praxis die eigene PID.
/// - `protected`: Prozesse, deren Teilbaum verschont bleibt.
/// - `census`: zuvor festgehaltene Nachkommen (siehe [`Census`]).
/// - `budget`: höchstens so lange wird gescannt und nachgekehrt.
///
/// # Returns
/// Zählwerte des Laufs; nie ein Fehler.
#[cfg(unix)]
pub(crate) fn kill_tree(
    root: u32,
    protected: &HashSet<u32>,
    census: &Census,
    budget: Duration,
) -> TreeReport {
    use signal::{KILL, STOP, Sent, Target};

    let started = std::time::Instant::now();
    let mut report = TreeReport::default();
    let mut held: HashMap<u32, Target> = HashMap::new();
    let mut denied: HashSet<u32> = HashSet::new();

    // Wurzeln: der Baum selbst plus alles Verwaiste aus der Aufstellung.
    let roots_of = |entries: &[ProcEntry]| -> Vec<u32> {
        let mut roots = vec![root];
        roots.extend(census.survivors(entries, protected));
        roots
    };
    // Verwaiste Überlebende sind selbst Ziele, nicht nur Wurzeln.
    let targets_of = |entries: &[ProcEntry]| -> Vec<ProcEntry> {
        let roots = roots_of(entries);
        let mut targets = descendants(entries, &roots, protected);
        let known: HashSet<u32> = targets.iter().map(|entry| entry.pid).collect();
        targets.extend(
            entries
                .iter()
                .filter(|entry| roots[1..].contains(&entry.pid) && !known.contains(&entry.pid))
                .copied(),
        );
        targets.retain(|entry| entry.pid != root && entry.pid > 1);
        targets
    };

    // 1. Einfrieren, bis der Baum sich nicht mehr vergrößert.
    for _ in 0..MAX_FREEZE_ROUNDS {
        report.rounds += 1;
        let Ok(entries) = snapshot() else { break };
        let fresh: Vec<ProcEntry> = targets_of(&entries)
            .into_iter()
            .filter(|entry| !held.contains_key(&entry.pid) && !denied.contains(&entry.pid))
            .collect();
        if fresh.is_empty() {
            break;
        }
        for entry in fresh {
            let Some(target) = signal::open(&entry) else {
                continue;
            };
            match signal::send(&target, STOP) {
                Sent::Delivered => {
                    held.insert(entry.pid, target);
                }
                Sent::Denied => {
                    denied.insert(entry.pid);
                }
                Sent::Gone => {}
            }
        }
        if started.elapsed() >= budget {
            report.budget_exhausted = true;
            break;
        }
    }

    // 2. Alles Eingefrorene beenden. SIGKILL wirkt auch auf gestoppte Prozesse.
    for target in held.values() {
        match signal::send(target, KILL) {
            Sent::Delivered => report.killed += 1,
            Sent::Denied => {
                denied.insert(target.pid);
            }
            Sent::Gone => {}
        }
    }

    // 3. Nachkehren: was zwischen Scan und Stopp entstanden ist.
    while started.elapsed() < budget {
        let Ok(entries) = snapshot() else { break };
        let left: Vec<ProcEntry> = targets_of(&entries)
            .into_iter()
            .filter(|entry| !denied.contains(&entry.pid))
            .collect();
        if left.is_empty() {
            report.denied = denied.len();
            return report;
        }
        for entry in left {
            if let Some(target) = signal::open(&entry) {
                match signal::send(&target, KILL) {
                    Sent::Delivered => report.killed += 1,
                    Sent::Denied => {
                        denied.insert(entry.pid);
                    }
                    Sent::Gone => {}
                }
            }
        }
        std::thread::sleep(SWEEP_PAUSE);
    }
    report.budget_exhausted = true;
    report.denied = denied.len();
    report
}

/// Auf Nicht-Unix-Systemen gibt es keinen Prozessbaum-Kill; das Ergebnis ist leer.
#[cfg(not(unix))]
pub(crate) fn kill_tree(
    _root: u32,
    _protected: &HashSet<u32>,
    _census: &Census,
    _budget: Duration,
) -> TreeReport {
    TreeReport::default()
}

#[cfg(test)]
mod tests;
