//! seccomp-Allowlist des Wardens (PL-100 WP 3.1, Entscheidung O10).
//!
//! # Zweck
//! Nach Socket, Policy, Schlüsseln, StateDirectory und Landlock braucht der
//! Warden nur noch eine kleine, feste Menge an Systemaufrufen: Speicher und
//! Threads (glibc, Rust-`std`), Dateien im StateDirectory und unter der
//! cgroup-Wurzel, Annahme und Austausch auf dem geerbten Socket, die Uhr
//! und das Schreiben ins Journal (stderr). [`install`] bindet genau diese
//! Menge als seccomp-BPF-Filter für **alle** Threads des Prozesses
//! (`SECCOMP_FILTER_FLAG_TSYNC`); später gestartete Threads und
//! Kindprozesse erben ihn. Jeder andere Aufruf scheitert mit `EPERM`
//! ([`DENIED_ERRNO`]).
//!
//! # Zusammensetzung
//! - [`base_syscalls`]: immer (Warden-Prozess nach dem Start).
//! - Der Bedarf des Netfilter-Backends
//!   (`crate::isolation::SandboxNeeds::syscalls`): nur wenn die Policy eine
//!   Netzaktion erlaubt. Für das `nft`-Kindprozess-Backend
//!   ([`nft_subprocess_syscalls`]) gehören dazu Prozessstart (`execve`,
//!   `pipe2`, `wait4`, ...) und was das `nft`-Programm selbst braucht, denn
//!   der Filter gilt über `execve` hinweg auch für das Kind; `socket` nur für
//!   `AF_NETLINK`.
//!
//! Mit der Vorgabe-Policy (nur Freeze/Thaw) kann der Warden nach dem Start
//! deshalb weder ein Programm ausführen noch einen Socket anlegen, und
//! `prctl` ist nur für `PR_SET_NAME` (Thread-Namen) offen.
//!
//! # Warum `EPERM` und nicht „Prozess beenden“
//! Die Liste ist aus dem Code hergeleitet, nicht aus einem `strace` eines
//! echten Laufs (offener Punkt für die Phase-3-VM). Ein übersehener Aufruf
//! soll als Fehler des einzelnen Eingriffs sichtbar werden
//! (`execution-failed`, eine Antwort an den Escalator), nicht als
//! Absturzschleife des Durchsetzers (`StartLimitBurst=3`). Das passt zu
//! `SystemCallErrorNumber=EPERM` der Unit. Nach der VM-Prüfung kann
//! [`DENIED_ACTION`] auf `KillProcess` wechseln.
//!
//! # Verhältnis zur Unit
//! Die Unit filtert zusätzlich mit `SystemCallFilter=@system-service`; der
//! Kernel wertet beide Filter aus, der strengere gewinnt. Jeder Aufruf
//! dieser Liste liegt in `@system-service` (geprüft mit
//! `systemd-analyze syscall-filter`, systemd 255). Der Aufruf `seccomp`
//! selbst liegt **nicht** darin; die Unit erlaubt ihn ausdrücklich, sonst
//! scheitert [`install`]. Nach [`install`] ist `seccomp` (wie `prctl` außer
//! `PR_SET_NAME` und alle `landlock_*`-Aufrufe) nicht mehr erlaubt.
//!
//! # Herkunft der Einträge
//! - `std::fs` und `std::io` über glibc (Zustandsdateien, `cgroup.freeze`,
//!   `cgroup.kill`, Inode-Prüfung über `statx`/`newfstatat`);
//! - atomares Ersetzen der Zustandsdateien: ausdrücklich `renameat2` auf
//!   jeder Architektur (`harw_dod_warden::STATE_RENAME_SYSCALL`, über
//!   `rustix::fs::renameat_with`). `std::fs::rename` wäre über glibc je nach
//!   Architektur `rename` (x86_64) oder `renameat` (aarch64) und ist deshalb
//!   im Warden nicht benutzt; beide Aufrufe stehen nicht auf der Liste;
//! - `rustix::net` 1.1.5 im `linux_raw`-Backend (`accept`/`accept4`,
//!   `getsockopt`/`setsockopt`, `recvfrom`, `sendto`; geprüft in
//!   `rustix-1.1.5/src/backend/linux_raw/net/syscalls.rs`);
//! - `std::thread` über glibc (`clone3`/`clone`, `set_robust_list`, `rseq`,
//!   `sigaltstack`, `prctl(PR_SET_NAME)` für den Hold-Timer) und
//!   `std::thread::sleep` (`clock_nanosleep`/`nanosleep`).
//!
//! # Abhängigkeits-Review `seccompiler` 0.5.0 (O10, 2026-11-08)
//! rust-vmm/Firecracker, Apache-2.0 OR BSD-3-Clause (beide auf der
//! Allowlist in `deny.toml`), veröffentlicht 2025-03-07, rund 29 Mio.
//! Downloads, kein RustSec-Eintrag bekannt. Laufzeit-Abhängigkeit nur
//! `libc` (über `landlock` schon in der Warden-Hülle); `serde`/`serde_json`
//! nur hinter dem nicht genutzten Feature `json`. Kein Build-Skript, kein
//! C-Code, kein `*-sys`. Rund 1 850 Zeilen ohne das JSON-Frontend; genau
//! zwei `unsafe`-Blöcke zur Laufzeit (`prctl(PR_SET_NO_NEW_PRIVS)` und
//! `syscall(SYS_seccomp, …)` in `apply_filter_with_flags`). Die Crate prüft
//! die Architektur im Filter (`AUDIT_ARCH_*`, sonst `KILL_PROCESS`) und
//! lehnt Programme ab `BPF_MAXINSNS` ab.
//!
//! # Nebenläufigkeit
//! [`install`] gehört in den Hauptthread, nach Landlock und vor jeden
//! weiteren Thread (Hold-Timer, Verbindungen); TSYNC überträgt den Filter
//! trotzdem auf alle vorhandenen Threads. [`SeccompProfile`] ist ein
//! unveränderlicher Werttyp.
//!
//! # Fehler
//! [`WardenBinError::SeccompUnavailable`]: unbekannte Architektur, Filter zu
//! groß oder Installation abgelehnt. Wie bei Landlock ein harter
//! Startfehler.
//!
//! # Examples
//! ```rust,ignore
//! // `harw-warden` hat kein `lib`-Target; geprüft in den Tests unten.
//! let profile = seccomp::install(&production.sandbox.syscalls)?;
//! tracing::info!(allowed = profile.allowed_count(), "seccomp allowlist installed");
//! ```

