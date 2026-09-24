//! Typisiertes Werkzeug `latex.build` (Runde 4, Teil E).
//!
//! # Zweck
//! Der LaTeX-Worker der UIA (`uia-latex-writer`) darf einen Build **starten**,
//! aber nie eine freie Shell benutzen. `latex.build` startet deshalb genau ein
//! festes Programm mit festem argv:
//!
//! ```text
//! latexmk -norc -xelatex -interaction=nonstopmode -halt-on-error \
//!         -file-line-error -no-shell-escape -cd <datei.tex>
//! ```
//!
//! Das Modell wählt nur die Datei, optional die Engine (`xelatex`, `pdflatex`,
//! `lualatex` als Enum) und `clean` (`latexmk -norc -c -cd <datei.tex>`).
//! Weitere Argumente gibt es nicht.
//!
//! # Sicherheit
//! - **Recht:** `ExecuteProcess` (Prozessstart) und `WriteWorkspace` (Build
//!   schreibt `.aux`/`.log`/`.pdf` neben die Quelle). Das Werkzeug steht nicht
//!   in der Auto-Freigabe — jeder Aufruf geht durch die Freigabekette.
//! - **Sandbox:** derselbe Bubblewrap-Pfad wie `shell.exec` (festgepinntes
//!   `bwrap`/`prlimit`, `--unshare-all --unshare-net`, stdin `/dev/null`,
//!   gemeinsames Ausgabebudget, Zeitlimit, Abbruch). Es gibt **keinen**
//!   Host-Pfad: ohne startbares `bwrap` wird nicht gebaut. Netz ist immer aus.
//! - **Datei:** der Pfad wird kanonisiert (Symlinks aufgelöst) und muss danach
//!   unter der kanonischen Workspace-Wurzel liegen, eine reguläre Datei sein
//!   und auf `.tex` enden — kein Symlink-Ausbruch.
//! - **Kein Shell-Escape, keine rc-Dateien:** `-no-shell-escape` schaltet
//!   `\write18` ab; `-norc` ignoriert jede `.latexmkrc` (Perl-Code, den sonst
//!   eine vom Modell geschriebene Datei ausführen könnte). Eine
//!   `.latexmkrc` im Projekt gilt nur für Builds der Nutzerin von Hand.
//! - **Nie installieren:** fehlt `latexmk` oder die Engine, liefert das
//!   Werkzeug `{"status": "not_installed", "missing": [...], "user_message":
//!   "..."}` — ein sauberes Ergebnis, kein Fehler, kein Installationsversuch.
//!
//! # TeX in der Sandbox
//! `/usr`, `/bin`, `/lib*` bindet der Plan ohnehin. Zusätzlich bindet dieses
//! Werkzeug nur lesend die üblichen TeX-/Font-Pfade außerhalb von `/usr`
//! ([`TEX_READ_ONLY_ROOTS`]) und — falls `latexmk` in einer TeX-Live-
//! Installation außerhalb von `/usr` liegt — deren Wurzel. `PATH` in der
//! Sandbox besteht aus den Verzeichnissen der gefundenen Programme plus dem
//! Minimal-`PATH`.

use crate::capture::{BoundedCapture, DrainEnd};
use crate::exec::{configure_stdio, terminate};
use crate::limits::{ShellLimits, launch_command};
use harw_authority::{Permission, SandboxSpec};
use harw_extension_api::contributors::ToolProvider;
use harw_sandbox::{BwrapLauncher, HostPathBinding};
use harw_tools::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolOutput, ToolsError,
    schema::{AdditionalProperties, JsonSchema, JsonSchemaType},
    spec::{FunctionToolSpec, ToolName, ToolSpec},
};
use harw_types::cancel::CancelToken;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    io::Read,
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::process::Command as TokioCommand;
use tracing::{info, warn};

// ── Konstanten ────────────────────────────────────────────────────────────────

/// Name des Werkzeugs.
pub const LATEX_BUILD_TOOL: &str = "latex.build";
/// Zeitlimit eines Builds in Sekunden (latexmk läuft mehrere Durchgänge plus
/// biber).
const DEFAULT_TIMEOUT_SECS: u64 = 120;
/// Gemeinsames Budget für stdout+stderr.
const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// Höchstens so viele Bytes der `.log`-Datei werden gelesen.
const LOG_READ_LIMIT: u64 = 4 * 1024 * 1024;
/// Höchstens so viele Zeilen im Log-Auszug.
const MAX_EXCERPT_LINES: usize = 40;
/// Längere Log-Zeilen werden auf diese Zeichenzahl gekürzt.
const MAX_EXCERPT_LINE_CHARS: usize = 300;
/// So viele letzte Zeilen der Prozessausgabe gehen in `output_tail`.
const OUTPUT_TAIL_LINES: usize = 20;
/// Minimal-`PATH` in der Sandbox, hinter den Programmverzeichnissen.
const MINIMAL_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
/// Das Programm, das jeder Aufruf braucht (Bauen und Aufräumen).
const LATEXMK: &str = "latexmk";

/// TeX- und Font-Pfade außerhalb von `/usr`, die `latex.build` nur lesend in
/// die Sandbox bindet (`--ro-bind-try`, fehlende Pfade sind kein Fehler).
///
/// - `/opt/texlive`: TeX-Live-Installationen außerhalb von `/usr/local`.
/// - `/var/lib/texmf`, `/etc/texmf`, `/etc/texlive`: Formatdateien und
///   Konfiguration der Distributionspakete (Debian/Ubuntu, Fedora).
/// - `/etc/fonts`, `/var/cache/fontconfig`: fontconfig für XeLaTeX/LuaLaTeX
///   (Systemschriften per Namen).
pub const TEX_READ_ONLY_ROOTS: &[&str] = &[
    "/opt/texlive",
    "/var/lib/texmf",
    "/etc/texmf",
    "/etc/texlive",
    "/etc/fonts",
    "/var/cache/fontconfig",
];

// ── Argumente ─────────────────────────────────────────────────────────────────

/// Die erlaubten Engines — ein Enum, kein freier String.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LatexEngine {
    /// XeLaTeX (Standard): Unicode, Systemschriften über `fontspec`.
    #[default]
    Xelatex,
    /// pdfLaTeX.
    Pdflatex,
    /// LuaLaTeX.
    Lualatex,
}

