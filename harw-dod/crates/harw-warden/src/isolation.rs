//! Netzisolation für [`harw_dod_warden::NetworkIsolator`]: Zielauflösung
//! ([`NftNetworkIsolator`]), ein austauschbarer Transport für nftables
//! ([`NetfilterBackend`], PL-100 Entscheidung O10) mit dem `nft`-Kindprozess
//! als heutigem Backend ([`NftCommandBackend`]) und der abgeschaltete
//! Isolator ([`DisabledNetworkIsolator`]), solange die Policy keine
//! Netzaktion erlaubt.
//!
//! # Entscheidung O10: Bewertung der Rust-nftables-Crates (2026-11-08)
//! O10 verlangt nftables „über eine Rust-Crate statt des `nft`-Subprozesses“,
//! nach einem Abhängigkeits-Review im TCB. Geprüft wurden alle Treffer der
//! crates.io-Suche nach `nftables` gegen sechs Kriterien: Lizenz auf der
//! Allowlist in `deny.toml`; reines Rust ohne C-Bindung; kein Start des
//! `nft`-Programms; Schreibzugriff auf Basisketten und Regeln samt
//! `socket cgroupv2 level N`; Reife (Alter, Verbreitung, Pflege); Zuwachs der
//! Warden-Hülle (Sperrklinke `MAX_WARDEN_RUNTIME_DEPS = 54` in
//! `xtask/src/gate_warden.rs`, kein `*-sys`-Name ohne Eintrag in
//! `xtask/arch-policy.toml`, `[sys_crates] forbid_for_tcb`).
//!
//! | Crate | Fassung | Lizenz | Befund |
//! |---|---|---|---|
//! | `nftables`, `nftables-async` | 0.6.3, 0.4.1 | MIT | erzeugen JSON für das `nft`-Programm und starten es — kein Gewinn gegenüber dem Kindprozess |
//! | `rustables` | 0.9.0 | GPL-3.0-or-later | Lizenz nicht auf der Allowlist |
//! | `nftnl` | 0.9.4 | MIT OR Apache-2.0 | Bindung an `libnftnl` über `nftnl-sys` (C-Build, Gate 5) |
//! | `nftnl-rs` | 0.5.1 | MPL-2.0 | kein `socket cgroupv2`, eigenes `unsafe`, zieht `rand` 0.9 |
//! | `jip-nftables` | 0.1.0 (2026-04-18) | MIT OR Apache-2.0 | reines Rust, aber nur lesend (`dump_tables/chains/rules`) |
//! | `nftables-nl` | 0.2.0 (2026-11-05) | MIT | reines Rust, schreibt nur Tabellen/Ketten, keine Regeln; drei Tage alt |
//! | `crabtables` | 0.1.0 (2026-10-24) | MIT OR Apache-2.0 | reines Rust, kennt `socket cgroupv2`; aber 35 Downloads, rund 32 000 Zeilen für den TCB-Review, eigenes `unsafe` samt libc-FFI, zieht `netlink-sys` 0.9 (`*-sys`-Name, neu in der Hülle) und verlangt `libc` ≥ 0.2.191 (Workspace-Lock: 0.2.190) |
//!
//! **Ergebnis:** Keine Crate besteht heute das Review. Die einzige
//! funktional passende (`crabtables`) ist zwei Wochen alt und für einen
//! privilegierten Prozess nicht prüfbar groß. Deshalb bleibt der
//! `nft`-Kindprozess das produktive Backend, jetzt hinter
//! [`NetfilterBackend`], und es kommt **keine** nftables-Abhängigkeit hinzu.
//! Ein späteres Netlink-Backend (eine gereifte Crate oder eine eigene,
//! kleine Kodierung über `rustix` und `NETLINK_NETFILTER`) ersetzt nur
//! [`NftCommandBackend`]: Eingabeprüfung, Zielauflösung und Fehlerbilder
//! bleiben in [`NftNetworkIsolator`], der Sandbox-Bedarf wandert über
//! [`SandboxNeeds`] mit. Der seccomp-Filter (Teil 2 von O10) ist davon
//! unabhängig umgesetzt (`crate::seccomp`).
//!
//! # Der heutige Weg: `nft` als Kindprozess
//! [`NftCommandBackend`] ruft das System-Werkzeug `nft` per
//! [`std::process::Command`] auf. `nft` kann eine cgroup-v2 direkt matchen
//! (`socket cgroupv2 level N "<pfad>"`, Kernel ≥ 5.13, `nft` ≥ 0.9.4) — genau
//! die Durchsetzung „diese cgroup kommt nicht mehr ins Netz“, ohne die Art
//! zu ändern, wie Jobs gestartet werden.
//!
//! # Regelwerk
//! Alle Regeln leben in einer eigenen Tabelle [`NFT_TABLE_FAMILY`]
//! [`NFT_TABLE_NAME`] (`inet harw_warden`); fremde Tabellen werden nie
//! berührt. Je isolierter cgroup entstehen zwei eigene Basisketten
//! (`o_<hex>` am `output`-Hook, `i_<hex>` am `input`-Hook, `<hex>` = die
//! hex-kodierte cgroup-Kennung — kollisionsfrei und immer ein gültiger
//! nft-Bezeichner), jede mit genau einer Regel
//! `socket cgroupv2 level N "<pfad>" counter drop`. Eigene Ketten statt
//! Regeln in einer gemeinsamen Kette machen das Rückgängigmachen
//! ([`NftOperation::Release`]) handle-frei: Kette leeren und löschen, kein
//! Parsen von `nft -a list`-Ausgaben.
//!
//! Jede Operation ist **ein** `nft`-Aufruf mit mehreren, durch `;`
//! getrennten Befehlen — `nft` reicht sie als eine Netlink-Transaktion ein,
//! sie gelten also atomar oder gar nicht. Isolieren ist idempotent
//! (`add table`/`add chain` scheitern nicht an Vorhandenem, `flush chain`
//! verhindert doppelte Regeln), Freigeben ebenso (die Kette wird vor dem
//! Löschen angelegt, falls sie fehlt).
//!
//! # Reine Befehlszeilen
//! Die Argumentvektoren entstehen ausschließlich in reinen Funktionen
//! ([`isolate_command_args`], [`release_command_args`],
//! [`release_all_command_args`]) — Tests prüfen exakte Vektoren, ohne
//! irgendetwas auszuführen. Der Transport ist über [`NetfilterBackend`]
//! austauschbar; Tests benutzen ein aufzeichnendes Backend.
//!
//! # Eingabeprüfung
//! `nft` parst seine Argumente als Skript. Die cgroup-Kennung wird deshalb
//! strenger geprüft als in `CgroupV2Executor`. Sie darf aus mehreren durch
//! `/` getrennten Segmenten bestehen (`harw.slice/job-1`); jedes Segment
//! wird einzeln geprüft: nicht leer, nicht `.` oder `..`, nur
//! ASCII-Alphanumerik und `._-@:+`. Ein führendes, doppeltes oder
//! abschließendes `/` scheitert damit am leeren Segment. Die ganze Kennung
//! samt `/` hat höchstens [`MAX_CGROUP_ID_LEN`] Bytes (Kettennamen sind in
//! nftables auf 255 Byte begrenzt). `level` ist die Zahl der Elternsegmente
//! plus die Zahl der Kennungssegmente. Alles andere ist
//! [`WardenError::InvalidCgroupId`], bevor ein Backend aufgerufen wird.
//!
//! # Fehlerbilder
//! - `nft` fehlt → [`WardenError::Io`] mit `ErrorKind::NotFound` und dem
//!   gesuchten Pfad.
//! - Fehlende Rechte (`CAP_NET_ADMIN`) → `ErrorKind::PermissionDenied`.
//! - cgroup existiert nicht (nft löst den Pfad beim Einfügen auf) →
//!   `ErrorKind::NotFound`.
//! - Nicht-Linux → `ErrorKind::Unsupported`, ohne einen Prozess zu starten.
//! - Netzaktionen durch die Policy abgeschaltet ([`DisabledNetworkIsolator`])
//!   → `ErrorKind::Unsupported`.
//! - `nft` endet nicht innerhalb von [`NFT_TIMEOUT`] → das Kind wird mit
//!   `SIGKILL` beendet und eingesammelt, `ErrorKind::TimedOut`; die Anfrage
//!   bekommt `execution-failed`, die Eingriffssperre wird frei.
//!
//! Die Fehlermeldungen selbst enthalten nie die cgroup-Kennung. Auf
//! `error`-Stufe steht nur, **dass** eine Änderung scheiterte (Backend und
//! Operation); cgroup, Fehlertext und die escapte `nft`-Fehlerausgabe stehen
//! nur auf `debug`.
//!
//! # Laufzeitvoraussetzungen und Sandbox
//! `CAP_NET_ADMIN` für den Warden-Prozess (über die Ambient-Menge an `nft`
//! vererbt). Die Vorgabe-Unit gibt es nicht; das Isolations-Drop-in der
//! Unit ergänzt es, und `main.rs` (`require_net_admin`) verweigert den Start,
//! wenn die Policy Netzaktionen erlaubt, die Capability aber fehlt.
//! Landlock und seccomp vererben sich über `execve` auf `nft`; was das
//! Kindprogramm deshalb zusätzlich braucht, meldet
//! [`NetfilterBackend::sandbox_needs`] ([`SandboxNeeds`]): Lese- und
//! Ausführungsrechte für `nft` samt Laufzeitbibliotheken, `/dev/null` für
//! dessen Standard-Ein- und -Ausgabe und die Systemaufrufe aus
//! `crate::seccomp::nft_subprocess_syscalls`. Ohne erlaubte Netzaktion
//! (Vorgabe) bekommt der Warden nichts davon.
//!
//! # Nebenläufigkeit
//! Alle Typen sind nach dem Bau unveränderlich und `Send + Sync`;
//! gleichzeitige Aufrufe serialisiert der Kernel über die
//! nftables-Transaktionen.
//!
//! # Examples
//! ```rust,ignore
//! // `harw-warden` hat kein `lib`-Target; geprüft in den Tests unten.
//! let isolator = NftNetworkIsolator::from_cgroup_root(Path::new("/sys/fs/cgroup"));
//! let needs = isolator.sandbox_needs();
//! ```