use std::collections::BTreeMap;

use seccompiler::{
    BackendError, BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition,
    SeccompFilter, SeccompRule, TargetArch,
};

use crate::error::WardenBinError;

/// `errno` für jeden nicht erlaubten Systemaufruf (`EPERM`, wie
/// `SystemCallErrorNumber=EPERM` der Unit).
pub const DENIED_ERRNO: u32 = libc::EPERM.unsigned_abs();

/// Antwort des Filters auf einen nicht erlaubten Systemaufruf.
pub const DENIED_ACTION: SeccompAction = SeccompAction::Errno(DENIED_ERRNO);

/// `prctl(PR_SET_NAME, …)`: der Hold-Timer-Thread benennt sich selbst.
const PRCTL_SET_NAME: [u64; 1] = [libc::PR_SET_NAME.unsigned_abs() as u64];

/// `socket(AF_NETLINK, …)`: nftables-Netlink des `nft`-Kindprozesses.
const SOCKET_AF_NETLINK: [u64; 1] = [libc::AF_NETLINK.unsigned_abs() as u64];

/// `kill(pid, SIGKILL)`: ein `nft`-Kind, das seine Frist überschreitet,
/// beenden (`crate::isolation`, `NFT_TIMEOUT`). Kein anderes Signal.
const KILL_SIGKILL: [u64; 1] = [libc::SIGKILL.unsigned_abs() as u64];

/// Bedingung an die Argumente eines erlaubten Systemaufrufs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgFilter {
    /// Jeder Aufruf ist erlaubt.
    Any,
    /// Nur Aufrufe, deren Argument `index` (32-Bit-Vergleich) einen der
    /// `values` trägt. Eine leere Wertmenge erlaubt nichts.
    OneOf {
        /// Argumentposition (0-basiert, höchstens 5).
        index: u8,
        /// Erlaubte Werte.
        values: &'static [u64],
    },
}

/// Ein erlaubter Systemaufruf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllowedSyscall {
    /// Name, nur für Logs und Tests.
    pub name: &'static str,
    /// Nummer auf der Zielarchitektur (`libc::SYS_*`).
    pub nr: i64,
    /// Argumentbedingung.
    pub filter: ArgFilter,
}

impl AllowedSyscall {
    /// Erlaubt `nr` ohne Argumentbedingung.
    ///
    /// # Arguments
    /// - `name` (`&'static str`): Name des Aufrufs.
    /// - `nr` (`i64`): Nummer (`libc::SYS_*`).
    ///
    /// # Returns
    /// Den Eintrag.
    #[must_use]
    pub const fn any(name: &'static str, nr: i64) -> Self {
        Self {
            name,
            nr,
            filter: ArgFilter::Any,
        }
    }