impl LatexEngine {
    /// Name des Engine-Programms.
    #[must_use]
    pub const fn binary(self) -> &'static str {
        match self {
            Self::Xelatex => "xelatex",
            Self::Pdflatex => "pdflatex",
            Self::Lualatex => "lualatex",
        }
    }

    /// Der latexmk-Schalter, der diese Engine wählt.
    #[must_use]
    pub const fn latexmk_flag(self) -> &'static str {
        match self {
            Self::Xelatex => "-xelatex",
            Self::Pdflatex => "-pdf",
            Self::Lualatex => "-lualatex",
        }
    }
}

/// Argumente eines `latex.build`-Aufrufs. Unbekannte Felder werden abgelehnt.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LatexBuildArgs {
    /// `.tex`-Datei, relativ zur Workspace-Wurzel oder absolut darunter.
    file: String,
    /// Engine; fehlt sie (oder ist `null`), gilt [`LatexEngine::Xelatex`].
    #[serde(default)]
    engine: Option<LatexEngine>,
    /// `true` räumt Hilfsdateien auf (`latexmk -c`) statt zu bauen.
    #[serde(default)]
    clean: Option<bool>,
}

/// Baut das feste argv (ohne Programm) für einen Build bzw. ein Aufräumen.
///
/// # Beschreibung
/// Rein und ohne Prozessstart testbar. `-norc` vor allem anderen, damit keine
/// `.latexmkrc` gelesen wird; `-cd` wechselt in das Verzeichnis der Datei.
///
/// # Argumente
/// - `engine` (`LatexEngine`): gewählte Engine (beim Aufräumen ohne Wirkung).
/// - `clean` (`bool`): `true` für `latexmk -c`.
/// - `file` (`&Path`): kanonischer, absoluter Pfad der `.tex`-Datei.
///
/// # Rückgabe
/// Die Argumente in Startreihenfolge.
#[must_use]
pub fn latexmk_args(engine: LatexEngine, clean: bool, file: &Path) -> Vec<OsString> {
    let mut args = vec![OsString::from("-norc")];
    if clean {
        args.push(OsString::from("-c"));
    } else {
        args.extend(
            [
                engine.latexmk_flag(),
                "-interaction=nonstopmode",
                "-halt-on-error",
                "-file-line-error",
                "-no-shell-escape",
            ]
            .map(OsString::from),
        );
    }
    args.push(OsString::from("-cd"));
    args.push(file.as_os_str().to_owned());
    args
}

/// Kanonisiert `file` und prüft die Workspace-Grenze.
///
/// # Argumente
/// - `root` (`&Path`): kanonische Workspace-Wurzel.
/// - `file` (`&str`): Pfad aus dem Aufruf, relativ zu `root` oder absolut.
///
/// # Rückgabe
/// Den kanonischen Pfad einer regulären `.tex`-Datei unter `root`.
///
/// # Errors
/// Eine lesbare Meldung, wenn die Datei fehlt, außerhalb des Workspaces liegt
/// (auch über einen Symlink), kein reguläre Datei ist oder nicht auf `.tex`
/// endet.
pub fn resolve_tex_file(root: &Path, file: &str) -> Result<PathBuf, String> {
    let trimmed = file.trim();
    if trimmed.is_empty() {
        return Err("file darf nicht leer sein".to_owned());
    }
    let candidate = Path::new(trimmed);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    let canonical = joined
        .canonicalize()
        .map_err(|err| format!("{trimmed}: Datei nicht gefunden ({err})"))?;
    if !canonical.starts_with(root) {
        return Err(format!(
            "{trimmed}: liegt außerhalb des Workspaces (auch Symlinks werden aufgelöst)"
        ));
    }
    if !canonical.is_file() {
        return Err(format!("{trimmed}: keine reguläre Datei"));
    }
    let is_tex = canonical
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tex"));
    if !is_tex {
        return Err(format!("{trimmed}: nur .tex-Dateien lassen sich bauen"));
    }
    Ok(canonical)
}

// ── Programmsuche ─────────────────────────────────────────────────────────────

/// Ergebnis der Programmsuche.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Toolchain {
    /// Absoluter Pfad von `latexmk`.
    latexmk: PathBuf,
    /// `PATH` für die Sandbox: Programmverzeichnisse plus [`MINIMAL_PATH`].
    sandbox_path: String,
    /// Zusätzliche, nur lesende Wurzeln (TeX-Live außerhalb von `/usr`).
    install_roots: Vec<PathBuf>,
}

/// `true`, wenn `path` eine reguläre, ausführbare Datei ist.
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// Sucht `program` in den absoluten Einträgen von `search_path`.
fn find_in_path(search_path: &OsStr, program: &str) -> Option<PathBuf> {
    std::env::split_paths(search_path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable_file(candidate))
}

/// Die TeX-Live-Wurzel zu einem Programm `…/<jahr>/bin/<arch>/<prog>`, falls
/// sie außerhalb von `/usr` liegt (unter `/usr` bindet der Plan ohnehin).
fn install_root_of(binary: &Path) -> Option<PathBuf> {
    let canonical = binary.canonicalize().ok()?;
    let arch_dir = canonical.parent()?;
    let bin_dir = arch_dir.parent()?;
    if bin_dir.file_name().and_then(OsStr::to_str) != Some("bin") {
        return None;
    }
    let root = bin_dir.parent()?;
    if root.starts_with("/usr") || root == Path::new("/") {
        return None;
    }
    Some(root.to_path_buf())
}

/// Sucht `latexmk` und (außer beim Aufräumen) die Engine.
///
/// # Rückgabe
/// `Ok(toolchain)` oder `Err(missing)` mit den fehlenden Programmnamen in
/// fester Reihenfolge (`latexmk` zuerst).
fn resolve_toolchain(
    search_path: &OsStr,
    engine: LatexEngine,
    clean: bool,
) -> Result<Toolchain, Vec<String>> {
    let mut needed = vec![LATEXMK];
    if !clean {
        needed.push(engine.binary());
    }
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for program in needed {
        match find_in_path(search_path, program) {
            Some(path) => found.push(path),
            None => missing.push(program.to_owned()),
        }
    }
    if !missing.is_empty() {
        return Err(missing);
    }
    // `latexmk` steht immer an erster Stelle von `needed`.
    let Some(latexmk) = found.first().cloned() else {
        return Err(vec![LATEXMK.to_owned()]);
    };
    let mut dirs: Vec<String> = Vec::new();
    let mut install_roots: Vec<PathBuf> = Vec::new();
    for path in &found {
        if let Some(dir) = path.parent().and_then(Path::to_str)
            && !dirs.iter().any(|known| known == dir)
        {
            dirs.push(dir.to_owned());
        }
        if let Some(root) = install_root_of(path)
            && !install_roots.contains(&root)
        {
            install_roots.push(root);
        }
    }
    dirs.push(MINIMAL_PATH.to_owned());
    Ok(Toolchain {
        latexmk,
        sandbox_path: dirs.join(":"),
        install_roots,
    })
}