use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use harw_dod_warden::{ActionAllowlist, NetworkIsolator, WardenError, WardenResult};
use harw_types::CgroupId;

use crate::seccomp::AllowedSyscall;

/// Adressfamilie der eigenen nftables-Tabelle (IPv4 und IPv6 gemeinsam).
pub const NFT_TABLE_FAMILY: &str = "inet";

/// Name der eigenen nftables-Tabelle; keine andere Tabelle wird berührt.
pub const NFT_TABLE_NAME: &str = "harw_warden";

/// Vorgabepfad des `nft`-Binarys.
pub const DEFAULT_NFT_BINARY: &str = "/usr/sbin/nft";

/// Einhängepunkt der cgroup-v2-Hierarchie; `nft` erwartet cgroup-Pfade
/// relativ dazu.
pub const CGROUP2_MOUNT: &str = "/sys/fs/cgroup";

/// Längste zulässige cgroup-Kennung in Byte, gemessen über die ganze
/// Kennung samt `/`-Trennern: `2 + 2 * 126 = 254` Zeichen Kettenname passen
/// unter die nftables-Grenze von 255.
pub const MAX_CGROUP_ID_LEN: usize = 126;

/// Die Aktionsarten (Wire-Namen, `WardenAction::kind_name`), für die der
/// Warden ein Netfilter-Backend braucht.
pub const NETWORK_ACTION_KINDS: [&str; 2] = ["isolate-network", "lift-isolation"];

/// `std::process::Stdio::null()` öffnet diese Datei für Standard-Ein- und
/// -Ausgabe des Kindprozesses.
const DEV_NULL: &str = "/dev/null";

/// Obergrenze für die geloggte `nft`-Fehlerausgabe in Zeichen.
#[cfg(target_os = "linux")]
const MAX_LOGGED_STDERR: usize = 2048;

/// Obergrenze für die gelesene `nft`-Fehlerausgabe in Bytes.
#[cfg(target_os = "linux")]
const MAX_READ_STDERR: u64 = 16_384;

/// Längste Laufzeit eines `nft`-Aufrufs. Der Warden hält während des
/// Aufrufs seine Eingriffssperre; ein hängendes `nft` (Netlink-Stau, eine
/// Sandbox-Ablehnung, die es warten lässt) darf weder spätere Anfragen noch
/// den Hold-Timer für immer blockieren.
pub const NFT_TIMEOUT: Duration = Duration::from_secs(10);

/// Abstand, in dem auf das Ende des `nft`-Kindprozesses geprüft wird.
#[cfg(target_os = "linux")]
const NFT_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Ob die Allowlist eine Netzaktion erlaubt (dann braucht der Warden ein
/// Netfilter-Backend und dessen Sandbox-Bedarf).
///
/// # Arguments
/// - `actions` (`&ActionAllowlist`): die Allowlist aus `warden.toml`.
///
/// # Returns
/// `true`, wenn `isolate-network` oder `lift-isolation` erlaubt ist.
#[must_use]
pub fn network_actions_enabled(actions: &ActionAllowlist) -> bool {
    actions
        .kinds()
        .iter()
        .any(|kind| NETWORK_ACTION_KINDS.contains(kind))
}

/// Was ein Netfilter-Backend nach Landlock und seccomp noch braucht.
///
/// # Description
/// Landlock und seccomp gelten prozessweit und vererben sich über `execve`.
/// Ein Backend, das ein Programm startet, braucht deshalb Rechte für dieses
/// Programm; ein Backend im eigenen Prozess nur die Systemaufrufe seines
/// Sockets. Der Warden bindet diese Rechte nur, wenn die Policy eine
/// Netzaktion erlaubt ([`network_actions_enabled`]); sonst bleibt der
/// Bedarf leer ([`SandboxNeeds::default`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SandboxNeeds {
    /// Pfade, die nur gelesen bzw. ausgeführt werden (Landlock
    /// `Execute | ReadFile | ReadDir`, für Dateien beschnitten).
    pub read_exec_paths: Vec<PathBuf>,
    /// Einzelne Dateien, die gelesen und geschrieben werden (Landlock, für
    /// Dateien beschnitten auf `ReadFile | WriteFile | Execute`).
    pub read_write_files: Vec<PathBuf>,
    /// Zusätzliche Systemaufrufe für die seccomp-Allowlist (auch für
    /// Kindprozesse, die den Filter erben).
    pub syscalls: Vec<AllowedSyscall>,
}

/// Eine nftables-Änderung der eigenen Tabelle, unabhängig vom Transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NftOperation<'a> {
    /// Isoliert die cgroup (idempotent, eine Transaktion).
    Isolate(&'a NftCgroupTarget),
    /// Hebt die Isolation der cgroup auf (idempotent, eine Transaktion).
    Release(&'a NftCgroupTarget),
}

/// Transport für nftables-Änderungen (PL-100 Entscheidung O10).
///
/// # Description
/// Bekommt nur bereits geprüfte Ziele ([`NftCgroupTarget`]); jede
/// Operation muss als **eine** nftables-Transaktion wirken (ganz oder gar
/// nicht). Implementierungen: [`NftCommandBackend`] (produktiv) und das
/// aufzeichnende Backend der Tests.
///
/// # Concurrency
/// Implementierungen müssen `Send + Sync` sein, damit der Isolator in
/// `harw_dod_warden::WardenParts` passt.
pub trait NetfilterBackend {
    /// Stabiler Kurzname für Logs.
    ///
    /// # Returns
    /// Z. B. `"nft-subprocess"`.
    fn name(&self) -> &'static str;

    /// Reicht `operation` als eine nftables-Transaktion ein.
    ///
    /// # Arguments
    /// - `operation` (`NftOperation`): die Änderung samt geprüftem Ziel.
    ///
    /// # Errors
    /// [`WardenError::Io`] mit einer kennungsfreien Meldung.
    fn apply(&self, operation: NftOperation<'_>) -> WardenResult<()>;

    /// Was dieses Backend nach Landlock und seccomp noch braucht.
    ///
    /// # Returns
    /// Den Bedarf (siehe [`SandboxNeeds`]).
    fn sandbox_needs(&self) -> SandboxNeeds;
}

/// Aufgelöstes Ziel einer Isolation: cgroup-Pfad relativ zum
/// cgroup-v2-Einhängepunkt, dessen Tiefe und der Stamm der Kettennamen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NftCgroupTarget {
    path: String,
    level: usize,
    chain_stem: String,
}

impl NftCgroupTarget {
    /// Baut das Ziel aus Elternsegmenten und der cgroup-Kennung.
    ///
    /// # Arguments
    /// - `parent` (`&[String]`): bereits geprüfte Segmente zwischen
    ///   Einhängepunkt und cgroup (leer: die Kennung ist relativ zum
    ///   Einhängepunkt).
    /// - `cgroup` (`&CgroupId`): die zu isolierende cgroup; auch ein
    ///   mehrstufiger relativer Pfad wie `harw.slice/job-1`.
    ///
    /// # Returns
    /// Das Ziel.
    ///
    /// # Errors
    /// [`WardenError::InvalidCgroupId`], wenn ein Segment der Kennung oder
    /// ein Elternsegment unzulässig ist (leer, `.`/`..`, unzulässige
    /// Zeichen) oder die ganze Kennung länger als [`MAX_CGROUP_ID_LEN`]
    /// Bytes ist.
    pub fn resolve(parent: &[String], cgroup: &CgroupId) -> WardenResult<Self> {
        let id = cgroup.as_str();
        if id.len() > MAX_CGROUP_ID_LEN || !id.split('/').all(is_safe_segment) {
            return Err(WardenError::InvalidCgroupId);
        }
        if !parent.iter().all(|segment| is_safe_segment(segment)) {
            return Err(WardenError::InvalidCgroupId);
        }
        let mut segments: Vec<&str> = parent.iter().map(String::as_str).collect();
        segments.extend(id.split('/'));
        Ok(Self {
            path: segments.join("/"),
            level: segments.len(),
            // Hex über die ganze Kennung samt `/`: bleibt injektiv, und die
            // Kettennamen einstufiger Kennungen ändern sich nicht.
            chain_stem: hex_encode(id.as_bytes()),
        })
    }

    /// Pfad relativ zum cgroup-v2-Einhängepunkt.
    ///
    /// # Returns
    /// Den Pfad, z. B. `harw.slice/job-1`.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Tiefe des Pfads (Anzahl Segmente) für `socket cgroupv2 level N`.
    ///
    /// # Returns
    /// Die Tiefe (≥ 1).
    #[must_use]
    pub fn level(&self) -> usize {
        self.level
    }

    // Name der Basiskette am `output`-Hook.
    fn output_chain(&self) -> String {
        format!("o_{}", self.chain_stem)
    }

    // Name der Basiskette am `input`-Hook.
    fn input_chain(&self) -> String {
        format!("i_{}", self.chain_stem)
    }
}

/// Prüft ein Pfadsegment auf die für ein `nft`-Skript sichere Zeichenmenge.
fn is_safe_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'@' | b':' | b'+')
        })
}