    /// Erlaubt `nr` nur mit bestimmten Werten im Argument `index`.
    ///
    /// # Arguments
    /// - `name` (`&'static str`): Name des Aufrufs.
    /// - `nr` (`i64`): Nummer (`libc::SYS_*`).
    /// - `index` (`u8`): Argumentposition (0-basiert).
    /// - `values` (`&'static [u64]`): erlaubte Werte (32-Bit-Vergleich).
    ///
    /// # Returns
    /// Den Eintrag.
    #[must_use]
    pub const fn one_of(name: &'static str, nr: i64, index: u8, values: &'static [u64]) -> Self {
        Self {
            name,
            nr,
            filter: ArgFilter::OneOf { index, values },
        }
    }
}

/// Die Systemaufrufe, die der Warden-Prozess nach dem Start immer braucht.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Herkunft der Einträge“. Enthält kein
/// `execve`, kein `socket`, kein `connect`, kein `kill`, keine
/// Rechte-, Mount- oder Namensraum-Aufrufe.
///
/// # Returns
/// Die Liste für die Zielarchitektur (`x86_64` zusätzlich mit den
/// Altaufrufen `open`, `stat`, `lstat`). Umbenannt wird nur über
/// `renameat2` (siehe Moduldoku, „Herkunft der Einträge“).
#[must_use]
pub fn base_syscalls() -> Vec<AllowedSyscall> {
    let mut list = vec![
        // Speicher: Allocator, Thread-Stacks, Schutzseiten.
        AllowedSyscall::any("brk", libc::SYS_brk),
        AllowedSyscall::any("mmap", libc::SYS_mmap),
        AllowedSyscall::any("munmap", libc::SYS_munmap),
        AllowedSyscall::any("mremap", libc::SYS_mremap),
        AllowedSyscall::any("mprotect", libc::SYS_mprotect),
        AllowedSyscall::any("madvise", libc::SYS_madvise),
        // Threads und Synchronisation (`std::thread`, `Mutex`, glibc).
        AllowedSyscall::any("futex", libc::SYS_futex),
        AllowedSyscall::any("sched_yield", libc::SYS_sched_yield),
        AllowedSyscall::any("sched_getaffinity", libc::SYS_sched_getaffinity),
        AllowedSyscall::any("clone", libc::SYS_clone),
        AllowedSyscall::any("clone3", libc::SYS_clone3),
        AllowedSyscall::any("set_robust_list", libc::SYS_set_robust_list),
        AllowedSyscall::any("rseq", libc::SYS_rseq),
        AllowedSyscall::any("exit", libc::SYS_exit),
        AllowedSyscall::any("exit_group", libc::SYS_exit_group),
        AllowedSyscall::any("restart_syscall", libc::SYS_restart_syscall),
        AllowedSyscall::one_of("prctl", libc::SYS_prctl, 0, &PRCTL_SET_NAME),
        // Signale: Stack-Overflow-Handler je Thread, `abort()`.
        AllowedSyscall::any("rt_sigprocmask", libc::SYS_rt_sigprocmask),
        AllowedSyscall::any("rt_sigaction", libc::SYS_rt_sigaction),
        AllowedSyscall::any("rt_sigreturn", libc::SYS_rt_sigreturn),
        AllowedSyscall::any("sigaltstack", libc::SYS_sigaltstack),
        AllowedSyscall::any("getpid", libc::SYS_getpid),
        AllowedSyscall::any("gettid", libc::SYS_gettid),
        AllowedSyscall::any("tgkill", libc::SYS_tgkill),
        // Uhr, Schlaf, Zufall (`jiff`, Hold-Timer, `HashMap`-Schlüssel).
        AllowedSyscall::any("clock_gettime", libc::SYS_clock_gettime),
        AllowedSyscall::any("clock_getres", libc::SYS_clock_getres),
        AllowedSyscall::any("gettimeofday", libc::SYS_gettimeofday),
        AllowedSyscall::any("clock_nanosleep", libc::SYS_clock_nanosleep),
        AllowedSyscall::any("nanosleep", libc::SYS_nanosleep),
        AllowedSyscall::any("getrandom", libc::SYS_getrandom),
        // Dateien: StateDirectory, cgroup-Kontrolldateien, Inode-Prüfung,
        // Journal auf stderr. Landlock begrenzt die Pfade.
        AllowedSyscall::any("openat", libc::SYS_openat),
        AllowedSyscall::any("close", libc::SYS_close),
        AllowedSyscall::any("read", libc::SYS_read),
        AllowedSyscall::any("pread64", libc::SYS_pread64),
        AllowedSyscall::any("write", libc::SYS_write),
        AllowedSyscall::any("writev", libc::SYS_writev),
        AllowedSyscall::any("lseek", libc::SYS_lseek),
        AllowedSyscall::any("fstat", libc::SYS_fstat),
        AllowedSyscall::any("newfstatat", libc::SYS_newfstatat),
        AllowedSyscall::any("statx", libc::SYS_statx),
        AllowedSyscall::any("fsync", libc::SYS_fsync),
        AllowedSyscall::any("fdatasync", libc::SYS_fdatasync),
        AllowedSyscall::any("ftruncate", libc::SYS_ftruncate),
        // Einziger Umbenennungsaufruf (`harw_dod_warden::STATE_RENAME_SYSCALL`).
        AllowedSyscall::any("renameat2", libc::SYS_renameat2),
        // IPC auf dem geerbten `SOCK_SEQPACKET`-Socket (rustix `linux_raw`).
        AllowedSyscall::any("accept", libc::SYS_accept),
        AllowedSyscall::any("accept4", libc::SYS_accept4),
        AllowedSyscall::any("getsockopt", libc::SYS_getsockopt),
        AllowedSyscall::any("setsockopt", libc::SYS_setsockopt),
        AllowedSyscall::any("recvfrom", libc::SYS_recvfrom),
        AllowedSyscall::any("sendto", libc::SYS_sendto),
    ];
    list.extend(legacy_base_syscalls());
    list
}

