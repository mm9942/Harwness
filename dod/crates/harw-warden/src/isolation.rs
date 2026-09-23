//! Netzisolation für [`harw_dod_warden::NetworkIsolator`]: ein echtes
//! Linux-Backend über das `nft`-Werkzeug (nftables) und der bisherige,
//! ehrlich fehlschlagende Platzhalter.
//!
//! # Der gewählte Weg: `nft` als Kindprozess
//! Frühere Recherchen (siehe Git-Historie dieser Datei) haben drei Wege
//! verworfen: `rustables`/`nftnl` (C-Build gegen `libnftnl` über
//! `bindgen`), `rtnetlink` (kennt keine cgroup, bräuchte
//! Netz-Namensraum-pro-cgroup-Infrastruktur) und eBPF-cgroup-Hooks
//! (`unsafe`-Syscalls oder eine neue Werkzeugkette). Übrig bleibt der eine
//! Weg, der **keine** neue Abhängigkeit, **kein** `unsafe` und **keinen**
//! C-Build braucht: das System-Werkzeug `nft` per
//! [`std::process::Command`] aufrufen. `nft` kann eine cgroup-v2 direkt
//! matchen (`socket cgroupv2 level N "<pfad>"`, Kernel ≥ 5.13, `nft` ≥
//! 0.9.4) — genau die Durchsetzung „diese cgroup kommt nicht mehr ins Netz",
//! ohne die Art zu ändern, wie Jobs gestartet werden. Das Abhängigkeitsbudget
//! von `harw-warden/Cargo.toml` bleibt unverändert; `harw-dod-netpolicy`
//! und `harw-dod-netlink` sind keine Abhängigkeiten dieses Crates und werden
//! deshalb nicht wiederverwendet.
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
//! ([`NftNetworkIsolator::release`]) handle-frei: Kette leeren und
//! löschen, kein Parsen von `nft -a list`-Ausgaben.
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
//! irgendetwas auszuführen. Der Pfad zum `nft`-Binary ist injizierbar
//! ([`NftNetworkIsolator::with_nft_binary`]).
//!
//! # Eingabeprüfung
//! `nft` parst seine Argumente als Skript. Die cgroup-Kennung wird deshalb
//! strenger geprüft als in `CgroupV2Executor`: nur ASCII-Alphanumerik und
//! `._-@:+`, kein `.`/`..` als ganzes Segment, höchstens
//! [`MAX_CGROUP_ID_LEN`] Bytes (Kettennamen sind in nftables auf 255 Byte
//! begrenzt). Alles andere ist [`WardenError::InvalidCgroupId`], bevor ein
//! Prozess gestartet wird.
//!
//! # Fehlerbilder
//! - `nft` fehlt → [`WardenError::Io`] mit `ErrorKind::NotFound` und dem
//!   gesuchten Pfad.
//! - Fehlende Rechte (`CAP_NET_ADMIN`) → `ErrorKind::PermissionDenied`.
//! - cgroup existiert nicht (nft löst den Pfad beim Einfügen auf) →
//!   `ErrorKind::NotFound`.
//! - Nicht-Linux → `ErrorKind::Unsupported`, ohne einen Prozess zu starten.
//!
//! Die Fehlermeldungen selbst enthalten nie die cgroup-Kennung; die
//! vollständige `nft`-Fehlerausgabe erscheint nur über `tracing::error!`
//! am Ausführungsort.
//!
//! # Laufzeitvoraussetzungen
//! `CAP_NET_ADMIN` für den Warden-Prozess, und die Landlock-Regel
//! (`crate::landlock`) muss das Ausführen des `nft`-Binarys samt seiner
//! Bibliotheken erlauben — sonst meldet der Aufruf `PermissionDenied`.
//!
//! # Nebenläufigkeit
//! Beide Typen sind zustandslos bzw. unveränderlich nach dem Bau und
//! automatisch `Send + Sync`; gleichzeitige Aufrufe serialisiert der Kernel
//! über die nftables-Transaktionen.