/// Kodiert Bytes als Kleinbuchstaben-Hex.
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for &byte in bytes {
        for nibble in [byte >> 4, byte & 0x0f] {
            // `from_digit` mit Basis 16 liefert für 0..=15 immer eine Ziffer.
            if let Some(digit) = char::from_digit(u32::from(nibble), 16) {
                out.push(digit);
            }
        }
    }
    out
}

/// Leitet die Elternsegmente zwischen [`CGROUP2_MOUNT`] und `cgroup_root` ab.
///
/// `None`, wenn die Wurzel nicht darunter liegt oder ein Segment unzulässig
/// ist — jede Isolation scheitert dann, ohne ein Backend aufzurufen.
fn parent_segments(cgroup_root: &Path) -> Option<Vec<String>> {
    cgroup_root
        .strip_prefix(CGROUP2_MOUNT)
        .ok()
        .and_then(|relative| {
            relative
                .components()
                .map(|component| match component {
                    Component::Normal(os) => os
                        .to_str()
                        .filter(|segment| is_safe_segment(segment))
                        .map(str::to_owned),
                    _ => None,
                })
                .collect::<Option<Vec<String>>>()
        })
}

/// Hängt Tokens an einen Argumentvektor an.
fn push_all(args: &mut Vec<String>, tokens: &[&str]) {
    args.extend(tokens.iter().map(|t| (*t).to_owned()));
}

/// `add table inet harw_warden ;`
fn push_add_table(args: &mut Vec<String>) {
    push_all(
        args,
        &["add", "table", NFT_TABLE_FAMILY, NFT_TABLE_NAME, ";"],
    );
}

/// `add chain inet harw_warden <chain> { type filter hook <hook> priority 0 ; policy accept ; } ;`
fn push_add_base_chain(args: &mut Vec<String>, chain: &str, hook: &str) {
    push_all(
        args,
        &["add", "chain", NFT_TABLE_FAMILY, NFT_TABLE_NAME, chain],
    );
    push_all(
        args,
        &[
            "{", "type", "filter", "hook", hook, "priority", "0", ";", "policy", "accept", ";",
            "}", ";",
        ],
    );
}

/// `<verb> chain inet harw_warden <chain> ;`
fn push_chain_cmd(args: &mut Vec<String>, verb: &str, chain: &str) {
    push_all(
        args,
        &[verb, "chain", NFT_TABLE_FAMILY, NFT_TABLE_NAME, chain, ";"],
    );
}

/// Argumentvektor (ohne Programmnamen) für das Isolieren einer cgroup.
///
/// # Description
/// Eine Transaktion: Tabelle anlegen, je Hook (`output`, `input`) eine
/// eigene Basiskette anlegen, leeren und mit genau einer Drop-Regel für die
/// cgroup füllen. Idempotent.
///
/// # Arguments
/// - `target` (`&NftCgroupTarget`): das aufgelöste Ziel.
///
/// # Returns
/// Die Argumente in Aufrufreihenfolge.
#[must_use]
pub fn isolate_command_args(target: &NftCgroupTarget) -> Vec<String> {
    let mut args = Vec::new();
    push_add_table(&mut args);
    let level = target.level().to_string();
    let quoted_path = format!("\"{}\"", target.path());
    let chains = [
        (target.output_chain(), "output"),
        (target.input_chain(), "input"),
    ];
    for (index, (chain, hook)) in chains.iter().enumerate() {
        push_add_base_chain(&mut args, chain, hook);
        push_chain_cmd(&mut args, "flush", chain);
        push_all(
            &mut args,
            &[
                "add",
                "rule",
                NFT_TABLE_FAMILY,
                NFT_TABLE_NAME,
                chain,
                "socket",
                "cgroupv2",
                "level",
                &level,
                &quoted_path,
                "counter",
                "drop",
            ],
        );
        if index.saturating_add(1) < chains.len() {
            args.push(";".to_owned());
        }
    }
    args
}

/// Argumentvektor (ohne Programmnamen) für das Aufheben der Isolation einer
/// cgroup.
///
/// # Description
/// Eine Transaktion: Tabelle und beide Ketten anlegen (falls sie fehlen,
/// damit das Löschen nicht scheitert), leeren, löschen. Idempotent; berührt
/// die Ketten anderer cgroups nicht.
///
/// # Arguments
/// - `target` (`&NftCgroupTarget`): das aufgelöste Ziel.
///
/// # Returns
/// Die Argumente in Aufrufreihenfolge.
#[must_use]
pub fn release_command_args(target: &NftCgroupTarget) -> Vec<String> {
    let mut args = Vec::new();
    push_add_table(&mut args);
    let chains = [
        (target.output_chain(), "output"),
        (target.input_chain(), "input"),
    ];
    for (chain, hook) in &chains {
        push_add_base_chain(&mut args, chain, hook);
        push_chain_cmd(&mut args, "flush", chain);
        push_chain_cmd(&mut args, "delete", chain);
    }
    // Das abschließende `;` ist überflüssig — entfernen, damit die
    // Befehlszeile mit einem Befehl endet.
    if args.last().is_some_and(|last| last == ";") {
        args.pop();
    }
    args
}

/// Argumentvektor (ohne Programmnamen), der die gesamte eigene Tabelle
/// entfernt und damit jede Isolation aufhebt.
///
/// # Returns
/// `add table inet harw_warden ; delete table inet harw_warden`.
#[cfg(test)]
#[must_use]
pub fn release_all_command_args() -> Vec<String> {
    let mut args = Vec::new();
    push_add_table(&mut args);
    push_all(
        &mut args,
        &["delete", "table", NFT_TABLE_FAMILY, NFT_TABLE_NAME],
    );
    args
}

/// Ordnet einen fehlgeschlagenen `nft`-Lauf einem `io::Error` mit klarer,
/// kennungsfreier Meldung zu.
///
/// # Arguments
/// - `status` (`Option<i32>`): Exit-Code (`None` bei Signalende).
/// - `stderr` (`&str`): Fehlerausgabe von `nft`.
///
/// # Returns
/// Den passenden Fehler.
#[must_use]
pub fn classify_nft_failure(status: Option<i32>, stderr: &str) -> io::Error {
    if stderr.contains("Operation not permitted") || stderr.contains("Permission denied") {
        return io::Error::new(
            io::ErrorKind::PermissionDenied,
            "nft refused the ruleset change: insufficient privileges (CAP_NET_ADMIN required)",
        );
    }
    if stderr.contains("No such file or directory") {
        return io::Error::new(
            io::ErrorKind::NotFound,
            "nft could not resolve the cgroup path (cgroup does not exist or cgroupv2 matching unsupported)",
        );
    }
    let status = status.map_or_else(|| "a signal".to_owned(), |code| format!("status {code}"));
    io::Error::other(format!("nft failed with {status}"))
}

/// Produktives Netfilter-Backend: das `nft`-Programm als Kindprozess.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Der heutige Weg“. Hält nur den Pfad zum
/// `nft`-Binary. Der Kindprozess erbt Landlock und seccomp; sein Bedarf
/// steht in [`NetfilterBackend::sandbox_needs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NftCommandBackend {
    nft_binary: PathBuf,
    timeout: Duration,
}

