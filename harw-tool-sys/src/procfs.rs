//! Lesender Zugriff auf `/proc` (reines Rust, `std::fs`).
//!
//! # Verantwortung
//! - reine Parser ([`parse_stat`], [`parse_status_uids`], [`parse_cmdline`],
//!   [`parse_meminfo`], [`parse_loadavg`], [`parse_cpu_times`], …), die auf
//!   Text arbeiten und deshalb ohne echtes `/proc` testbar sind,
//! - [`ProcFs`]: liest echte oder nachgebaute (`ProcFs::at`) `/proc`-Bäume,
//! - [`Clock`]: Ticks pro Sekunde, Seitengröße, Boot-Zeit und RAM-Größe.
//!
//! # Races
//! Ein Prozess kann zwischen `readdir` und dem Lesen seiner Dateien
//! verschwinden oder seine Daten ändern. Alle Leser liefern dann `None` bzw.
//! einen leeren Wert; nie ein Panic, nie einen Fehler für das ganze Werkzeug.
//! Dateien werden höchstens [`MAX_PROC_FILE_BYTES`] groß gelesen.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Obergrenze beim Lesen einer einzelnen `/proc`-Datei.
pub const MAX_PROC_FILE_BYTES: u64 = 256 * 1024;

/// Höchstzahl gelesener Prozesse je Aufruf.
pub const MAX_PROCESSES: usize = 100_000;

/// Felder von `/proc/<pid>/stat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatFields {
    /// Prozess-ID.
    pub pid: i32,
    /// Kurzname (`comm`, höchstens 15 Zeichen).
    pub comm: String,
    /// Zustandszeichen (`R`, `S`, `D`, `Z`, `T`, …).
    pub state: char,
    /// Eltern-PID.
    pub ppid: i32,
    /// CPU-Ticks im Benutzermodus.
    pub utime: u64,
    /// CPU-Ticks im Kernelmodus.
    pub stime: u64,
    /// Anzahl Threads.
    pub threads: u64,
    /// Startzeit in Ticks seit dem Boot.
    pub starttime: u64,
    /// Virtuelle Größe in Bytes.
    pub vsize: u64,
    /// Resident Set in Seiten.
    pub rss_pages: u64,
}

/// Parst eine `stat`-Zeile. `comm` darf Leerzeichen und Klammern enthalten.
#[must_use]
pub fn parse_stat(text: &str) -> Option<StatFields> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    if close < open {
        return None;
    }
    let pid = text[..open].trim().parse().ok()?;
    let comm = text[open + 1..close].to_owned();
    let rest: Vec<&str> = text[close + 1..].split_whitespace().collect();
    let field = |index: usize| rest.get(index).copied();
    Some(StatFields {
        pid,
        comm,
        state: field(0)?.chars().next()?,
        ppid: field(1)?.parse().ok()?,
        utime: field(11)?.parse().ok()?,
        stime: field(12)?.parse().ok()?,
        threads: field(17)?.parse().ok()?,
        starttime: field(19)?.parse().ok()?,
        vsize: field(20)?.parse().ok()?,
        rss_pages: field(21)?.parse().ok()?,
    })
}

/// Liest `(real_uid, effective_uid)` aus `/proc/<pid>/status`.
#[must_use]
pub fn parse_status_uids(text: &str) -> Option<(u32, u32)> {
    let line = text.lines().find(|l| l.starts_with("Uid:"))?;
    let mut parts = line.split_whitespace().skip(1);
    let real = parts.next()?.parse().ok()?;
    let effective = parts.next()?.parse().ok()?;
    Some((real, effective))
}