use std::io;
use std::path::{Component, Path, PathBuf};

use harw_dod_warden::{NetworkIsolator, WardenError, WardenResult};
use harw_types::CgroupId;

/// Adressfamilie der eigenen nftables-Tabelle (IPv4 und IPv6 gemeinsam).
pub const NFT_TABLE_FAMILY: &str = "inet";

/// Name der eigenen nftables-Tabelle; keine andere Tabelle wird berührt.
pub const NFT_TABLE_NAME: &str = "harw_warden";

/// Vorgabepfad des `nft`-Binarys.
pub const DEFAULT_NFT_BINARY: &str = "/usr/sbin/nft";

/// Einhängepunkt der cgroup-v2-Hierarchie; `nft` erwartet cgroup-Pfade
/// relativ dazu.
pub const CGROUP2_MOUNT: &str = "/sys/fs/cgroup";

/// Längste zulässige cgroup-Kennung in Byte: `2 + 2 * 126 = 254` Zeichen
/// Kettenname passen unter die nftables-Grenze von 255.
pub const MAX_CGROUP_ID_LEN: usize = 126;

/// Obergrenze für die geloggte `nft`-Fehlerausgabe in Zeichen.
#[cfg(target_os = "linux")]
const MAX_LOGGED_STDERR: usize = 2048;

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
    ///   Einhängepunkt und cgroup (leer: cgroup liegt direkt darunter).
    /// - `cgroup` (`&CgroupId`): die zu isolierende cgroup.
    ///
    /// # Returns
    /// Das Ziel.
    ///
    /// # Errors
    /// [`WardenError::InvalidCgroupId`], wenn die Kennung oder ein
    /// Elternsegment unzulässige Zeichen enthält oder zu lang ist.
    pub fn resolve(parent: &[String], cgroup: &CgroupId) -> WardenResult<Self> {
        let id = cgroup.as_str();
        if !is_safe_segment(id) || id.len() > MAX_CGROUP_ID_LEN {
            return Err(WardenError::InvalidCgroupId);
        }
        if !parent.iter().all(|segment| is_safe_segment(segment)) {
            return Err(WardenError::InvalidCgroupId);
        }
        let mut segments: Vec<&str> = parent.iter().map(String::as_str).collect();
        segments.push(id);
        Ok(Self {
            path: segments.join("/"),
            level: segments.len(),
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

    fn output_chain(&self) -> String {
        format!("o_{}", self.chain_stem)
    }

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
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    out
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
        if index + 1 < chains.len() {
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

/// Isoliert cgroups über nftables-Regeln (`nft`-Kindprozess).
///
/// # Description
/// Siehe Moduldoku. Hält nur den Pfad zum `nft`-Binary und die
/// Elternsegmente zwischen cgroup-v2-Einhängepunkt und den verwalteten
/// cgroups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NftNetworkIsolator {
    nft_binary: PathBuf,
    /// `None`: die konfigurierte cgroup-Wurzel liegt nicht unter
    /// [`CGROUP2_MOUNT`] oder enthält unzulässige Segmente — jede Isolation
    /// scheitert dann, ohne `nft` zu starten.
    cgroup_parent: Option<Vec<String>>,
}

impl Default for NftNetworkIsolator {
    fn default() -> Self {
        Self::new()
    }
}

impl NftNetworkIsolator {
    /// Baut einen Isolator für cgroups direkt unter [`CGROUP2_MOUNT`] mit
    /// [`DEFAULT_NFT_BINARY`].
    ///
    /// # Returns
    /// Den Isolator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nft_binary: PathBuf::from(DEFAULT_NFT_BINARY),
            cgroup_parent: Some(Vec::new()),
        }
    }

    /// Baut einen Isolator passend zur `--cgroup-root` des Wardens.
    ///
    /// # Description
    /// `nft` erwartet cgroup-Pfade relativ zu [`CGROUP2_MOUNT`]; die Wurzel
    /// wird deshalb davon abgezogen. Liegt sie nicht darunter (z. B. ein
    /// Test-Fixture), scheitert jede spätere Isolation mit
    /// `ErrorKind::InvalidInput`, ohne `nft` zu starten.
    ///
    /// # Arguments
    /// - `cgroup_root` (`&Path`): dieselbe Wurzel wie für
    ///   `CgroupV2Executor`.
    ///
    /// # Returns
    /// Den Isolator.
    #[must_use]
    pub fn from_cgroup_root(cgroup_root: &Path) -> Self {
        let cgroup_parent = cgroup_root
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
            });
        Self {
            nft_binary: PathBuf::from(DEFAULT_NFT_BINARY),
            cgroup_parent,
        }
    }

    /// Ersetzt den Pfad zum `nft`-Binary (Tests, abweichende Distributionen).
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
        self.nft_binary = nft_binary.into();
        self
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
        self.run(&release_command_args(&target)).inspect_err(|err| {
            tracing::error!(cgroup = %cgroup.as_str(), error = %err, "network release failed");
        })
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
        self.run(&release_all_command_args()).inspect_err(|err| {
            tracing::error!(error = %err, "network release of all cgroups failed");
        })
    }

    /// Führt `nft` mit den gegebenen Argumenten aus.
    #[cfg(target_os = "linux")]
    fn run(&self, args: &[String]) -> WardenResult<()> {
        use std::process::{Command, Stdio};

        let output = Command::new(&self.nft_binary)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|err| self.spawn_error(&err))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let logged: String = stderr.chars().take(MAX_LOGGED_STDERR).collect();
        tracing::error!(
            nft = %self.nft_binary.display(),
            status = ?output.status.code(),
            stderr = %logged.trim_end(),
            "nft invocation failed"
        );
        Err(WardenError::Io(classify_nft_failure(
            output.status.code(),
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

impl NetworkIsolator for NftNetworkIsolator {
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
    ///   Prozessstart).
    /// - [`WardenError::Io`]: `nft` fehlt (`NotFound`), Rechte fehlen
    ///   (`PermissionDenied`), cgroup existiert nicht (`NotFound`),
    ///   Wurzel nicht unter [`CGROUP2_MOUNT`] (`InvalidInput`), Nicht-Linux
    ///   (`Unsupported`) oder sonstiger `nft`-Fehlschlag.
    fn isolate(&self, cgroup: &CgroupId) -> WardenResult<()> {
        let target = self.target(cgroup)?;
        self.run(&isolate_command_args(&target)).inspect_err(|err| {
            tracing::error!(cgroup = %cgroup.as_str(), error = %err, "network isolation failed");
        })
    }

    /// Entfernt die Isolations-Chains dieser cgroup (idempotent).
    fn release_isolation(&self, cgroup: &CgroupId) -> WardenResult<()> {
        self.release(cgroup)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        NftCgroupTarget, NftNetworkIsolator, classify_nft_failure, isolate_command_args,
        release_all_command_args, release_command_args,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_warden::{NetworkIsolator, WardenError};
    use harw_types::CgroupId;
    use std::io::ErrorKind;
    use std::path::Path;

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
    fn test_unsafe_ids_are_rejected_before_any_process_starts() -> TestResult {
        // Absichtlich nicht existierendes Binary: würde je ein Prozess
        // gestartet, käme `Io`, nicht `InvalidCgroupId`.
        let isolator = NftNetworkIsolator::new().with_nft_binary("/nonexistent/harw-test-nft");
        let too_long = "a".repeat(super::MAX_CGROUP_ID_LEN + 1);
        for id in [
            "a/b",
            "..",
            ".",
            "a\"b",
            "a b",
            "a;b",
            "a\\b",
            too_long.as_str(),
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
}