/// Die Meldung für die Nutzerin, wenn Programme fehlen (Deutsch, nur Hinweis —
/// es wird nichts ausgeführt oder installiert).
#[must_use]
pub fn not_installed_message(missing: &[String]) -> String {
    format!(
        "LaTeX ist auf diesem Rechner nicht (vollständig) installiert: es fehlt {}. \
         Harwness installiert nichts selbst. Hinweis zur Installation: \
         Debian/Ubuntu: sudo apt install latexmk texlive-xetex texlive-lang-german · \
         Fedora: sudo dnf install latexmk texlive-xetex · \
         Arch: sudo pacman -S texlive-binextra texlive-xetex · \
         macOS: MacTeX bzw. brew install --cask mactex. \
         Die .tex-Dateien sind fertig; danach baut z. B. \
         „latexmk -xelatex <datei>.tex“ das PDF.",
        missing.join(", ")
    )
}

/// Das strukturierte Ergebnis für fehlende Programme.
fn not_installed_output(missing: &[String]) -> ToolOutput {
    ToolOutput::json(json!({
        "status": "not_installed",
        "missing": missing,
        "user_message": not_installed_message(missing),
    }))
}

// ── Log-Auswertung ────────────────────────────────────────────────────────────

/// `true` für `datei.tex:12: …`-Zeilen (`-file-line-error`).
fn is_file_line_error(line: &str) -> bool {
    let mut parts = line.splitn(3, ':');
    let (Some(file), Some(number), Some(_rest)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let known_ext = [".tex", ".sty", ".cls", ".bib", ".bbl", ".ltx", ".def"]
        .iter()
        .any(|ext| file.ends_with(ext));
    known_ext && !number.is_empty() && number.chars().all(|c| c.is_ascii_digit())
}

/// `true` für Zeilen, die für die Diagnose zählen.
fn is_relevant_log_line(line: &str) -> bool {
    const MARKERS: [&str; 11] = [
        "Undefined reference",
        "undefined references",
        "Missing character",
        "Emergency stop",
        "Fatal error",
        "LaTeX Error",
        "fontspec Error",
        "LaTeX Warning",
        "not found",
        "Please (re)run",
        "Rerun to get",
    ];
    line.starts_with('!') || is_file_line_error(line) || MARKERS.iter().any(|m| line.contains(m))
}

/// Kürzt eine Zeile auf [`MAX_EXCERPT_LINE_CHARS`] Zeichen.
fn clip_line(line: &str) -> String {
    if line.chars().count() <= MAX_EXCERPT_LINE_CHARS {
        return line.to_owned();
    }
    let mut clipped: String = line.chars().take(MAX_EXCERPT_LINE_CHARS).collect();
    clipped.push('…');
    clipped
}

/// Zieht die ersten Fehler und Warnungen aus einem TeX-Log.
///
/// # Beschreibung
/// Behält `! …`-Zeilen (samt der folgenden `l.<n>`-Zeile), `datei:zeile:`-
/// Zeilen, „Undefined reference“, „Missing character“ (fehlende Glyphen einer
/// Schrift), `fontspec`-Fehler, „not found“ (Schrift/Datei) sowie
/// `LaTeX Warning`/Rerun-Hinweise — in Log-Reihenfolge, ohne Duplikate,
/// höchstens [`MAX_EXCERPT_LINES`] Zeilen.
#[must_use]
pub fn extract_log_excerpt(log: &str) -> Vec<String> {
    let lines: Vec<&str> = log.lines().collect();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut excerpt = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if excerpt.len() >= MAX_EXCERPT_LINES {
            break;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() || !is_relevant_log_line(trimmed) || !seen.insert(trimmed) {
            continue;
        }
        excerpt.push(clip_line(trimmed));
        if trimmed.starts_with('!') {
            // Die Fundstelle `l.<n> …` folgt meist binnen weniger Zeilen.
            if let Some(location) = lines
                .iter()
                .skip(index + 1)
                .take(6)
                .map(|candidate| candidate.trim_end())
                .find(|candidate| candidate.starts_with("l."))
                && excerpt.len() < MAX_EXCERPT_LINES
                && seen.insert(location)
            {
                excerpt.push(clip_line(location));
            }
        }
    }
    excerpt
}

/// Die letzten [`OUTPUT_TAIL_LINES`] nicht leeren Zeilen der Prozessausgabe.
fn output_tail(capture: &BoundedCapture) -> String {
    let stdout = String::from_utf8_lossy(capture.stdout());
    let stderr = String::from_utf8_lossy(capture.stderr());
    let lines: Vec<&str> = stdout
        .lines()
        .chain(stderr.lines())
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(OUTPUT_TAIL_LINES);
    lines[start..]
        .iter()
        .map(|line| clip_line(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Liest die `.log`-Datei neben `file`, wenn sie in diesem Lauf entstand und
/// unter `root` liegt (höchstens [`LOG_READ_LIMIT`] Bytes).
fn read_fresh_log(root: &Path, file: &Path, started: SystemTime) -> Option<String> {
    let log = file.with_extension("log").canonicalize().ok()?;
    if !log.starts_with(root) {
        return None;
    }
    let metadata = std::fs::metadata(&log).ok()?;
    if !metadata.is_file() || metadata.modified().ok()? < started {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&log)
        .ok()?
        .take(LOG_READ_LIMIT)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Pfad relativ zur Workspace-Wurzel (für die Rückgabe).
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

// ── Ausführung ────────────────────────────────────────────────────────────────

/// Wie der Build-Prozess endete.
enum RunEnd {
    /// Regulär beendet (oder wegen Ausgabeüberlauf getötet).
    Finished(Option<ExitStatus>),
    /// Zeitlimit überschritten; Prozessbaum getötet.
    TimedOut,
}

/// Ausführer eines `latex.build`-Aufrufs; Konfiguration vom Provider.
struct LatexBuildExecutor {
    timeout_secs: u64,
    max_output_bytes: usize,
    limits: ShellLimits,
    search_path: Option<OsString>,
    extra_read_only: Vec<PathBuf>,
    #[cfg(test)]
    launch_directly: bool,
}

impl LatexBuildExecutor {
    /// Startet `command`, sammelt die Ausgabe gekappt und beachtet Zeitlimit
    /// und Abbruch.
    ///
    /// # Errors
    /// `Err(ToolOutput)` für Start-/I/O-Fehler, `Err(None)` bei Abbruch.
    async fn collect(
        &self,
        command: &mut TokioCommand,
        cancel: Option<&CancelToken>,
    ) -> Result<(RunEnd, BoundedCapture), Option<ToolOutput>> {
        let mut child = configure_stdio(command).spawn().map_err(|err| {
            warn!(error = %err, "latex.build spawn failed");
            Some(ToolOutput::error(format!(
                "latex.build: Start fehlgeschlagen: {err}"
            )))
        })?;
        let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take())
        else {
            terminate(&mut child).await;
            return Err(Some(ToolOutput::error(
                "latex.build: stdout/stderr-Pipes fehlen",
            )));
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(self.timeout_secs);
        let mut capture = BoundedCapture::new(self.max_output_bytes);
        let drained = match cancel {
            Some(cancel) => {
                tokio::select! {
                    result = tokio::time::timeout_at(deadline, capture.drain(&mut stdout, &mut stderr)) => result,
                    () = cancel.cancelled() => {
                        terminate(&mut child).await;
                        return Err(None);
                    }
                }
            }
            None => {
                tokio::time::timeout_at(deadline, capture.drain(&mut stdout, &mut stderr)).await
            }
        };
        match drained {
            Err(_elapsed) => {
                terminate(&mut child).await;
                Ok((RunEnd::TimedOut, capture))
            }
            Ok(Err(err)) => {
                terminate(&mut child).await;
                Err(Some(ToolOutput::error(format!(
                    "latex.build: I/O-Fehler: {err}"
                ))))
            }
            Ok(Ok(DrainEnd::LimitExceeded)) => {
                // latexmk selbst schreibt wenig; wer das Budget sprengt, läuft
                // aus dem Ruder — Baum beenden statt bis zum Zeitlimit warten.
                let status = terminate(&mut child).await;
                Ok((RunEnd::Finished(status), capture))
            }
            Ok(Ok(DrainEnd::Eof)) => {
                let waited = match cancel {
                    Some(cancel) => {
                        tokio::select! {
                            result = tokio::time::timeout_at(deadline, child.wait()) => result,
                            () = cancel.cancelled() => {
                                terminate(&mut child).await;
                                return Err(None);
                            }
                        }
                    }
                    None => tokio::time::timeout_at(deadline, child.wait()).await,
                };
                match waited {
                    Ok(Ok(status)) => Ok((RunEnd::Finished(Some(status)), capture)),
                    Ok(Err(err)) => {
                        terminate(&mut child).await;
                        Err(Some(ToolOutput::error(format!(
                            "latex.build: Warten fehlgeschlagen: {err}"
                        ))))
                    }
                    Err(_elapsed) => {
                        terminate(&mut child).await;
                        Ok((RunEnd::TimedOut, capture))
                    }
                }
            }
        }
    }

    /// Baut den Startbefehl: `prlimit … -- bwrap <plan> -- latexmk <args>`.
    ///
    /// # Errors
    /// Eine Fehlermeldung, wenn Limits, `bwrap` oder der Plan scheitern — es
    /// gibt keinen Rückfall auf den Host.
    fn sandboxed_command(
        &self,
        sandbox: &SandboxSpec,
        toolchain: &Toolchain,
        args: &[OsString],
    ) -> Result<TokioCommand, String> {
        self.limits
            .validate()
            .map_err(|err| format!("latex.build: Ressourcengrenzen: {err}"))?;
        let tmpfs_size = self
            .limits
            .tmpfs_size()
            .map_err(|err| format!("latex.build: Ressourcengrenzen: {err}"))?;
        let prlimit = self
            .limits
            .resolve_prlimit()
            .map_err(|err| format!("latex.build: Ressourcengrenzen: {err}"))?;
        let launcher = BwrapLauncher::discover()
            .map_err(|err| format!("latex.build: Sandbox nicht verfügbar: {err}"))?;
        let mut read_only: Vec<PathBuf> = TEX_READ_ONLY_ROOTS.iter().map(PathBuf::from).collect();
        read_only.extend(toolchain.install_roots.iter().cloned());
        read_only.extend(self.extra_read_only.iter().cloned());
        let launcher = launcher
            .with_tmpfs_size(tmpfs_size)
            .with_host_path(HostPathBinding {
                path: toolchain.sandbox_path.clone(),
                ..HostPathBinding::default()
            })
            .with_read_only_paths(read_only);
        let mut command_line = vec![toolchain.latexmk.as_os_str().to_owned()];
        command_line.extend(args.iter().cloned());
        let plan = launcher
            .plan(sandbox, &command_line)
            .map_err(|err| format!("latex.build: Sandbox-Plan abgelehnt: {err}"))?;
        let launch = launch_command(
            prlimit.as_deref(),
            &self.limits,
            launcher.executable(),
            plan.args(),
        );
        let mut command = TokioCommand::new(&launch.program);
        command.args(&launch.args);
        Ok(command)
    }

    /// Der eigentliche Ablauf nach dem Parsen.
    async fn run(
        &self,
        args: LatexBuildArgs,
        sandbox: &SandboxSpec,
        cancel: Option<&CancelToken>,
    ) -> Result<ToolOutput, ToolsError> {
        let root = sandbox.workspace().canonical_root().to_path_buf();
        let file = match resolve_tex_file(&root, &args.file) {
            Ok(file) => file,
            Err(message) => return Ok(ToolOutput::error(format!("latex.build: {message}"))),
        };
        let engine = args.engine.unwrap_or_default();
        let clean = args.clean.unwrap_or(false);

        let search_path = self
            .search_path
            .clone()
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_default();
        let toolchain = match resolve_toolchain(&search_path, engine, clean) {
            Ok(toolchain) => toolchain,
            Err(missing) => {
                info!(?missing, "latex.build: LaTeX nicht installiert");
                return Ok(not_installed_output(&missing));
            }
        };

        let latexmk_argv = latexmk_args(engine, clean, &file);
        #[cfg(test)]
        let prepared = if self.launch_directly {
            let mut command = TokioCommand::new(&toolchain.latexmk);
            command
                .args(&latexmk_argv)
                .current_dir(&root)
                .env("PATH", &toolchain.sandbox_path);
            Ok(command)
        } else {
            self.sandboxed_command(sandbox, &toolchain, &latexmk_argv)
        };
        #[cfg(not(test))]
        let prepared = self.sandboxed_command(sandbox, &toolchain, &latexmk_argv);
        let mut command = match prepared {
            Ok(command) => command,
            Err(message) => {
                warn!(%message, "latex.build: Sandbox-Start nicht möglich");
                return Ok(ToolOutput::error(message));
            }
        };

        let started = SystemTime::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let (end, capture) = match self.collect(&mut command, cancel).await {
            Ok(result) => result,
            Err(Some(output)) => return Ok(output),
            Err(None) => return Err(ToolsError::Cancelled),
        };

        let tail = output_tail(&capture);
        let truncated = capture.limit_exceeded();
        let log_excerpt = read_fresh_log(&root, &file, started)
            .map(|log| extract_log_excerpt(&log))
            .unwrap_or_else(|| extract_log_excerpt(&tail));
        let file_rel = relative(&root, &file);
        let mut result = BTreeMap::<&str, Value>::new();
        result.insert("engine", json!(engine.binary()));
        result.insert("file", json!(file_rel));
        result.insert("output_tail", json!(tail));
        result.insert("truncated", json!(truncated));
        match end {
            RunEnd::TimedOut => {
                warn!(timeout_secs = self.timeout_secs, "latex.build timed out");
                result.insert("status", json!("timeout"));
                result.insert("timeout_secs", json!(self.timeout_secs));
                result.insert("log_excerpt", json!(log_excerpt));
            }
            RunEnd::Finished(status) => {
                let exit_code = status.and_then(|status| status.code()).unwrap_or(-1);
                let ok = exit_code == 0 && !truncated;
                result.insert("exit_code", json!(exit_code));
                result.insert("log_excerpt", json!(log_excerpt));
                if clean {
                    result.insert("status", json!(if ok { "cleaned" } else { "failed" }));
                } else {
                    let pdf = file.with_extension("pdf");
                    let pdf_rel = (ok && pdf.is_file()).then(|| relative(&root, &pdf));
                    result.insert("status", json!(if ok { "ok" } else { "failed" }));
                    result.insert("pdf", json!(pdf_rel));
                }
                info!(exit_code, clean, "latex.build completed");
            }
        }
        Ok(ToolOutput::json(json!(result)))
    }
}

impl ToolExecutor for LatexBuildExecutor {
    /// Führt `latex.build` aus.
    ///
    /// # Beschreibung
    /// 1. Argumente parsen (unbekannte Felder werden abgelehnt).
    /// 2. `ExecuteProcess` und `WriteWorkspace` prüfen.
    /// 3. Datei kanonisieren und gegen die Workspace-Grenze prüfen.
    /// 4. `latexmk`/Engine suchen; fehlt etwas → `not_installed`.
    /// 5. In der Bubblewrap-Sandbox bauen und Log auswerten.
    ///
    /// # Errors
    /// [`ToolsError::InvalidArguments`] bei unpassenden Argumenten,
    /// [`ToolsError::Cancelled`] bei Abbruch. Alles andere kommt als
    /// [`ToolOutput`] zurück.
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let args: LatexBuildArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|err| {
                    ToolsError::InvalidArguments {
                        name: LATEX_BUILD_TOOL.to_owned(),
                        reason: err.to_string(),
                    }
                })?;
            for permission in [Permission::ExecuteProcess, Permission::WriteWorkspace] {
                if let Some(denied) = harw_tools::sandbox_guard::require_permission(
                    context,
                    permission,
                    LATEX_BUILD_TOOL,
                ) {
                    warn!(?permission, "latex.build denied");
                    return Ok(denied);
                }
            }
            self.run(args, context.sandbox(), context.cancel()).await
        })
    }
}

// ── Provider ──────────────────────────────────────────────────────────────────

/// Registriert das Werkzeug `latex.build`.
///
/// # Beschreibung
/// Vorgaben: Zeitlimit 120 s, Ausgabebudget 64 KiB, [`ShellLimits`] mit
/// 120 s CPU-Zeit und 4 GiB Adressraum (TeX-Läufe mit großen Schriften).
/// Die Programmsuche nutzt den `PATH` des Harness-Prozesses.
///
/// # Nebenläufigkeit
/// `Send + Sync`; [`ToolProvider::parallel_safe`] ist `false` (Builds
/// schreiben in dieselben Hilfsdateien).
///
/// # Beispiele
/// ```rust
/// use harw_extension_api::contributors::ToolProvider;
/// use harw_tool_shell::LatexToolProvider;
/// use harw_tools::spec::ToolName;
///
/// let provider = LatexToolProvider::new();
/// assert_eq!(provider.tools().len(), 1);
/// assert!(provider.executor(&ToolName::new("latex.build")).is_some());
/// ```
pub struct LatexToolProvider {
    /// Zeitlimit eines Aufrufs in Sekunden.
    pub timeout_secs: u64,
    /// Gemeinsames Budget für stdout+stderr in Bytes.
    pub max_output_bytes: usize,
    /// rlimits und tmpfs-Größe je Aufruf.
    pub limits: ShellLimits,
    search_path: Option<OsString>,
    extra_read_only: Vec<PathBuf>,
    #[cfg(test)]
    launch_directly: bool,
}

impl LatexToolProvider {
    /// Neuer Provider mit den Vorgaben (siehe Typdoku).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Setzt den Suchpfad für `latexmk` und die Engine (statt `PATH` des
    /// Prozesses) — für Betreiber mit TeX außerhalb des `PATH` und für Tests.
    #[must_use]
    pub fn with_search_path(mut self, search_path: impl Into<OsString>) -> Self {
        self.search_path = Some(search_path.into());
        self
    }

    /// Weitere, nur lesend gebundene Pfade (z. B. eine eigene
    /// Schriftensammlung außerhalb von `/usr`).
    #[must_use]
    pub fn with_read_only_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.extra_read_only = paths;
        self
    }

    /// Das Parameterschema: `file` (Pflicht), `engine` (Enum), `clean`.
    fn parameter_schema() -> JsonSchema {
        let mut properties = BTreeMap::new();
        properties.insert(
            "file".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::String),
                description: Some(
                    "Path of the .tex file inside the workspace (relative to the workspace root)."
                        .to_owned(),
                ),
                ..Default::default()
            },
        );
        properties.insert(
            "engine".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::String),
                description: Some("TeX engine; default xelatex.".to_owned()),
                enum_values: Some(vec![json!("xelatex"), json!("pdflatex"), json!("lualatex")]),
                ..Default::default()
            },
        );
        properties.insert(
            "clean".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::Boolean),
                description: Some(
                    "true removes auxiliary files (latexmk -c) instead of building.".to_owned(),
                ),
                ..Default::default()
            },
        );
        JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(properties),
            required: Some(vec!["file".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        }
    }
}