impl Default for NftCommandBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NftCommandBackend {
    /// Baut das Backend für [`DEFAULT_NFT_BINARY`].
    ///
    /// # Returns
    /// Das Backend.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nft_binary: PathBuf::from(DEFAULT_NFT_BINARY),
            timeout: NFT_TIMEOUT,
        }
    }

    /// Baut das Backend für ein anderes `nft`-Binary (Tests).
    ///
    /// # Arguments
    /// - `nft_binary` (`impl Into<PathBuf>`): absoluter Pfad oder Name zur
    ///   `PATH`-Suche (ein Name bekommt keine Landlock-Regel).
    ///
    /// # Returns
    /// Das Backend.
    #[cfg(test)]
    #[must_use]
    pub fn with_binary(nft_binary: impl Into<PathBuf>) -> Self {
        Self {
            nft_binary: nft_binary.into(),
            timeout: NFT_TIMEOUT,
        }
    }

    /// Pfad zum verwendeten `nft`-Binary.
    ///
    /// # Returns
    /// Den Pfad.
    #[cfg(test)]
    #[must_use]
    pub fn nft_binary(&self) -> &Path {
        &self.nft_binary
    }

    /// Ersetzt die Frist eines `nft`-Aufrufs (Tests).
    ///
    /// # Arguments
    /// - `timeout` (`Duration`): die neue Frist.
    ///
    /// # Returns
    /// Das geänderte Backend.
    #[cfg(test)]
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Die Frist eines `nft`-Aufrufs.
    ///
    /// # Returns
    /// Die Frist.
    #[cfg(test)]
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Führt `nft` mit den gegebenen Argumenten aus, höchstens
    /// [`NFT_TIMEOUT`] lang.
    ///
    /// # Description
    /// Startet das Kind, wartet mit Frist ([`wait_or_kill`]) und liest erst
    /// danach dessen Fehlerausgabe (höchstens [`MAX_READ_STDERR`] Bytes).
    /// Schreibt `nft` mehr als ein Pipe-Puffer fasst, blockiert es und läuft
    /// in die Frist — `nft` meldet Fehler in wenigen Zeilen.
    ///
    /// # Errors
    /// [`WardenError::Io`]: Start scheitert, Frist überschritten
    /// (`ErrorKind::TimedOut`, das Kind ist dann beendet und eingesammelt)
    /// oder `nft` meldet einen Fehler (siehe [`classify_nft_failure`]).
    #[cfg(target_os = "linux")]
    fn run(&self, args: &[String]) -> WardenResult<()> {
        use std::io::Read as _;
        use std::process::{Command, Stdio};

        let mut child = Command::new(&self.nft_binary)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| self.spawn_error(&err))?;
        let status = wait_or_kill(&mut child, self.timeout).map_err(|error| {
            tracing::error!(kind = ?error.kind(), "nft invocation did not complete");
            WardenError::Io(error)
        })?;
        if status.success() {
            return Ok(());
        }
        let mut stderr_bytes = Vec::new();
        if let Some(stderr) = child.stderr.take()
            && let Err(error) = stderr.take(MAX_READ_STDERR).read_to_end(&mut stderr_bytes)
        {
            tracing::debug!(error = %error, "could not read the nft failure output");
        }
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        let logged: String = stderr.chars().take(MAX_LOGGED_STDERR).collect();
        tracing::error!(status = ?status.code(), "nft invocation failed");
        // Die `nft`-Ausgabe kann die cgroup nennen: nur auf `debug`, über
        // `Debug` (Steuerzeichen escapt).
        tracing::debug!(
            nft = %self.nft_binary.display(),
            stderr = ?logged.trim_end(),
            "nft failure output"
        );
        Err(WardenError::Io(classify_nft_failure(
            status.code(),
            &stderr,
        )))
    }

    /// Nicht-Linux: nftables existiert nicht; nie einen Prozess starten.
    #[cfg(not(target_os = "linux"))]
    fn run(&self, _args: &[String]) -> WardenResult<()> {
        Err(WardenError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "network isolation via nftables is only supported on Linux",
        )))
    }

    /// Übersetzt einen Startfehler des `nft`-Prozesses in eine klare Meldung.
    #[cfg(target_os = "linux")]
    fn spawn_error(&self, err: &io::Error) -> WardenError {
        let path = self.nft_binary.display();
        let error = match err.kind() {
            io::ErrorKind::NotFound => io::Error::new(
                io::ErrorKind::NotFound,
                format!("nft binary not found at {path} (is nftables installed?)"),
            ),
            io::ErrorKind::PermissionDenied => io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("not permitted to execute nft binary at {path}"),
            ),
            kind => io::Error::new(
                kind,
                format!("failed to execute nft binary at {path}: {err}"),
            ),
        };
        WardenError::Io(error)
    }
}

/// Waits for `child` at most `timeout`; on timeout kills and reaps it.
///
/// # Description
/// Fragt alle [`NFT_POLL_INTERVAL`] nicht blockierend nach dem Ende
/// (`waitpid(WNOHANG)`). Ist die Frist um, wird das Kind mit `SIGKILL`
/// beendet und eingesammelt (kein Zombie) — seccomp erlaubt dafür
/// `kill` nur mit `SIGKILL` (`crate::seccomp::nft_subprocess_syscalls`).
///
/// # Arguments
/// - `child` (`&mut Child`): der laufende Kindprozess.
/// - `timeout` (`Duration`): die Frist.
///
/// # Returns
/// Den Exit-Status, wenn das Kind rechtzeitig endete.
///
/// # Errors
/// `ErrorKind::TimedOut` nach Ablauf der Frist; jeder Fehler von
/// `try_wait` (das Kind wird dann ebenfalls beendet und eingesammelt).
#[cfg(target_os = "linux")]
fn wait_or_kill(
    child: &mut std::process::Child,
    timeout: Duration,
) -> io::Result<std::process::ExitStatus> {
    let outcome = wait_with_deadline(child, timeout);
    match outcome {
        Ok(Some(status)) => Ok(status),
        Ok(None) => {
            kill_and_reap(child);
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "nft did not finish within the time limit",
            ))
        }
        Err(error) => {
            kill_and_reap(child);
            Err(error)
        }
    }
}

// Wartet höchstens `timeout`; `Ok(None)`, wenn das Kind dann noch läuft.
#[cfg(target_os = "linux")]
fn wait_with_deadline(
    child: &mut std::process::Child,
    timeout: Duration,
) -> io::Result<Option<std::process::ExitStatus>> {
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if started.elapsed() >= timeout {
            return Ok(None);
        }
        std::thread::sleep(NFT_POLL_INTERVAL);
    }
}

// Beendet ein hängendes Kind und sammelt es ein; Fehler nur auf `debug`
// (das Kind kann inzwischen von selbst geendet haben).
#[cfg(target_os = "linux")]
fn kill_and_reap(child: &mut std::process::Child) {
    if let Err(error) = child.kill() {
        tracing::debug!(error = %error, "could not kill the nft child");
    }
    if let Err(error) = child.wait() {
        tracing::debug!(error = %error, "could not reap the nft child");
    }
}

impl NetfilterBackend for NftCommandBackend {
    fn name(&self) -> &'static str {
        "nft-subprocess"
    }

    fn apply(&self, operation: NftOperation<'_>) -> WardenResult<()> {
        match operation {
            NftOperation::Isolate(target) => self.run(&isolate_command_args(target)),
            NftOperation::Release(target) => self.run(&release_command_args(target)),
        }
    }

    /// `nft` samt Lader und Bibliotheken nur lesend/ausführend
    /// ([`crate::landlock::read_exec_paths`]), `/dev/null` lesend und
    /// schreibend (Standard-Ein- und -Ausgabe des Kindes) und die
    /// Systemaufrufe für Prozessstart und das `nft`-Programm
    /// ([`crate::seccomp::nft_subprocess_syscalls`]).
    fn sandbox_needs(&self) -> SandboxNeeds {
        if !self.nft_binary.is_absolute() {
            tracing::debug!(nft = %self.nft_binary.display(), "configured nft binary path");
            tracing::warn!(
                "nft binary path is not absolute; no landlock rule for it, isolation will likely fail"
            );
        }
        SandboxNeeds {
            read_exec_paths: crate::landlock::read_exec_paths(&self.nft_binary),
            read_write_files: vec![PathBuf::from(DEV_NULL)],
            syscalls: crate::seccomp::nft_subprocess_syscalls(),
        }
    }
}