/// Liest `VmRSS` (in KiB) aus `/proc/<pid>/status`. Kernel-Threads haben keins.
///
/// Ab Linux 6.2 ist dieser Wert genauer als das `rss`-Feld von `stat` (dessen
/// Zähler sind verzögert); `ps` (procps-ng 4) verwendet ebenfalls `VmRSS`.
#[must_use]
pub fn parse_status_vmrss(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Zerlegt `/proc/<pid>/cmdline` (NUL-getrennt) in Argumente.
#[must_use]
pub fn parse_cmdline(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

/// `/proc/meminfo` als Schlüssel → KiB.
#[must_use]
pub fn parse_meminfo(text: &str) -> BTreeMap<String, u64> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let number = rest
            .split_whitespace()
            .next()
            .and_then(|n| n.parse::<u64>().ok());
        if let Some(number) = number {
            map.insert(key.trim().to_owned(), number);
        }
    }
    map
}

/// Lastdurchschnitt und Prozesszahlen aus `/proc/loadavg`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoadAvg {
    /// 1 Minute.
    pub one: f64,
    /// 5 Minuten.
    pub five: f64,
    /// 15 Minuten.
    pub fifteen: f64,
    /// Laufbereite Tasks.
    pub running: u64,
    /// Alle Tasks.
    pub total: u64,
}

/// Parst `/proc/loadavg`.
#[must_use]
pub fn parse_loadavg(text: &str) -> Option<LoadAvg> {
    let mut parts = text.split_whitespace();
    let one = parts.next()?.parse().ok()?;
    let five = parts.next()?.parse().ok()?;
    let fifteen = parts.next()?.parse().ok()?;
    let (running, total) = parts.next()?.split_once('/')?;
    Some(LoadAvg {
        one,
        five,
        fifteen,
        running: running.parse().ok()?,
        total: total.parse().ok()?,
    })
}

/// Summe der CPU-Ticks der Zeile `cpu ` in `/proc/stat`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTimes {
    /// Benutzermodus (inkl. nice).
    pub user: u64,
    /// Kernelmodus (inkl. irq/softirq).
    pub system: u64,
    /// Leerlauf.
    pub idle: u64,
    /// Warten auf I/O.
    pub iowait: u64,
    /// Gestohlene Zeit (Virtualisierung).
    pub steal: u64,
}

impl CpuTimes {
    /// Summe aller Kategorien.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.user + self.system + self.idle + self.iowait + self.steal
    }
}

/// Parst die Gesamt-CPU-Zeile aus `/proc/stat` und `btime`.
#[must_use]
pub fn parse_cpu_times(text: &str) -> Option<(CpuTimes, Option<i64>)> {
    let mut times = None;
    let mut btime = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("cpu ") {
            let v: Vec<u64> = rest
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            let get = |i: usize| v.get(i).copied().unwrap_or(0);
            times = Some(CpuTimes {
                user: get(0) + get(1),
                system: get(2) + get(5) + get(6),
                idle: get(3),
                iowait: get(4),
                steal: get(7),
            });
        } else if let Some(rest) = line.strip_prefix("btime ") {
            btime = rest.trim().parse().ok();
        }
    }
    times.map(|t| (t, btime))
}

/// Zählt die logischen CPUs (`cpuN`-Zeilen in `/proc/stat`).
#[must_use]
pub fn count_cpus(text: &str) -> usize {
    text.lines()
        .filter(|l| {
            l.starts_with("cpu") && l[3..].chars().next().is_some_and(|c| c.is_ascii_digit())
        })
        .count()
}

/// Konstanten des Systems für Umrechnungen.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    /// Ticks pro Sekunde (`CLK_TCK`).
    pub ticks_per_sec: u64,
    /// Seitengröße in Bytes.
    pub page_size: u64,
    /// Boot-Zeit (Unix-Sekunden), falls bekannt.
    pub boot_time: Option<i64>,
    /// Gesamt-RAM in KiB.
    pub mem_total_kib: u64,
    /// Laufzeit des Systems in Sekunden.
    pub uptime_secs: f64,
}