// Altaufrufe, die glibc auf x86_64 je nach Fassung für `std::fs` noch
// benutzt. Kein `rename`/`renameat`: die Zustandsdateien werden nur über
// `renameat2` ersetzt.
#[cfg(target_arch = "x86_64")]
fn legacy_base_syscalls() -> Vec<AllowedSyscall> {
    vec![
        AllowedSyscall::any("open", libc::SYS_open),
        AllowedSyscall::any("stat", libc::SYS_stat),
        AllowedSyscall::any("lstat", libc::SYS_lstat),
    ]
}

// Andere Architekturen kennen diese Altaufrufe nicht.
#[cfg(not(target_arch = "x86_64"))]
fn legacy_base_syscalls() -> Vec<AllowedSyscall> {
    Vec::new()
}

/// Zusätzliche Systemaufrufe des `nft`-Kindprozess-Backends.
///
/// # Description
/// Elternseite: `posix_spawn` über glibc (`clone3`/`clone`/`vfork`,
/// `pipe2`, `dup2`/`dup3`, `fcntl`, `execve`, `wait4`/`waitid`) und
/// `kill` nur mit `SIGKILL` (Frist überschritten). Kindseite (der
/// Filter gilt nach `execve` weiter): dynamischer Lader
/// (`set_tid_address`, `prlimit64`, `faccessat`, `readlinkat`, `uname`,
/// `get*id`, unter x86_64 `arch_prctl`/`access`/`readlink`) und nftables
/// selbst (`socket` nur `AF_NETLINK`, `bind`, `getsockname`, `sendmsg`,
/// `recvmsg`, `select`/`pselect6`/`poll`/`ppoll`). Best-effort aus der
/// Struktur von glibc und nftables hergeleitet; die VM-Prüfung mit
/// `strace -f` steht aus (offener Punkt).
///
/// # Returns
/// Die Liste für die Zielarchitektur.
#[must_use]
pub fn nft_subprocess_syscalls() -> Vec<AllowedSyscall> {
    let mut list = vec![
        // Prozessstart und Warten (Elternseite).
        AllowedSyscall::any("pipe2", libc::SYS_pipe2),
        AllowedSyscall::any("dup3", libc::SYS_dup3),
        AllowedSyscall::any("fcntl", libc::SYS_fcntl),
        AllowedSyscall::any("execve", libc::SYS_execve),
        AllowedSyscall::any("wait4", libc::SYS_wait4),
        AllowedSyscall::any("waitid", libc::SYS_waitid),
        // Ein hängendes Kind nach der Frist beenden: nur `SIGKILL`. Der
        // Warden läuft ohne `CAP_KILL`; erreichbar sind damit nur Prozesse
        // derselben uid (seine eigenen `nft`-Kinder).
        AllowedSyscall::one_of("kill", libc::SYS_kill, 1, &KILL_SIGKILL),
        // Dynamischer Lader und glibc-Start des Kindes.
        AllowedSyscall::any("set_tid_address", libc::SYS_set_tid_address),
        AllowedSyscall::any("prlimit64", libc::SYS_prlimit64),
        AllowedSyscall::any("faccessat", libc::SYS_faccessat),
        AllowedSyscall::any("faccessat2", libc::SYS_faccessat2),
        AllowedSyscall::any("readlinkat", libc::SYS_readlinkat),
        AllowedSyscall::any("uname", libc::SYS_uname),
        AllowedSyscall::any("getuid", libc::SYS_getuid),
        AllowedSyscall::any("geteuid", libc::SYS_geteuid),
        AllowedSyscall::any("getgid", libc::SYS_getgid),
        AllowedSyscall::any("getegid", libc::SYS_getegid),
        // nftables über Netlink (nur `AF_NETLINK`).
        AllowedSyscall::one_of("socket", libc::SYS_socket, 0, &SOCKET_AF_NETLINK),
        AllowedSyscall::any("bind", libc::SYS_bind),
        AllowedSyscall::any("getsockname", libc::SYS_getsockname),
        AllowedSyscall::any("sendmsg", libc::SYS_sendmsg),
        AllowedSyscall::any("recvmsg", libc::SYS_recvmsg),
        AllowedSyscall::any("pselect6", libc::SYS_pselect6),
        AllowedSyscall::any("ppoll", libc::SYS_ppoll),
    ];
    list.extend(legacy_nft_subprocess_syscalls());
    list
}