/// Isoliert cgroups über ein [`NetfilterBackend`] (Vorgabe:
/// [`NftCommandBackend`]).
///
/// # Description
/// Prüft und löst das Ziel auf ([`NftCgroupTarget::resolve`]) und reicht
/// erst dann eine [`NftOperation`] an das Backend. Hält nur das Backend und
/// die Elternsegmente zwischen cgroup-v2-Einhängepunkt und den verwalteten
/// cgroups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NftNetworkIsolator<B = NftCommandBackend> {
    backend: B,
    /// `None`: die konfigurierte cgroup-Wurzel liegt nicht unter
    /// [`CGROUP2_MOUNT`] oder enthält unzulässige Segmente — jede Isolation
    /// scheitert dann, ohne das Backend aufzurufen.
    cgroup_parent: Option<Vec<String>>,
}

impl Default for NftNetworkIsolator {
    fn default() -> Self {
        Self::new()
    }
}

impl NftNetworkIsolator {
    /// Baut einen Isolator für cgroup-Pfade relativ zu [`CGROUP2_MOUNT`]
    /// mit [`NftCommandBackend::new`].
    ///
    /// # Returns
    /// Den Isolator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            backend: NftCommandBackend::new(),
            cgroup_parent: Some(Vec::new()),
        }
    }

    /// Baut den produktiven Isolator passend zur `--cgroup-root` des Wardens.
    ///
    /// # Description
    /// Wie [`Self::with_backend`] mit [`NftCommandBackend::new`].
    ///
    /// # Arguments
    /// - `cgroup_root` (`&Path`): dieselbe Wurzel wie für
    ///   `CgroupV2Executor`.
    ///
    /// # Returns
    /// Den Isolator.
    #[must_use]
    pub fn from_cgroup_root(cgroup_root: &Path) -> Self {
        Self::with_backend(NftCommandBackend::new(), cgroup_root)
    }

    /// Ersetzt den Pfad zum `nft`-Binary (Tests).
    ///
    /// # Arguments
    /// - `nft_binary` (`impl Into<PathBuf>`): absoluter Pfad oder Name zur
    ///   `PATH`-Suche.
    ///
    /// # Returns
    /// Den geänderten Isolator.
    #[cfg(test)]
    #[must_use]
    pub fn with_nft_binary(mut self, nft_binary: impl Into<PathBuf>) -> Self {
        self.backend = NftCommandBackend::with_binary(nft_binary);
        self
    }

    /// Pfad zum verwendeten `nft`-Binary.
    ///
    /// # Returns
    /// Den Pfad.
    #[cfg(test)]
    #[must_use]
    pub fn nft_binary(&self) -> &Path {
        self.backend.nft_binary()
    }

    /// Entfernt die gesamte eigene Tabelle und damit jede Isolation.
    ///
    /// # Returns
    /// `Ok(())`, auch wenn die Tabelle nicht existierte.
    ///
    /// # Errors
    /// [`WardenError::Io`]: `nft` fehlt, Rechte fehlen, Nicht-Linux oder
    /// ein sonstiger `nft`-Fehlschlag.
    #[cfg(test)]
    pub fn release_all(&self) -> WardenResult<()> {
        self.backend.run(&release_all_command_args())
    }
}

impl<B: NetfilterBackend> NftNetworkIsolator<B> {
    /// Baut einen Isolator mit einem beliebigen Backend.
    ///
    /// # Description
    /// `nft` und der Kernel erwarten cgroup-Pfade relativ zu
    /// [`CGROUP2_MOUNT`]; die Wurzel wird deshalb davon abgezogen. Liegt sie
    /// nicht darunter (z. B. ein Test-Fixture), scheitert jede spätere
    /// Isolation mit `ErrorKind::InvalidInput`, ohne das Backend
    /// aufzurufen.
    ///
    /// # Arguments
    /// - `backend` (`B`): der Transport.
    /// - `cgroup_root` (`&Path`): dieselbe Wurzel wie für
    ///   `CgroupV2Executor`.
    ///
    /// # Returns
    /// Den Isolator.
    #[must_use]
    pub fn with_backend(backend: B, cgroup_root: &Path) -> Self {
        Self {
            backend,
            cgroup_parent: parent_segments(cgroup_root),
        }
    }

    /// Was das Backend nach Landlock und seccomp noch braucht.
    ///
    /// # Returns
    /// Siehe [`NetfilterBackend::sandbox_needs`].
    #[must_use]
    pub fn sandbox_needs(&self) -> SandboxNeeds {
        self.backend.sandbox_needs()
    }

    /// Löst die cgroup zum nft-Ziel auf.
    ///
    /// # Errors
    /// - [`WardenError::Io`] (`InvalidInput`): cgroup-Wurzel nicht unter
    ///   [`CGROUP2_MOUNT`].
    /// - [`WardenError::InvalidCgroupId`]: siehe [`NftCgroupTarget::resolve`].
    fn target(&self, cgroup: &CgroupId) -> WardenResult<NftCgroupTarget> {
        let Some(parent) = &self.cgroup_parent else {
            return Err(WardenError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "cgroup root is not below {CGROUP2_MOUNT}; nftables cgroupv2 matching impossible"
                ),
            )));
        };
        NftCgroupTarget::resolve(parent, cgroup)
    }

    /// Hebt die Isolation einer cgroup wieder auf.
    ///
    /// # Arguments
    /// - `cgroup` (`&CgroupId`): die freizugebende cgroup.
    ///
    /// # Returns
    /// `Ok(())`, auch wenn die cgroup gar nicht isoliert war.
    ///
    /// # Errors
    /// Wie [`NetworkIsolator::isolate`] dieser Implementierung.
    pub fn release(&self, cgroup: &CgroupId) -> WardenResult<()> {
        let target = self.target(cgroup)?;
        self.backend
            .apply(NftOperation::Release(&target))
            .inspect_err(|error| self.log_failure("release", cgroup, error))
    }

    // Fehlschlag eines Backends: Stufe `error` ohne Kennung, Details auf
    // `debug`.
    fn log_failure(&self, operation: &'static str, cgroup: &CgroupId, error: &WardenError) {
        tracing::error!(
            backend = self.backend.name(),
            operation = operation,
            "network isolation change failed"
        );
        tracing::debug!(
            cgroup = %cgroup.as_str(),
            error = %error,
            "network isolation failure detail"
        );
    }
}

impl<B: NetfilterBackend> NetworkIsolator for NftNetworkIsolator<B> {
    /// Trennt die cgroup vom Netz (ein- und ausgehend).
    ///
    /// # Arguments
    /// - `cgroup` (`&CgroupId`): die zu isolierende cgroup; sie muss
    ///   existieren, weil `nft` den Pfad beim Einfügen auflöst.
    ///
    /// # Returns
    /// `Ok(())`, sobald die Regeln aktiv sind.
    ///
    /// # Errors
    /// - [`WardenError::InvalidCgroupId`]: unzulässige Kennung (kein
    ///   Backend-Aufruf).
    /// - [`WardenError::Io`]: Wurzel nicht unter [`CGROUP2_MOUNT`]
    ///   (`InvalidInput`, kein Backend-Aufruf) oder ein Fehler des Backends
    ///   (siehe Moduldoku, „Fehlerbilder“).
    fn isolate(&self, cgroup: &CgroupId) -> WardenResult<()> {
        let target = self.target(cgroup)?;
        self.backend
            .apply(NftOperation::Isolate(&target))
            .inspect_err(|error| self.log_failure("isolate", cgroup, error))
    }

    /// Entfernt die Isolations-Chains dieser cgroup (idempotent).
    fn release_isolation(&self, cgroup: &CgroupId) -> WardenResult<()> {
        self.release(cgroup)
    }
}

/// Isolator, solange die Policy keine Netzaktion erlaubt.
///
/// # Description
/// Die Policy lehnt `isolate-network`/`lift-isolation` dann schon vor jeder
/// Ausführung ab; dieser Isolator ist die zweite Linie: er startet nichts
/// und braucht deshalb weder `nft` noch `CAP_NET_ADMIN` noch zusätzliche
/// Systemaufrufe. Jeder Aufruf scheitert mit `ErrorKind::Unsupported`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DisabledNetworkIsolator;

impl DisabledNetworkIsolator {
    // Der feste, kennungsfreie Fehler.
    fn refused() -> WardenError {
        WardenError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "network actions are disabled by the warden policy",
        ))
    }
}

impl NetworkIsolator for DisabledNetworkIsolator {
    fn isolate(&self, _cgroup: &CgroupId) -> WardenResult<()> {
        Err(Self::refused())
    }