impl Default for LatexToolProvider {
    fn default() -> Self {
        Self {
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            limits: ShellLimits {
                cpu_secs: DEFAULT_TIMEOUT_SECS,
                as_bytes: 4 * 1024 * 1024 * 1024,
                ..ShellLimits::default()
            },
            search_path: None,
            extra_read_only: Vec::new(),
            #[cfg(test)]
            launch_directly: false,
        }
    }
}

impl ToolProvider for LatexToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new(LATEX_BUILD_TOOL),
            description: "Build a LaTeX document with latexmk inside the isolated project \
                sandbox (no network, no shell escape, .latexmkrc ignored). Only the .tex file, \
                the engine (xelatex|pdflatex|lualatex) and clean are selectable. Returns status \
                (ok|failed|timeout|cleaned|not_installed), the PDF path and the first log errors. \
                On not_installed stop and pass user_message to the user verbatim. \
                Requires ExecuteProcess and WriteWorkspace."
                .to_owned(),
            parameters: Self::parameter_schema(),
            strict: true,
        })]
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        (name.as_str() == LATEX_BUILD_TOOL).then(|| {
            Arc::new(LatexBuildExecutor {
                timeout_secs: self.timeout_secs,
                max_output_bytes: self.max_output_bytes,
                limits: self.limits,
                search_path: self.search_path.clone(),
                extra_read_only: self.extra_read_only.clone(),
                #[cfg(test)]
                launch_directly: self.launch_directly,
            }) as Arc<dyn ToolExecutor>
        })
    }

    fn parallel_safe(&self, _name: &ToolName) -> bool {
        false
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn sandbox(dir: &TempDir, permissions: &[Permission]) -> TestResult<SandboxSpec> {
        let workspace = dir.path().join("project");
        fs::create_dir_all(&workspace).map_err(ctx("workspace"))?;
        let registry = WorkspaceRegistry::build(
            dir.path(),
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tenant"),
                workspace: WorkspaceId::from_str("project"),
                root: workspace,
            }],
        )
        .map_err(ctx("registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("tenant"),
                &WorkspaceId::from_str("project"),
            )
            .map_err(ctx("binding"))?;
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy(permissions.iter().copied()),
        ))
    }

    fn full_rights() -> [Permission; 3] {
        [
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
        ]
    }

    fn call(arguments: Value) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(LATEX_BUILD_TOOL),
            arguments,
        }
    }

    /// Legt ein ausführbares Shell-Skript `name` in `dir` an.
    fn fake_binary(dir: &Path, name: &str, body: &str) -> TestResult {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).map_err(ctx("fake binary"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .map_err(ctx("fake binary mode"))?;
        Ok(())
    }

    async fn run(
        provider: &LatexToolProvider,
        spec: SandboxSpec,
        arguments: Value,
    ) -> TestResult<ToolOutput> {
        let context = ToolExecutionContext::new(SessionId::new(), TurnId::new(), spec);
        let executor = provider
            .executor(&ToolName::new(LATEX_BUILD_TOOL))
            .ok_or(TestError::Missing("executor"))?;
        executor
            .execute(&context, &call(arguments))
            .await
            .map_err(ctx("execute"))
    }

    fn json_of(output: &ToolOutput) -> TestResult<&Value> {
        match output {
            ToolOutput::Json { content } => Ok(content),
            other => Err(TestError::Unexpected(format!("kein JSON: {other:?}"))),
        }
    }

    #[test]
    fn test_argv_is_fixed_without_shell_escape_and_without_rc_files() {
        let file = Path::new("/ws/doc/main.tex");
        let args: Vec<String> = latexmk_args(LatexEngine::Xelatex, false, file)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-norc",
                "-xelatex",
                "-interaction=nonstopmode",
                "-halt-on-error",
                "-file-line-error",
                "-no-shell-escape",
                "-cd",
                "/ws/doc/main.tex",
            ]
        );
        assert!(!args.iter().any(|a| a == "-shell-escape"));
        let pdf: Vec<String> = latexmk_args(LatexEngine::Pdflatex, false, file)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(pdf.get(1).map(String::as_str), Some("-pdf"));
        let clean: Vec<String> = latexmk_args(LatexEngine::Lualatex, true, file)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(clean, ["-norc", "-c", "-cd", "/ws/doc/main.tex"]);
    }

    #[test]
    fn test_resolve_tex_file_enforces_workspace_and_extension() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let root = dir.path().join("ws");
        fs::create_dir_all(root.join("doc")).map_err(ctx("doc dir"))?;
        let root = root.canonicalize().map_err(ctx("canon"))?;
        fs::write(root.join("doc/main.tex"), "x").map_err(ctx("tex"))?;
        fs::write(root.join("doc/notes.txt"), "x").map_err(ctx("txt"))?;
        fs::write(dir.path().join("outside.tex"), "x").map_err(ctx("outside"))?;
        std::os::unix::fs::symlink(dir.path().join("outside.tex"), root.join("escape.tex"))
            .map_err(ctx("symlink"))?;

        let resolved = resolve_tex_file(&root, "doc/main.tex").map_err(TestError::Unexpected)?;
        assert_eq!(resolved, root.join("doc/main.tex"));
        assert!(resolve_tex_file(&root, "doc/notes.txt").is_err());
        assert!(resolve_tex_file(&root, "../outside.tex").is_err());
        assert!(
            resolve_tex_file(&root, "escape.tex").is_err(),
            "Symlink-Ausbruch"
        );
        assert!(resolve_tex_file(&root, "doc").is_err());
        assert!(resolve_tex_file(&root, "fehlt.tex").is_err());
        assert!(resolve_tex_file(&root, "  ").is_err());
        Ok(())
    }

    #[test]
    fn test_log_excerpt_keeps_first_errors_and_font_problems() {
        let log = "This is XeTeX\n\
                   ./main.tex:12: Undefined control sequence.\n\
                   l.12 \\foo\n\
                   ! LaTeX Error: File `missing.sty' not found.\n\
                   \n\
                   l.3 \\usepackage\n\
                   Missing character: There is no ß in font cmr10!\n\
                   Missing character: There is no ß in font cmr10!\n\
                   LaTeX Warning: There were undefined references.\n\
                   Output written on main.pdf\n";
        let excerpt = extract_log_excerpt(log);
        assert_eq!(
            excerpt,
            [
                "./main.tex:12: Undefined control sequence.",
                "! LaTeX Error: File `missing.sty' not found.",
                "l.3 \\usepackage",
                "Missing character: There is no ß in font cmr10!",
                "LaTeX Warning: There were undefined references.",
            ]
        );
    }

    #[test]
    fn test_not_installed_message_names_programs_and_hints() {
        let message = not_installed_message(&["latexmk".to_owned(), "xelatex".to_owned()]);
        for needle in [
            "latexmk, xelatex",
            "sudo apt install latexmk texlive-xetex texlive-lang-german",
            "sudo dnf install latexmk texlive-xetex",
            "sudo pacman -S texlive-binextra texlive-xetex",
            "brew install --cask mactex",
            "installiert nichts selbst",
        ] {
            assert!(message.contains(needle), "{needle}: {message}");
        }
    }

    #[tokio::test]
    async fn test_missing_latexmk_reports_not_installed_with_user_message() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(&dir, &full_rights())?;
        fs::write(spec.workspace().canonical_root().join("main.tex"), "x").map_err(ctx("tex"))?;
        let empty_bin = dir.path().join("empty-bin");
        fs::create_dir_all(&empty_bin).map_err(ctx("bin"))?;
        let provider = LatexToolProvider::new().with_search_path(empty_bin.as_os_str());
        let output = run(&provider, spec, json!({ "file": "main.tex" })).await?;
        let value = json_of(&output)?;
        assert_eq!(value["status"], "not_installed");
        assert_eq!(value["missing"], json!(["latexmk", "xelatex"]));
        let message = value["user_message"].as_str().unwrap_or_default();
        assert!(message.contains("latexmk, xelatex"), "{message}");
        Ok(())
    }

    #[tokio::test]
    async fn test_missing_engine_only_is_reported() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(&dir, &full_rights())?;
        fs::write(spec.workspace().canonical_root().join("main.tex"), "x").map_err(ctx("tex"))?;
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).map_err(ctx("bin"))?;
        fake_binary(&bin, "latexmk", "exit 0")?;
        let provider = LatexToolProvider::new().with_search_path(bin.as_os_str());
        let output = run(
            &provider,
            spec,
            json!({ "file": "main.tex", "engine": "lualatex" }),
        )
        .await?;
        let value = json_of(&output)?;
        assert_eq!(value["status"], "not_installed");
        assert_eq!(value["missing"], json!(["lualatex"]));
        Ok(())
    }

    #[tokio::test]
    async fn test_permissions_are_required() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(
            &dir,
            &[Permission::ReadWorkspace, Permission::WriteWorkspace],
        )?;
        let output = run(
            &LatexToolProvider::new(),
            spec,
            json!({ "file": "main.tex" }),
        )
        .await?;
        assert!(
            matches!(&output, ToolOutput::Error { message } if message.contains("ExecuteProcess")),
            "{output:?}"
        );
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(
            &dir,
            &[Permission::ReadWorkspace, Permission::ExecuteProcess],
        )?;
        let output = run(
            &LatexToolProvider::new(),
            spec,
            json!({ "file": "main.tex" }),
        )
        .await?;
        assert!(
            matches!(&output, ToolOutput::Error { message } if message.contains("WriteWorkspace")),
            "{output:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_unknown_arguments_are_rejected() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(&dir, &full_rights())?;
        let context = ToolExecutionContext::new(SessionId::new(), TurnId::new(), spec);
        let executor = LatexToolProvider::new()
            .executor(&ToolName::new(LATEX_BUILD_TOOL))
            .ok_or(TestError::Missing("executor"))?;
        for arguments in [
            json!({ "file": "main.tex", "args": ["-shell-escape"] }),
            json!({ "file": "main.tex", "engine": "sh" }),
        ] {
            let result = executor.execute(&context, &call(arguments)).await;
            assert!(
                matches!(result, Err(ToolsError::InvalidArguments { .. })),
                "{result:?}"
            );
        }
        Ok(())
    }

    /// Build-Pfad mit Fake-Binaries (ohne echtes TeX, ohne Sandbox): das
    /// Fake-`latexmk` protokolliert sein argv, schreibt Log und PDF.
    #[tokio::test]
    async fn test_fake_binaries_build_path_reports_pdf_and_log_excerpt() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(&dir, &full_rights())?;
        let root = spec.workspace().canonical_root().to_path_buf();
        fs::create_dir_all(root.join("doc")).map_err(ctx("doc"))?;
        fs::write(root.join("doc/main.tex"), "x").map_err(ctx("tex"))?;
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).map_err(ctx("bin"))?;
        let argv_log = dir.path().join("argv.txt");
        fake_binary(&bin, "xelatex", "exit 0")?;
        fake_binary(
            &bin,
            "latexmk",
            &format!(
                "for a in \"$@\"; do echo \"$a\" >> '{argv}'; done\n\
                 for a in \"$@\"; do f=\"$a\"; done\n\
                 base=\"${{f%.tex}}\"\n\
                 printf 'LaTeX Warning: Citation `x` undefined.\\n' > \"$base.log\"\n\
                 printf '%%PDF-1.5' > \"$base.pdf\"\n\
                 echo 'Latexmk: All targets are up-to-date'",
                argv = argv_log.display()
            ),
        )?;
        let mut provider = LatexToolProvider::new().with_search_path(bin.as_os_str());
        provider.launch_directly = true;
        let output = run(&provider, spec, json!({ "file": "doc/main.tex" })).await?;
        let value = json_of(&output)?;
        assert_eq!(value["status"], "ok", "{value}");
        assert_eq!(value["pdf"], "doc/main.pdf");
        assert_eq!(value["engine"], "xelatex");
        assert_eq!(
            value["log_excerpt"],
            json!(["LaTeX Warning: Citation `x` undefined."])
        );
        let argv = fs::read_to_string(&argv_log).map_err(ctx("argv"))?;
        assert!(argv.contains("-no-shell-escape"), "{argv}");
        assert!(argv.contains("-norc"), "{argv}");
        Ok(())
    }

    #[tokio::test]
    async fn test_fake_binaries_failed_build_reports_failed_without_pdf() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(&dir, &full_rights())?;
        let root = spec.workspace().canonical_root().to_path_buf();
        fs::write(root.join("main.tex"), "x").map_err(ctx("tex"))?;
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).map_err(ctx("bin"))?;
        fake_binary(&bin, "xelatex", "exit 0")?;
        fake_binary(
            &bin,
            "latexmk",
            "echo './main.tex:3: Undefined control sequence.'\nexit 12",
        )?;
        let mut provider = LatexToolProvider::new().with_search_path(bin.as_os_str());
        provider.launch_directly = true;
        let output = run(&provider, spec, json!({ "file": "main.tex" })).await?;
        let value = json_of(&output)?;
        assert_eq!(value["status"], "failed", "{value}");
        assert_eq!(value["exit_code"], 12);
        assert_eq!(value["pdf"], Value::Null);
        assert_eq!(
            value["log_excerpt"],
            json!(["./main.tex:3: Undefined control sequence."])
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_timeout_kills_the_build() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(&dir, &full_rights())?;
        fs::write(spec.workspace().canonical_root().join("main.tex"), "x").map_err(ctx("tex"))?;
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).map_err(ctx("bin"))?;
        fake_binary(&bin, "xelatex", "exit 0")?;
        fake_binary(&bin, "latexmk", "exec sleep 30")?;
        let mut provider = LatexToolProvider::new().with_search_path(bin.as_os_str());
        provider.launch_directly = true;
        provider.timeout_secs = 1;
        let output = run(&provider, spec, json!({ "file": "main.tex" })).await?;
        let value = json_of(&output)?;
        assert_eq!(value["status"], "timeout", "{value}");
        Ok(())
    }

    #[test]
    fn test_toolchain_path_and_install_root() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let bin = dir.path().join("texlive/2025/bin/x86_64-linux");
        fs::create_dir_all(&bin).map_err(ctx("bin"))?;
        fake_binary(&bin, "latexmk", "exit 0")?;
        fake_binary(&bin, "xelatex", "exit 0")?;
        let toolchain = resolve_toolchain(bin.as_os_str(), LatexEngine::Xelatex, false)
            .map_err(|missing| TestError::Unexpected(format!("{missing:?}")))?;
        assert_eq!(toolchain.latexmk, bin.join("latexmk"));
        assert!(toolchain.sandbox_path.ends_with(MINIMAL_PATH));
        let root = dir
            .path()
            .join("texlive/2025")
            .canonicalize()
            .map_err(ctx("canon"))?;
        assert_eq!(toolchain.install_roots, vec![root]);
        // Aufräumen braucht die Engine nicht.
        let clean_bin = dir.path().join("clean-bin");
        fs::create_dir_all(&clean_bin).map_err(ctx("clean bin"))?;
        fake_binary(&clean_bin, "latexmk", "exit 0")?;
        assert!(resolve_toolchain(clean_bin.as_os_str(), LatexEngine::Xelatex, true).is_ok());
        Ok(())
    }

    #[test]
    fn test_provider_lists_one_strict_tool_and_is_not_parallel_safe() {
        let provider = LatexToolProvider::new();
        let tools = provider.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), LATEX_BUILD_TOOL);
        assert!(!provider.parallel_safe(&ToolName::new(LATEX_BUILD_TOOL)));
        assert!(provider.executor(&ToolName::new("shell.exec")).is_none());
    }

    /// `true`, wenn `prlimit` + `bwrap` mit User-Namespace hier startbar sind
    /// (dieselbe Probe wie die Sandbox-Tests von `shell.exec`).
    fn sandbox_runtime_available() -> bool {
        let Ok(launcher) = BwrapLauncher::discover() else {
            return false;
        };
        let Ok(Some(prlimit)) = ShellLimits::default().resolve_prlimit() else {
            return false;
        };
        let mut args = ShellLimits::default().prlimit_args();
        args.push(launcher.executable().as_os_str().to_owned());
        args.extend(
            [
                "--die-with-parent",
                "--unshare-all",
                "--ro-bind",
                "/",
                "/",
                "--",
                "/bin/true",
            ]
            .map(OsString::from),
        );
        std::process::Command::new(prlimit)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Derselbe Build-Pfad wie oben, aber wirklich in der Bubblewrap-Sandbox:
    /// das Fake-`latexmk` liegt außerhalb von `/usr` (Bindung über den
    /// Sandbox-`PATH`) und schreibt das PDF in den beschreibbaren Workspace.
    /// Ohne startbare Sandbox wird der Test übersprungen.
    #[tokio::test]
    async fn test_fake_binaries_build_inside_the_sandbox() -> TestResult {
        if !sandbox_runtime_available() {
            eprintln!("übersprungen: bwrap/prlimit/userns nicht startbar");
            return Ok(());
        }
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(&dir, &full_rights())?;
        let root = spec.workspace().canonical_root().to_path_buf();
        fs::write(root.join("main.tex"), "x").map_err(ctx("tex"))?;
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).map_err(ctx("bin"))?;
        fake_binary(&bin, "xelatex", "exit 0")?;
        fake_binary(
            &bin,
            "latexmk",
            "for a in \"$@\"; do f=\"$a\"; done\n\
             printf '%%PDF-1.5' > \"${f%.tex}.pdf\"",
        )?;
        let provider = LatexToolProvider::new().with_search_path(bin.as_os_str());
        let output = run(&provider, spec, json!({ "file": "main.tex" })).await?;
        let value = json_of(&output)?;
        assert_eq!(value["status"], "ok", "{value}");
        assert_eq!(value["pdf"], "main.pdf");
        Ok(())
    }
}