// x86_64-Altaufrufe von glibc-`posix_spawn`, Lader und nftables.
#[cfg(target_arch = "x86_64")]
fn legacy_nft_subprocess_syscalls() -> Vec<AllowedSyscall> {
    vec![
        AllowedSyscall::any("vfork", libc::SYS_vfork),
        AllowedSyscall::any("dup2", libc::SYS_dup2),
        AllowedSyscall::any("arch_prctl", libc::SYS_arch_prctl),
        AllowedSyscall::any("access", libc::SYS_access),
        AllowedSyscall::any("readlink", libc::SYS_readlink),
        AllowedSyscall::any("select", libc::SYS_select),
        AllowedSyscall::any("poll", libc::SYS_poll),
    ]
}

// Andere Architekturen kennen diese Altaufrufe nicht.
#[cfg(not(target_arch = "x86_64"))]
fn legacy_nft_subprocess_syscalls() -> Vec<AllowedSyscall> {
    Vec::new()
}

/// Die zusammengeführte Allowlist: je Systemaufrufnummer unbedingt erlaubt
/// oder nur mit bestimmten Argumentwerten.
///
/// # Description
/// Mehrere Einträge für dieselbe Nummer werden vereinigt: [`ArgFilter::Any`]
/// gewinnt, sonst gilt die Vereinigung der (Index, Wert)-Paare. Eine leere
/// Wertmenge erlaubt den Aufruf nicht (seccompiler deutet eine leere
/// Regelkette als „unbedingt erlaubt“ — genau das darf hier nie entstehen).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeccompProfile {
    // `None`: unbedingt erlaubt; `Some`: erlaubte (Index, Wert)-Paare.
    rules: BTreeMap<i64, Option<Vec<(u8, u64)>>>,
    // Name je Nummer (erster Eintrag gewinnt), nur für Logs und Tests.
    names: BTreeMap<i64, &'static str>,
}

impl SeccompProfile {
    /// Baut [`base_syscalls`] plus `extra`.
    ///
    /// # Arguments
    /// - `extra` (`&[AllowedSyscall]`): Bedarf des Netfilter-Backends (leer,
    ///   wenn die Policy keine Netzaktion erlaubt).
    ///
    /// # Returns
    /// Das Profil.
    #[must_use]
    pub fn with_extra(extra: &[AllowedSyscall]) -> Self {
        let mut profile = Self {
            rules: BTreeMap::new(),
            names: BTreeMap::new(),
        };
        for syscall in base_syscalls().iter().chain(extra) {
            profile.allow(*syscall);
        }
        profile
    }

    // Nimmt einen Eintrag auf (siehe Typdoku zur Vereinigung).
    fn allow(&mut self, syscall: AllowedSyscall) {
        match syscall.filter {
            ArgFilter::Any => {
                self.rules.insert(syscall.nr, None);
            }
            ArgFilter::OneOf { index, values } => {
                if values.is_empty() {
                    return;
                }
                let entry = self
                    .rules
                    .entry(syscall.nr)
                    .or_insert_with(|| Some(Vec::new()));
                if let Some(pairs) = entry {
                    for value in values {
                        if !pairs.contains(&(index, *value)) {
                            pairs.push((index, *value));
                        }
                    }
                }
            }
        }
        self.names.entry(syscall.nr).or_insert(syscall.name);
    }

    /// Anzahl der erlaubten Systemaufrufnummern.
    ///
    /// # Returns
    /// Die Anzahl.
    #[must_use]
    pub fn allowed_count(&self) -> usize {
        self.rules.len()
    }