    fn release_isolation(&self, _cgroup: &CgroupId) -> WardenResult<()> {
        Err(Self::refused())
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use super::{
        DisabledNetworkIsolator, NETWORK_ACTION_KINDS, NetfilterBackend, NftCgroupTarget,
        NftCommandBackend, NftNetworkIsolator, NftOperation, SandboxNeeds, classify_nft_failure,
        isolate_command_args, network_actions_enabled, release_all_command_args,
        release_command_args,
    };
    use crate::test_support::{TestError, TestResult, ctx, private_tempdir};
    use harw_dod_warden::{
        ActionAllowlist, ContainmentMarkers, DenyList, NetworkIsolator, RecordingAuditSink,
        RecordingExecutor, StaticTargetObserver, Warden, WardenError, WardenOutcome, WardenParts,
        WardenPolicy, WardenResult, WardenState,
    };
    use harw_dod_warden_proto::{
        AuthorizationBody, CgroupKernelId, CgroupTarget, Denial, EscalationStage, KeyId, KeyRing,
        ProofKey, ProofPolicy, SignedAuthorization, WardenAction, WardenRequest, WardenResponse,
    };
    use harw_types::{ApprovalActor, CgroupId, FindingId};

    fn cgroup(id: &str) -> TestResult<CgroupId> {
        CgroupId::try_from_str(id).map_err(ctx("non-empty id"))
    }

    fn words(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    fn io_kind(result: Result<(), WardenError>) -> TestResult<ErrorKind> {
        match result {
            Err(WardenError::Io(err)) => Ok(err.kind()),
            Err(other) => Err(TestError::Unexpected(format!("expected Io, got {other}"))),
            Ok(()) => Err(TestError::Unexpected("expected an error".to_owned())),
        }
    }

    /// Aufzeichnendes Backend: merkt sich jede Operation (Klone teilen das
    /// Protokoll) und scheitert auf Wunsch.
    #[derive(Debug, Clone, Default)]
    struct RecordingBackend {
        applied: Arc<Mutex<Vec<String>>>,
        fail_with: Option<ErrorKind>,
    }

    impl RecordingBackend {
        fn failing(kind: ErrorKind) -> Self {
            Self {
                applied: Arc::default(),
                fail_with: Some(kind),
            }
        }

        fn applied(&self) -> TestResult<Vec<String>> {
            self.applied
                .lock()
                .map(|log| log.to_vec())
                .map_err(|_| TestError::Unexpected("recording backend poisoned".to_owned()))
        }
    }

    impl NetfilterBackend for RecordingBackend {
        fn name(&self) -> &'static str {
            "recording"
        }

        fn apply(&self, operation: NftOperation<'_>) -> WardenResult<()> {
            let entry = match operation {
                NftOperation::Isolate(target) => {
                    format!("isolate {} level {}", target.path(), target.level())
                }
                NftOperation::Release(target) => {
                    format!("release {} level {}", target.path(), target.level())
                }
            };
            self.applied
                .lock()
                .map_err(|_| WardenError::Io(std::io::Error::other("recording log poisoned")))?
                .push(entry);
            match self.fail_with {
                Some(kind) => Err(WardenError::Io(std::io::Error::from(kind))),
                None => Ok(()),
            }
        }

        fn sandbox_needs(&self) -> SandboxNeeds {
            SandboxNeeds::default()
        }
    }

    #[test]
    fn test_isolate_args_for_top_level_cgroup() -> TestResult {
        let target =
            NftCgroupTarget::resolve(&[], &cgroup("cgroup-1")?).map_err(ctx("valid id"))?;
        assert_eq!(target.path(), "cgroup-1");
        assert_eq!(target.level(), 1);
        let expected = words(concat!(
            "add table inet harw_warden ; ",
            "add chain inet harw_warden o_6367726f75702d31 { type filter hook output priority 0 ; policy accept ; } ; ",
            "flush chain inet harw_warden o_6367726f75702d31 ; ",
            "add rule inet harw_warden o_6367726f75702d31 socket cgroupv2 level 1 \"cgroup-1\" counter drop ; ",
            "add chain inet harw_warden i_6367726f75702d31 { type filter hook input priority 0 ; policy accept ; } ; ",
            "flush chain inet harw_warden i_6367726f75702d31 ; ",
            "add rule inet harw_warden i_6367726f75702d31 socket cgroupv2 level 1 \"cgroup-1\" counter drop"
        ));
        assert_eq!(isolate_command_args(&target), expected);
        Ok(())
    }

    #[test]
    fn test_isolate_args_for_nested_cgroup_use_full_path_and_level() -> TestResult {
        let isolator = NftNetworkIsolator::from_cgroup_root(Path::new("/sys/fs/cgroup/harw.slice"));
        let target = isolator
            .target(&cgroup("job-1")?)
            .map_err(ctx("valid id"))?;
        assert_eq!(target.path(), "harw.slice/job-1");
        assert_eq!(target.level(), 2);
        let args = isolate_command_args(&target);
        let rule_tail = words("socket cgroupv2 level 2 \"harw.slice/job-1\" counter drop");
        assert!(
            args.windows(rule_tail.len())
                .any(|w| w == rule_tail.as_slice())
        );
        Ok(())
    }

    #[test]
    fn test_isolate_args_for_multi_segment_id_use_full_path_and_level() -> TestResult {
        let target = NftCgroupTarget::resolve(&[], &cgroup("harw.slice/job-1")?)
            .map_err(ctx("valid multi-segment id"))?;
        assert_eq!(target.path(), "harw.slice/job-1");
        assert_eq!(target.level(), 2);
        let args = isolate_command_args(&target);
        let rule_tail = words("socket cgroupv2 level 2 \"harw.slice/job-1\" counter drop");
        assert!(
            args.windows(rule_tail.len())
                .any(|w| w == rule_tail.as_slice())
        );
        // Kettenstamm = Hex der ganzen Kennung samt `/`.
        let output_chain = "o_686172772e736c6963652f6a6f622d31";
        assert!(args.iter().any(|arg| arg == output_chain));
        // Freigeben trifft dieselbe Kette wie das Isolieren.
        let delete = words("delete chain inet harw_warden o_686172772e736c6963652f6a6f622d31");
        assert!(
            release_command_args(&target)
                .windows(delete.len())
                .any(|w| w == delete.as_slice())
        );
        Ok(())
    }

    #[test]
    fn test_multi_segment_id_below_parent_adds_levels() -> TestResult {
        let isolator = NftNetworkIsolator::from_cgroup_root(Path::new("/sys/fs/cgroup/harw.slice"));
        let target = isolator
            .target(&cgroup("job-1.slice/step-2")?)
            .map_err(ctx("valid multi-segment id"))?;
        assert_eq!(target.path(), "harw.slice/job-1.slice/step-2");
        assert_eq!(target.level(), 3);
        Ok(())
    }

    #[test]
    fn test_release_args_flush_and_delete_both_chains() -> TestResult {
        let target = NftCgroupTarget::resolve(&[], &cgroup("a")?).map_err(ctx("valid id"))?;
        let expected = words(concat!(
            "add table inet harw_warden ; ",
            "add chain inet harw_warden o_61 { type filter hook output priority 0 ; policy accept ; } ; ",
            "flush chain inet harw_warden o_61 ; ",
            "delete chain inet harw_warden o_61 ; ",
            "add chain inet harw_warden i_61 { type filter hook input priority 0 ; policy accept ; } ; ",
            "flush chain inet harw_warden i_61 ; ",
            "delete chain inet harw_warden i_61"
        ));
        assert_eq!(release_command_args(&target), expected);
        Ok(())
    }

    #[test]
    fn test_release_all_args_delete_only_the_own_table() {
        assert_eq!(
            release_all_command_args(),
            words("add table inet harw_warden ; delete table inet harw_warden")
        );
    }

    #[test]
    fn test_hex_encode_is_lowercase_and_covers_every_byte() {
        assert_eq!(super::hex_encode(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
        assert_eq!(super::hex_encode(&[]), "");
    }

    #[test]
    fn test_unsafe_ids_are_rejected_before_any_process_starts() -> TestResult {
        // Absichtlich nicht existierendes Binary: würde je ein Prozess
        // gestartet, käme `Io`, nicht `InvalidCgroupId`.
        let isolator = NftNetworkIsolator::new().with_nft_binary("/nonexistent/harw-test-nft");
        let too_long = "a".repeat(super::MAX_CGROUP_ID_LEN + 1);
        // Die Grenze gilt für die ganze Kennung samt `/` (127 Byte).
        let too_long_multi = format!("{}/b", "a".repeat(super::MAX_CGROUP_ID_LEN - 1));
        for id in [
            "/abs",
            "a//b",
            "a/",
            "harw.slice/../x",
            "harw.slice/./x",
            "harw.slice/a b",
            "..",
            ".",
            "a\"b",
            "a b",
            "a;b",
            "a\\b",
            too_long.as_str(),
            too_long_multi.as_str(),
        ] {
            let result = isolator.isolate(&cgroup(id)?);
            assert!(
                matches!(result, Err(WardenError::InvalidCgroupId)),
                "id {id:?} must be rejected"
            );
        }
        Ok(())
    }

    #[test]
    fn test_root_outside_cgroup2_mount_fails_with_invalid_input() -> TestResult {
        let isolator = NftNetworkIsolator::from_cgroup_root(Path::new("/tmp/fixture"))
            .with_nft_binary("/nonexistent/harw-test-nft");
        assert_eq!(
            io_kind(isolator.isolate(&cgroup("job-1")?))?,
            ErrorKind::InvalidInput
        );
        assert_eq!(
            io_kind(isolator.release(&cgroup("job-1")?))?,
            ErrorKind::InvalidInput
        );
        Ok(())
    }

    #[test]
    fn test_nft_binary_is_injectable() {
        let isolator = NftNetworkIsolator::new().with_nft_binary("/opt/nft");
        assert_eq!(isolator.nft_binary(), Path::new("/opt/nft"));
        assert_eq!(
            NftNetworkIsolator::new().nft_binary(),
            Path::new(super::DEFAULT_NFT_BINARY)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_missing_nft_binary_reports_not_found() -> TestResult {
        let isolator = NftNetworkIsolator::new().with_nft_binary("/nonexistent/harw-test-nft");
        let Err(WardenError::Io(err)) = isolator.isolate(&cgroup("job-1")?) else {
            return Err(TestError::Unexpected(
                "missing nft must fail with Io".to_owned(),
            ));
        };
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert!(err.to_string().contains("/nonexistent/harw-test-nft"));
        assert!(!err.to_string().contains("job-1"));
        assert_eq!(io_kind(isolator.release_all())?, ErrorKind::NotFound);
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn test_non_linux_reports_unsupported() -> TestResult {
        let isolator = NftNetworkIsolator::new();
        assert_eq!(
            io_kind(isolator.isolate(&cgroup("job-1")?))?,
            ErrorKind::Unsupported
        );
        Ok(())
    }

    #[test]
    fn test_classify_nft_failure() {
        let perm = classify_nft_failure(
            Some(1),
            "Error: Could not process rule: Operation not permitted",
        );
        assert_eq!(perm.kind(), ErrorKind::PermissionDenied);
        let missing = classify_nft_failure(
            Some(1),
            "Error: cgroupv2 path fails: No such file or directory",
        );
        assert_eq!(missing.kind(), ErrorKind::NotFound);
        let other = classify_nft_failure(Some(1), "Error: syntax error");
        assert_eq!(other.kind(), ErrorKind::Other);
        assert_eq!(other.to_string(), "nft failed with status 1");
        assert_eq!(
            classify_nft_failure(None, "").to_string(),
            "nft failed with a signal"
        );
    }

    #[test]
    fn test_command_backend_defaults_to_the_nft_timeout() {
        let backend = NftCommandBackend::new();
        assert_eq!(backend.timeout(), super::NFT_TIMEOUT);
        let short = backend.with_timeout(std::time::Duration::from_millis(5));
        assert_eq!(short.timeout(), std::time::Duration::from_millis(5));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_wait_or_kill_kills_and_reaps_a_hanging_child() -> TestResult {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(ctx("spawn sleep"))?;
        let started = std::time::Instant::now();
        let result = super::wait_or_kill(&mut child, std::time::Duration::from_millis(100));
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "a hanging child must time out".to_owned(),
            ));
        };
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
        // Eingesammelt: der Status liegt bereits vor.
        assert!(child.try_wait().map_err(ctx("try_wait"))?.is_some());
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_wait_or_kill_returns_the_status_of_a_child_that_finishes() -> TestResult {
        let mut child = std::process::Command::new("true")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(ctx("spawn true"))?;
        let status = super::wait_or_kill(&mut child, std::time::Duration::from_secs(20))
            .map_err(ctx("child finishes"))?;
        assert!(status.success());
        Ok(())
    }

    // --- NetfilterBackend (PL-100 O10) -----------------------------------

    #[test]
    fn test_backend_receives_the_resolved_target_for_isolate_and_release() -> TestResult {
        let backend = RecordingBackend::default();
        let isolator = NftNetworkIsolator::with_backend(
            backend.clone(),
            Path::new("/sys/fs/cgroup/harw.slice"),
        );
        isolator
            .isolate(&cgroup("job-1")?)
            .map_err(ctx("isolate"))?;
        isolator
            .release_isolation(&cgroup("job-1")?)
            .map_err(ctx("release"))?;
        assert_eq!(
            backend.applied()?,
            vec![
                "isolate harw.slice/job-1 level 2".to_owned(),
                "release harw.slice/job-1 level 2".to_owned(),
            ]
        );
        Ok(())
    }

    #[test]
    fn test_backend_is_never_called_for_unsafe_ids_or_a_foreign_root() -> TestResult {
        let backend = RecordingBackend::default();
        let isolator =
            NftNetworkIsolator::with_backend(backend.clone(), Path::new("/sys/fs/cgroup"));
        for id in ["a;b", "../x", "a b", "a//b"] {
            assert!(matches!(
                isolator.isolate(&cgroup(id)?),
                Err(WardenError::InvalidCgroupId)
            ));
            assert!(matches!(
                isolator.release_isolation(&cgroup(id)?),
                Err(WardenError::InvalidCgroupId)
            ));
        }
        let foreign = NftNetworkIsolator::with_backend(backend.clone(), Path::new("/srv/cgroups"));
        assert_eq!(
            io_kind(foreign.isolate(&cgroup("job-1")?))?,
            ErrorKind::InvalidInput
        );
        assert!(backend.applied()?.is_empty());
        Ok(())
    }

    #[test]
    fn test_backend_failure_is_passed_through_unchanged() -> TestResult {
        let backend = RecordingBackend::failing(ErrorKind::PermissionDenied);
        let isolator =
            NftNetworkIsolator::with_backend(backend.clone(), Path::new("/sys/fs/cgroup"));
        assert_eq!(
            io_kind(isolator.isolate(&cgroup("job-1")?))?,
            ErrorKind::PermissionDenied
        );
        // Der Versuch ist trotzdem genau einmal beim Backend angekommen.
        assert_eq!(backend.applied()?, vec!["isolate job-1 level 1".to_owned()]);
        Ok(())
    }

    #[test]
    fn test_isolator_reports_the_backend_sandbox_needs() {
        let fake = NftNetworkIsolator::with_backend(
            RecordingBackend::default(),
            Path::new("/sys/fs/cgroup"),
        );
        assert_eq!(fake.sandbox_needs(), SandboxNeeds::default());
    }

    #[test]
    fn test_command_backend_needs_nft_paths_dev_null_and_process_syscalls() {
        let needs = NftCommandBackend::new().sandbox_needs();
        assert_eq!(
            needs.read_exec_paths.first().map(PathBuf::as_path),
            Some(Path::new(super::DEFAULT_NFT_BINARY))
        );
        assert!(
            !needs
                .read_exec_paths
                .iter()
                .any(|path| path.starts_with("/sys"))
        );
        assert_eq!(needs.read_write_files, vec![PathBuf::from("/dev/null")]);
        let names: Vec<&str> = needs.syscalls.iter().map(|syscall| syscall.name).collect();
        for required in [
            "execve", "pipe2", "wait4", "kill", "socket", "sendmsg", "recvmsg",
        ] {
            assert!(names.contains(&required), "missing {required} in {names:?}");
        }
        assert_eq!(NftCommandBackend::new().name(), "nft-subprocess");
    }

    #[test]
    fn test_disabled_isolator_refuses_everything_without_sandbox_needs() -> TestResult {
        let disabled = DisabledNetworkIsolator;
        assert_eq!(
            io_kind(disabled.isolate(&cgroup("job-1")?))?,
            ErrorKind::Unsupported
        );
        assert_eq!(
            io_kind(disabled.release_isolation(&cgroup("job-1")?))?,
            ErrorKind::Unsupported
        );
        Ok(())
    }

    #[test]
    fn test_network_actions_enabled_follows_the_allowlist() -> TestResult {
        assert!(!network_actions_enabled(&ActionAllowlist::freeze_and_thaw()));
        let with_isolation = ActionAllowlist::new([
            "freeze-cgroup",
            "thaw-cgroup",
            "isolate-network",
            "lift-isolation",
        ])
        .map_err(ctx("allowlist"))?;
        assert!(network_actions_enabled(&with_isolation));
        let lift_only = ActionAllowlist::new(["lift-isolation"]).map_err(ctx("allowlist"))?;
        assert!(network_actions_enabled(&lift_only));
        let kill_only = ActionAllowlist::new(["kill-process-tree"]).map_err(ctx("allowlist"))?;
        assert!(!network_actions_enabled(&kill_only));
        Ok(())
    }

    #[test]
    fn test_network_action_kinds_match_the_wire_names() -> TestResult {
        let job = cgroup("harw.slice/job-1")?;
        let isolate = WardenAction::IsolateNetwork {
            cgroup: job.clone(),
        };
        let lift = WardenAction::LiftIsolation { cgroup: job };
        assert_eq!(
            NETWORK_ACTION_KINDS,
            [isolate.kind_name(), lift.kind_name()]
        );
        for kind in NETWORK_ACTION_KINDS {
            assert!(harw_dod_warden::policy::ACCEPTED_ACTION_KINDS.contains(&kind));
        }
        Ok(())
    }

    // --- Ende zu Ende über den Warden (fake Backend) ----------------------

    const KEY: [u8; 32] = [0x5a; 32];
    const NOW: i64 = 1_800_000_000;
    const KERNEL_ID: u64 = 7_331;

    fn at(offset: i64) -> TestResult<jiff::Timestamp> {
        jiff::Timestamp::from_second(NOW.saturating_add(offset)).map_err(ctx("timestamp"))
    }

    fn network_warden(
        dir: &Path,
        backend: RecordingBackend,
        allowlist: ActionAllowlist,
    ) -> TestResult<Warden> {
        let observer = StaticTargetObserver::new();
        observer.set(
            &cgroup("harw.slice/job-1")?,
            CgroupKernelId::new(KERNEL_ID).map_err(ctx("kid"))?,
        );
        network_warden_observing(dir, backend, allowlist, observer)
    }

    fn network_warden_observing(
        dir: &Path,
        backend: RecordingBackend,
        allowlist: ActionAllowlist,
        observer: StaticTargetObserver,
    ) -> TestResult<Warden> {
        let mut keys = KeyRing::new();
        keys.insert(
            KeyId::new("k1").map_err(ctx("key id"))?,
            ProofKey::from_bytes(KEY),
        )
        .map_err(ctx("ring"))?;
        let executor = RecordingExecutor::new();
        Ok(Warden::new(WardenParts {
            freezer: Box::new(executor.clone()),
            thawer: Box::new(executor.clone()),
            isolator: Box::new(NftNetworkIsolator::with_backend(
                backend,
                Path::new("/sys/fs/cgroup"),
            )),
            killer: Box::new(executor),
            observer: Box::new(observer),
            audit: Box::new(RecordingAuditSink::new()),
            keys,
            policy: WardenPolicy::new(
                ProofPolicy::new(120, 5, vec!["harw.slice".to_owned()]).map_err(ctx("proof"))?,
                allowlist,
                DenyList::builtin(),
            ),
            state: WardenState::open(dir, at(0)?).map_err(ctx("state"))?,
            // Netzaktionen legen keine Eindämmungsmarker an.
            markers: ContainmentMarkers::new(dir.join("holds")),
        }))
    }

    fn escalated_request(action: &WardenAction, nonce: u8) -> TestResult<WardenRequest> {
        let body = AuthorizationBody {
            finding_id: FindingId::try_from_str("finding-net-1").map_err(ctx("finding"))?,
            approver: ApprovalActor::Operator {
                id: "operator-1".to_owned(),
            },
            action: action.clone(),
            stage: EscalationStage::Escalated,
            target: CgroupTarget::new(
                action.cgroup().clone(),
                CgroupKernelId::new(KERNEL_ID).map_err(ctx("kid"))?,
                at(-60)?,
            ),
            hold_secs: None,
            irreversible_ack: action.requires_irreversible_ack(),
            nonce: [nonce; 16],
            issued_at: at(0)?,
            expires_at: at(60)?,
        };
        Ok(WardenRequest::new(
            action.clone(),
            SignedAuthorization::sign(
                &ProofKey::from_bytes(KEY),
                &KeyId::new("k1").map_err(ctx("key id"))?,
                body,
            ),
        ))
    }

    #[test]
    fn test_warden_isolates_and_lifts_through_the_backend() -> TestResult {
        let dir = private_tempdir()?;
        let backend = RecordingBackend::default();
        let allowlist = ActionAllowlist::new([
            "freeze-cgroup",
            "thaw-cgroup",
            "isolate-network",
            "lift-isolation",
        ])
        .map_err(ctx("allowlist"))?;
        let warden = network_warden(dir.path(), backend.clone(), allowlist)?;
        let job = cgroup("harw.slice/job-1")?;

        let isolate = WardenAction::IsolateNetwork {
            cgroup: job.clone(),
        };
        let outcome = warden.handle(&escalated_request(&isolate, 1)?, at(1)?);
        if !matches!(outcome, WardenOutcome::Executed(_)) {
            return Err(TestError::Unexpected(format!(
                "isolate: expected Executed, got {outcome:?}"
            )));
        }
        let lift = WardenAction::LiftIsolation { cgroup: job };
        let outcome = warden.handle(&escalated_request(&lift, 2)?, at(2)?);
        if !matches!(outcome, WardenOutcome::Executed(_)) {
            return Err(TestError::Unexpected(format!(
                "lift: expected Executed, got {outcome:?}"
            )));
        }
        assert_eq!(
            backend.applied()?,
            vec![
                "isolate harw.slice/job-1 level 2".to_owned(),
                "release harw.slice/job-1 level 2".to_owned(),
            ]
        );
        Ok(())
    }

    #[test]
    fn test_warden_reports_a_failing_backend_as_execution_failed() -> TestResult {
        // `NotFound` ist auch die Fehlerart eines fehlenden `nft`-Programms
        // (`NftCommandBackend::spawn_error`): bei unverändertem Ziel ist das
        // `execution-failed`, nie `target-changed`.
        for kind in [ErrorKind::PermissionDenied, ErrorKind::NotFound] {
            let dir = private_tempdir()?;
            let backend = RecordingBackend::failing(kind);
            let allowlist = ActionAllowlist::new([
                "freeze-cgroup",
                "thaw-cgroup",
                "isolate-network",
                "lift-isolation",
            ])
            .map_err(ctx("allowlist"))?;
            let warden = network_warden(dir.path(), backend.clone(), allowlist)?;
            let isolate = WardenAction::IsolateNetwork {
                cgroup: cgroup("harw.slice/job-1")?,
            };
            let outcome = warden.handle(&escalated_request(&isolate, 3)?, at(1)?);
            if outcome != WardenOutcome::ExecutionFailed {
                return Err(TestError::Unexpected(format!(
                    "{kind:?}: expected ExecutionFailed, got {outcome:?}"
                )));
            }
            // Auch dieser Pfad hat eine Antwort.
            assert!(matches!(
                outcome.to_wire(),
                WardenResponse::Denied {
                    reason: Denial::ExecutionFailed
                }
            ));
            assert_eq!(backend.applied()?.len(), 1);
        }
        Ok(())
    }

    #[test]
    fn test_lift_of_a_vanished_cgroup_still_removes_its_chains() -> TestResult {
        let dir = private_tempdir()?;
        let backend = RecordingBackend::default();
        let allowlist = ActionAllowlist::new([
            "freeze-cgroup",
            "thaw-cgroup",
            "isolate-network",
            "lift-isolation",
        ])
        .map_err(ctx("allowlist"))?;
        let observer = StaticTargetObserver::new();
        let job = cgroup("harw.slice/job-1")?;
        observer.set(&job, CgroupKernelId::new(KERNEL_ID).map_err(ctx("kid"))?);
        let warden =
            network_warden_observing(dir.path(), backend.clone(), allowlist, observer.clone())?;
        let isolate = WardenAction::IsolateNetwork {
            cgroup: job.clone(),
        };
        let outcome = warden.handle(&escalated_request(&isolate, 5)?, at(1)?);
        assert!(matches!(outcome, WardenOutcome::Executed(_)));
        // Die cgroup endet; ihre Ketten hängen am Pfad und bleiben sonst stehen.
        observer.remove(&job);
        let lift = WardenAction::LiftIsolation { cgroup: job };
        let outcome = warden.handle(&escalated_request(&lift, 6)?, at(2)?);
        if !matches!(outcome, WardenOutcome::Executed(_)) {
            return Err(TestError::Unexpected(format!(
                "lift of a vanished cgroup: expected Executed, got {outcome:?}"
            )));
        }
        assert_eq!(
            backend.applied()?,
            vec![
                "isolate harw.slice/job-1 level 2".to_owned(),
                "release harw.slice/job-1 level 2".to_owned(),
            ]
        );
        Ok(())
    }

    #[test]
    fn test_policy_without_network_actions_never_reaches_the_backend() -> TestResult {
        let dir = private_tempdir()?;
        let backend = RecordingBackend::default();
        let warden = network_warden(
            dir.path(),
            backend.clone(),
            ActionAllowlist::freeze_and_thaw(),
        )?;
        let isolate = WardenAction::IsolateNetwork {
            cgroup: cgroup("harw.slice/job-1")?,
        };
        let outcome = warden.handle(&escalated_request(&isolate, 4)?, at(1)?);
        assert_eq!(outcome, WardenOutcome::Denied(Denial::PolicyDenied));
        assert!(backend.applied()?.is_empty());
        Ok(())
    }
}