/// Ein Prozess mit den Feldern, die die Werkzeuge brauchen.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcInfo {
    /// Stat-Felder.
    pub stat: StatFields,
    /// Reale UID.
    pub uid: u32,
    /// `VmRSS` in KiB aus `status`, falls vorhanden.
    pub vm_rss_kib: Option<u64>,
    /// Kommandozeile (leer bei Kernel-Threads oder wenn nicht lesbar).
    pub cmdline: Vec<String>,
}

impl ProcInfo {
    /// Resident Set in KiB: `VmRSS` aus `status`, sonst die Seitenzahl aus `stat`.
    #[must_use]
    pub fn rss_kib(&self, page_size: u64) -> u64 {
        self.vm_rss_kib
            .unwrap_or_else(|| self.stat.rss_pages.saturating_mul(page_size) / 1024)
    }
}

/// Zugriff auf einen `/proc`-Baum.
#[derive(Debug, Clone)]
pub struct ProcFs {
    root: PathBuf,
}

/// Liest eine Datei höchstens [`MAX_PROC_FILE_BYTES`] groß.
fn read_capped(path: &Path) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_PROC_FILE_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

fn read_text(path: &Path) -> Option<String> {
    read_capped(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

impl ProcFs {
    /// Das echte `/proc`.
    #[must_use]
    pub fn real() -> Self {
        Self::at("/proc")
    }

    /// Ein beliebiger (z. B. nachgebauter) Baum.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Wurzel des Baums.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Alle numerischen Verzeichnisnamen, aufsteigend, höchstens [`MAX_PROCESSES`].
    #[must_use]
    pub fn pids(&self) -> Vec<i32> {
        let Ok(dir) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut pids: Vec<i32> = dir
            .filter_map(Result::ok)
            .filter_map(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .and_then(|n| n.parse::<i32>().ok())
            })
            .filter(|pid| *pid > 0)
            .collect();
        pids.sort_unstable();
        pids.truncate(MAX_PROCESSES);
        pids
    }

    /// Liest einen Prozess; `None`, wenn er verschwunden oder unlesbar ist.
    #[must_use]
    pub fn process(&self, pid: i32, with_cmdline: bool) -> Option<ProcInfo> {
        let base = self.root.join(pid.to_string());
        let stat = parse_stat(&read_text(&base.join("stat"))?)?;
        let status = read_text(&base.join("status"));
        let uid = status
            .as_deref()
            .and_then(parse_status_uids)
            .map_or(0, |(real, _)| real);
        let vm_rss_kib = status.as_deref().and_then(parse_status_vmrss);
        let cmdline = if with_cmdline {
            read_capped(&base.join("cmdline"))
                .map(|b| parse_cmdline(&b))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Some(ProcInfo {
            stat,
            uid,
            vm_rss_kib,
            cmdline,
        })
    }

    /// Alle lesbaren Prozesse; `vanished` zählt die zwischenzeitlich verschwundenen.
    #[must_use]
    pub fn processes(&self, with_cmdline: bool) -> (Vec<ProcInfo>, usize) {
        let mut vanished = 0usize;
        let mut out = Vec::new();
        for pid in self.pids() {
            match self.process(pid, with_cmdline) {
                Some(info) => out.push(info),
                None => vanished += 1,
            }
        }
        (out, vanished)
    }

    /// Inhalt von `meminfo`.
    #[must_use]
    pub fn meminfo(&self) -> BTreeMap<String, u64> {
        read_text(&self.root.join("meminfo"))
            .map(|t| parse_meminfo(&t))
            .unwrap_or_default()
    }

    /// `(uptime, idle)` in Sekunden.
    #[must_use]
    pub fn uptime(&self) -> Option<(f64, f64)> {
        let text = read_text(&self.root.join("uptime"))?;
        let mut parts = text.split_whitespace();
        Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
    }

    /// Lastdurchschnitt.
    #[must_use]
    pub fn loadavg(&self) -> Option<LoadAvg> {
        parse_loadavg(&read_text(&self.root.join("loadavg"))?)
    }

    /// CPU-Ticks, Boot-Zeit und CPU-Anzahl aus `stat`.
    #[must_use]
    pub fn cpu(&self) -> Option<(CpuTimes, Option<i64>, usize)> {
        let text = read_text(&self.root.join("stat"))?;
        let (times, btime) = parse_cpu_times(&text)?;
        Some((times, btime, count_cpus(&text).max(1)))
    }

    /// Systemkonstanten. Ticks und Seitengröße kommen von `rustix`.
    #[must_use]
    pub fn clock(&self) -> Clock {
        let ticks = rustix::param::clock_ticks_per_second();
        Clock {
            ticks_per_sec: if ticks > 0 { ticks } else { 100 },
            page_size: u64::try_from(rustix::param::page_size())
                .unwrap_or(4096)
                .max(1),
            boot_time: self.cpu().and_then(|(_, btime, _)| btime),
            mem_total_kib: self.meminfo().get("MemTotal").copied().unwrap_or(0),
            uptime_secs: self.uptime().map_or(0.0, |(up, _)| up),
        }
    }

    /// Eine Datei eines Prozesses als Text (für `lsof`, `ss`).
    #[must_use]
    pub fn read_file(&self, relative: &str) -> Option<String> {
        read_text(&self.root.join(relative))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;
    use std::fs;

    const STAT: &str = "1234 (my (weird) proc) S 1 1234 1234 0 -1 4194560 100 0 0 0 150 50 0 0 20 0 3 0 5000 104857600 2560 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";

    #[test]
    fn parses_stat_with_parentheses_in_comm() -> TestResult {
        let stat = parse_stat(STAT).ok_or(crate::test_support::TestError::Missing("stat"))?;
        assert_eq!(stat.pid, 1234);
        assert_eq!(stat.comm, "my (weird) proc");
        assert_eq!(stat.state, 'S');
        assert_eq!(stat.ppid, 1);
        assert_eq!((stat.utime, stat.stime), (150, 50));
        assert_eq!(stat.threads, 3);
        assert_eq!(stat.starttime, 5000);
        assert_eq!(stat.vsize, 104_857_600);
        assert_eq!(stat.rss_pages, 2560);
        Ok(())
    }

    #[test]
    fn rejects_garbage_stat_lines() -> TestResult {
        for bad in [
            "",
            "no parens",
            "1 (x",
            "1 )x(",
            "abc (x) S 1",
            "1 (x) S 1 2 3",
            "1 (x) S",
            "(x) S 1",
        ] {
            assert!(parse_stat(bad).is_none(), "{bad:?}");
        }
        Ok(())
    }

    #[test]
    fn parses_status_cmdline_meminfo_loadavg_cpu() -> TestResult {
        assert_eq!(
            parse_status_uids("Name:\tx\nUid:\t1000\t1001\t1000\t1000\n"),
            Some((1000, 1001))
        );
        assert_eq!(parse_status_uids("Name:\tx\n"), None);
        assert_eq!(
            parse_status_vmrss("Name:\tx\nVmRSS:\t  1944 kB\n"),
            Some(1944)
        );
        assert_eq!(parse_status_vmrss("Name:\tkthreadd\n"), None);
        assert_eq!(
            parse_cmdline(b"sh\0-c\0echo hi\0"),
            vec!["sh", "-c", "echo hi"]
        );
        assert!(parse_cmdline(b"").is_empty());
        assert_eq!(parse_cmdline(&[0xff, 0xfe, 0]).len(), 1);
        let mem = parse_meminfo(
            "MemTotal:       16384 kB\nMemFree:  100 kB\nbroken\nHugePages_Total:       0\n",
        );
        assert_eq!(mem.get("MemTotal"), Some(&16384));
        assert_eq!(mem.get("HugePages_Total"), Some(&0));
        let load = parse_loadavg("0.50 1.25 2.00 3/456 7890\n")
            .ok_or(crate::test_support::TestError::Missing("load"))?;
        assert_eq!(
            (load.one, load.five, load.fifteen, load.running, load.total),
            (0.5, 1.25, 2.0, 3, 456)
        );
        assert!(parse_loadavg("x y z").is_none());
        let (cpu, btime) = parse_cpu_times(
            "cpu  10 5 20 100 7 1 2 3 0 0\ncpu0 1 1 1 1 1 1 1 1 0 0\nbtime 1700000000\n",
        )
        .ok_or(crate::test_support::TestError::Missing("cpu"))?;
        assert_eq!(
            cpu,
            CpuTimes {
                user: 15,
                system: 23,
                idle: 100,
                iowait: 7,
                steal: 3
            }
        );
        assert_eq!(btime, Some(1_700_000_000));
        assert_eq!(count_cpus("cpu  1\ncpu0 1\ncpu1 1\nintr 5\n"), 2);
        Ok(())
    }

    fn fake_proc() -> TestResult<tempfile::TempDir> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        for (pid, name) in [(1, "init"), (42, "worker")] {
            let p = root.join(pid.to_string());
            fs::create_dir_all(&p)?;
            fs::write(
                p.join("stat"),
                STAT.replacen("1234 (my (weird) proc)", &format!("{pid} ({name})"), 1),
            )?;
            fs::write(
                p.join("status"),
                format!("Name:\t{name}\nUid:\t{}\t{}\t0\t0\n", pid * 10, pid * 10),
            )?;
            fs::write(p.join("cmdline"), format!("/bin/{name}\0--flag\0"))?;
        }
        // Ein „verschwundener“ Prozess: Verzeichnis ohne stat.
        fs::create_dir_all(root.join("99"))?;
        fs::create_dir_all(root.join("self"))?;
        fs::write(root.join("meminfo"), "MemTotal: 2048 kB\n")?;
        fs::write(root.join("uptime"), "100.50 400.00\n")?;
        fs::write(
            root.join("stat"),
            "cpu  1 0 1 8 0 0 0 0 0 0\ncpu0 1 0 1 8 0 0 0 0 0 0\nbtime 1700000000\n",
        )?;
        Ok(dir)
    }

    #[test]
    fn reads_a_fake_proc_tree_and_counts_vanished_processes() -> TestResult {
        let dir = fake_proc()?;
        let fs = ProcFs::at(dir.path());
        assert_eq!(fs.pids(), vec![1, 42, 99]);
        let (procs, vanished) = fs.processes(true);
        assert_eq!(vanished, 1);
        assert_eq!(procs.len(), 2);
        assert_eq!(procs[1].stat.comm, "worker");
        assert_eq!(procs[1].uid, 420);
        assert_eq!(procs[1].cmdline, vec!["/bin/worker", "--flag"]);
        let clock = fs.clock();
        assert_eq!(clock.boot_time, Some(1_700_000_000));
        assert_eq!(clock.mem_total_kib, 2048);
        assert!((clock.uptime_secs - 100.5).abs() < 1e-9);
        assert!(fs.process(7, false).is_none());
        Ok(())
    }

    #[test]
    fn missing_root_is_empty_not_an_error() -> TestResult {
        let fs = ProcFs::at("/definitely/not/there");
        assert!(fs.pids().is_empty());
        assert!(fs.meminfo().is_empty());
        assert!(fs.uptime().is_none());
        assert!(fs.loadavg().is_none());
        assert!(fs.cpu().is_none());
        Ok(())
    }

    #[test]
    fn real_proc_contains_this_process() -> TestResult {
        let fs = ProcFs::real();
        let me = i32::try_from(std::process::id()).unwrap_or(0);
        let info = fs
            .process(me, true)
            .ok_or(crate::test_support::TestError::Missing("self"))?;
        assert_eq!(info.stat.pid, me);
        assert!(!info.stat.comm.is_empty());
        assert!(fs.clock().ticks_per_sec > 0);
        assert!(fs.meminfo().contains_key("MemTotal"));
        Ok(())
    }
}