    /// Namen der erlaubten Systemaufrufe, nach Nummer sortiert.
    ///
    /// # Returns
    /// Die Namen.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.names.values().copied().collect()
    }

    /// Ob `nr` überhaupt (mit irgendeinem Argument) erlaubt ist.
    #[cfg(test)]
    fn allows(&self, nr: i64) -> bool {
        self.rules.contains_key(&nr)
    }

    /// Ob `nr` mit dem Wert `value` im Argument `index` erlaubt ist.
    #[cfg(test)]
    fn allows_with(&self, nr: i64, index: u8, value: u64) -> bool {
        match self.rules.get(&nr) {
            Some(None) => true,
            Some(Some(pairs)) => pairs.contains(&(index, value)),
            None => false,
        }
    }

    /// Übersetzt das Profil in einen seccompiler-Filter.
    ///
    /// # Description
    /// Treffer: `Allow`; alles andere: [`DENIED_ACTION`]. Jedes
    /// (Index, Wert)-Paar wird eine eigene Regel mit genau einer Bedingung
    /// (Regeln einer Nummer sind „oder“-verknüpft).
    ///
    /// # Arguments
    /// - `arch` (`TargetArch`): Zielarchitektur des Filters.
    ///
    /// # Errors
    /// [`BackendError`]: Argumentindex außerhalb des Bereichs (nicht
    /// erreichbar mit den eingebauten Listen).
    pub fn to_filter(&self, arch: TargetArch) -> Result<SeccompFilter, BackendError> {
        let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
        for (nr, conditions) in &self.rules {
            let chain = match conditions {
                None => Vec::new(),
                Some(pairs) if pairs.is_empty() => continue,
                Some(pairs) => pairs
                    .iter()
                    .map(|(index, value)| {
                        SeccompCondition::new(
                            *index,
                            SeccompCmpArgLen::Dword,
                            SeccompCmpOp::Eq,
                            *value,
                        )
                        .and_then(|condition| SeccompRule::new(vec![condition]))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            };
            rules.insert(*nr, chain);
        }
        SeccompFilter::new(rules, DENIED_ACTION, SeccompAction::Allow, arch)
    }

    /// Übersetzt das Profil in ein ladbares BPF-Programm.
    ///
    /// # Arguments
    /// - `arch` (`TargetArch`): Zielarchitektur.
    ///
    /// # Errors
    /// `seccompiler::Error::Backend`: siehe [`Self::to_filter`] oder ein zu
    /// großer Filter.
    pub fn compile(&self, arch: TargetArch) -> Result<BpfProgram, seccompiler::Error> {
        let filter = self.to_filter(arch)?;
        BpfProgram::try_from(filter).map_err(seccompiler::Error::from)
    }
}

/// Löst einen Architekturnamen (`std::env::consts::ARCH`) auf.
///
/// # Errors
/// `seccompiler::Error::Backend`: von seccompiler nicht unterstützt (nur
/// `x86_64`, `aarch64`, `riscv64`).
fn arch_from_name(name: &str) -> Result<TargetArch, seccompiler::Error> {
    TargetArch::try_from(name).map_err(seccompiler::Error::from)
}

/// Installiert die Allowlist für alle Threads dieses Prozesses.
///
/// # Description
/// Baut [`SeccompProfile::with_extra`], übersetzt es für die
/// Host-Architektur und bindet es mit `apply_filter_all_threads`
/// (setzt zusätzlich `PR_SET_NO_NEW_PRIVS`, das die Unit ohnehin
/// verlangt). Muss nach Landlock laufen: `landlock_*` ist danach nicht mehr
/// erlaubt. **Nie in Tests aufrufen** — der Filter gälte für den ganzen
/// Testprozess.
///
/// # Arguments
/// - `extra` (`&[AllowedSyscall]`): Bedarf des Netfilter-Backends.
///
/// # Returns
/// Das installierte Profil (für das Start-Log).
///
/// # Errors
/// [`WardenBinError::SeccompUnavailable`]: Architektur, Übersetzung oder
/// Installation scheitert.
pub fn install(extra: &[AllowedSyscall]) -> Result<SeccompProfile, WardenBinError> {
    let profile = SeccompProfile::with_extra(extra);
    let program = arch_from_name(std::env::consts::ARCH)
        .and_then(|arch| profile.compile(arch))
        .map_err(|error| {
            tracing::error!(error = %error, "seccomp filter could not be built; refusing to start");
            WardenBinError::SeccompUnavailable(error)
        })?;
    seccompiler::apply_filter_all_threads(&program).map_err(|error| {
        tracing::error!(error = %error, "seccomp filter could not be installed; refusing to start");
        WardenBinError::SeccompUnavailable(error)
    })?;
    tracing::debug!(syscalls = ?profile.names(), "seccomp allowlist");
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::{
        AllowedSyscall, DENIED_ACTION, DENIED_ERRNO, SeccompProfile, arch_from_name, base_syscalls,
        nft_subprocess_syscalls,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use seccompiler::SeccompAction;

    // Größe, ab der der Kernel ein BPF-Programm ablehnt (`BPF_MAXINSNS`).
    const BPF_MAX_LEN: usize = 4096;

    fn value(raw: i32) -> u64 {
        u64::from(raw.unsigned_abs())
    }

    #[test]
    fn test_denied_calls_fail_with_eperm() {
        assert_eq!(DENIED_ERRNO, 1);
        assert_eq!(DENIED_ACTION, SeccompAction::Errno(1));
    }

    #[test]
    fn test_base_profile_covers_the_runtime_paths() {
        let names = SeccompProfile::with_extra(&[]).names();
        for required in [
            // Zustandsdateien und cgroup-Kontrolldateien.
            "openat",
            "read",
            "write",
            "close",
            "fsync",
            "fdatasync",
            "renameat2",
            "statx",
            "newfstatat",
            "fstat", // Threads und Hold-Timer.
            "clone3",
            "clone",
            "futex",
            "mmap",
            "munmap",
            "sigaltstack",
            "clock_nanosleep",
            "nanosleep",
            "clock_gettime",
            "getrandom",
            "exit", // IPC.
            "accept",
            "accept4",
            "getsockopt",
            "setsockopt",
            "recvfrom",
            "sendto",
        ] {
            assert!(names.contains(&required), "missing {required} in {names:?}");
        }
    }

    #[test]
    fn test_base_profile_cannot_spawn_connect_or_escalate() {
        let profile = SeccompProfile::with_extra(&[]);
        for (name, nr) in [
            ("execve", libc::SYS_execve),
            ("execveat", libc::SYS_execveat),
            ("socket", libc::SYS_socket),
            ("connect", libc::SYS_connect),
            ("bind", libc::SYS_bind),
            ("pipe2", libc::SYS_pipe2),
            ("wait4", libc::SYS_wait4),
            ("kill", libc::SYS_kill),
            ("ptrace", libc::SYS_ptrace),
            ("mount", libc::SYS_mount),
            ("unshare", libc::SYS_unshare),
            ("setns", libc::SYS_setns),
            ("setuid", libc::SYS_setuid),
            ("capset", libc::SYS_capset),
            ("bpf", libc::SYS_bpf),
            ("seccomp", libc::SYS_seccomp),
            ("landlock_restrict_self", libc::SYS_landlock_restrict_self),
            ("unlinkat", libc::SYS_unlinkat),
            ("mkdirat", libc::SYS_mkdirat),
            ("fchmodat", libc::SYS_fchmodat),
            ("ioctl", libc::SYS_ioctl),
            ("init_module", libc::SYS_init_module),
        ] {
            assert!(!profile.allows(nr), "{name} must not be allowed");
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_base_profile_has_no_legacy_process_calls_on_x86_64() {
        let profile = SeccompProfile::with_extra(&[]);
        for nr in [
            libc::SYS_fork,
            libc::SYS_vfork,
            libc::SYS_unlink,
            libc::SYS_rename,
            libc::SYS_renameat,
        ] {
            assert!(!profile.allows(nr));
        }
        assert!(profile.allows(libc::SYS_open));
    }

    // Die Zustandsdateien (Holds, Nonce-Ledger) werden auf jeder
    // Architektur über genau diesen Aufruf ersetzt; fehlte er, scheiterte
    // nach `install` jedes Stellen oder Aufheben eines Holds und jede
    // Verdichtung des Nonce-Ledgers mit `EPERM` (aarch64: glibc-`rename()`
    // wäre `renameat`).
    #[test]
    fn test_profile_allows_the_rename_call_of_the_state_files_on_every_arch() {
        let profile = SeccompProfile::with_extra(&[]);
        assert_eq!(harw_dod_warden::STATE_RENAME_SYSCALL, "renameat2");
        assert!(
            profile
                .names()
                .contains(&harw_dod_warden::STATE_RENAME_SYSCALL)
        );
        assert!(profile.allows(libc::SYS_renameat2));
    }

    #[test]
    fn test_prctl_is_limited_to_thread_names() {
        let profile = SeccompProfile::with_extra(&[]);
        assert!(profile.allows_with(libc::SYS_prctl, 0, value(libc::PR_SET_NAME)));
        for other in [
            libc::PR_SET_SECCOMP,
            libc::PR_SET_NO_NEW_PRIVS,
            libc::PR_SET_DUMPABLE,
            libc::PR_CAP_AMBIENT,
        ] {
            assert!(!profile.allows_with(libc::SYS_prctl, 0, value(other)));
        }
    }

    #[test]
    fn test_nft_profile_adds_process_start_and_netlink_sockets_only() {
        let profile = SeccompProfile::with_extra(&nft_subprocess_syscalls());
        for nr in [
            libc::SYS_execve,
            libc::SYS_pipe2,
            libc::SYS_wait4,
            libc::SYS_sendmsg,
            libc::SYS_recvmsg,
        ] {
            assert!(profile.allows(nr));
        }
        assert!(profile.allows_with(libc::SYS_socket, 0, value(libc::AF_NETLINK)));
        for family in [
            libc::AF_INET,
            libc::AF_INET6,
            libc::AF_UNIX,
            libc::AF_PACKET,
        ] {
            assert!(!profile.allows_with(libc::SYS_socket, 0, value(family)));
        }
        for nr in [libc::SYS_connect, libc::SYS_ptrace] {
            assert!(!profile.allows(nr));
        }
        // `kill` nur mit `SIGKILL` (hängendes `nft`-Kind), kein anderes Signal.
        assert!(profile.allows_with(libc::SYS_kill, 1, value(libc::SIGKILL)));
        for signal in [libc::SIGTERM, libc::SIGSTOP, libc::SIGHUP] {
            assert!(!profile.allows_with(libc::SYS_kill, 1, value(signal)));
        }
    }

    #[test]
    fn test_merge_unions_values_and_any_wins() {
        const DUMPABLE: [u64; 1] = [4];
        let narrow = SeccompProfile::with_extra(&[AllowedSyscall::one_of(
            "prctl",
            libc::SYS_prctl,
            0,
            &DUMPABLE,
        )]);
        assert!(narrow.allows_with(libc::SYS_prctl, 0, 4));
        assert!(narrow.allows_with(libc::SYS_prctl, 0, value(libc::PR_SET_NAME)));
        assert!(!narrow.allows_with(libc::SYS_prctl, 0, 22));
        let wide = SeccompProfile::with_extra(&[AllowedSyscall::any("prctl", libc::SYS_prctl)]);
        assert!(wide.allows_with(libc::SYS_prctl, 0, 22));
    }

    #[test]
    fn test_empty_value_set_never_allows_the_call() -> TestResult {
        const NOTHING: [u64; 0] = [];
        let profile = SeccompProfile::with_extra(&[AllowedSyscall::one_of(
            "socket",
            libc::SYS_socket,
            0,
            &NOTHING,
        )]);
        assert!(!profile.allows(libc::SYS_socket));
        let arch = arch_from_name(std::env::consts::ARCH).map_err(ctx("host arch"))?;
        profile.compile(arch).map_err(ctx("compiles"))?;
        Ok(())
    }

    #[test]
    fn test_lists_have_unique_numbers_and_names() -> TestResult {
        for list in [base_syscalls(), nft_subprocess_syscalls()] {
            let mut seen: Vec<i64> = Vec::new();
            for syscall in &list {
                if syscall.name.is_empty() || seen.contains(&syscall.nr) {
                    return Err(TestError::Unexpected(format!(
                        "duplicate or unnamed entry {syscall:?}"
                    )));
                }
                seen.push(syscall.nr);
            }
        }
        Ok(())
    }

    #[test]
    fn test_profiles_compile_for_the_host_arch_within_the_kernel_limit() -> TestResult {
        let arch = arch_from_name(std::env::consts::ARCH).map_err(ctx("host arch"))?;
        for extra in [Vec::new(), nft_subprocess_syscalls()] {
            let profile = SeccompProfile::with_extra(&extra);
            let program = profile.compile(arch).map_err(ctx("compiles"))?;
            assert!(!program.is_empty());
            assert!(
                program.len() < BPF_MAX_LEN,
                "{} instructions",
                program.len()
            );
        }
        Ok(())
    }

    #[test]
    fn test_unsupported_arch_is_rejected() {
        assert!(arch_from_name("sparc64").is_err());
        assert!(arch_from_name("x86_64").is_ok());
    }

    #[test]
    fn test_nft_profile_is_a_superset_of_the_base_profile() {
        let base = SeccompProfile::with_extra(&[]);
        let nft = SeccompProfile::with_extra(&nft_subprocess_syscalls());
        assert!(nft.allowed_count() > base.allowed_count());
        for name in base.names() {
            assert!(
                nft.names().contains(&name),
                "{name} lost in the nft profile"
            );
        }
    }
}
