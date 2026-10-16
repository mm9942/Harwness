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
//!
//! # Runde 7, Teil T
//! - **Rückfall ohne `latexmk` (T3):** fehlt `latexmk`, aber die Engine ist
//!   da, läuft die Engine direkt (`-interaction=nonstopmode -halt-on-error
//!   -file-line-error -no-shell-escape <datei>.tex`, Arbeitsverzeichnis =
//!   Ordner der Datei), dazwischen `biber`, wenn eine `.bcf` entstand und
//!   `biber` vorhanden ist, danach die Engine ein zweites Mal (ein dritter
//!   Lauf nur bei „Rerun“-Hinweis). Alle Läufe teilen sich eine Deadline.
//!   Das Ergebnis nennt `builder` (`latexmk`|`direct`), die Seitenzahl,
//!   Overfull-Boxen über 1 pt, die Zahl der Underfull-Boxen, fehlende
//!   Zeichen und Trennmuster-Warnungen ([`log`]); `status =
//!   "ok_with_warnings"`, wenn davon etwas zutrifft.
//! - **`latex.template` (T2, [`template`]):** schreibt `harw-report.sty` und
//!   das Gerüst der Hauptdatei (nie überschreibend, `WriteWorkspace`).
//! - **`latex.check` (T4, [`check`]):** Vorabprüfung von Klasse, Paketen,
//!   Schriften und Sprachen per `kpsewhich`/`fc-list` in derselben Sandbox
//!   (`ExecuteProcess`).

pub mod check;
pub mod log;
pub mod template;

pub use self::log::{LogReport, OverfullBox, parse_build_log};
pub use check::LATEX_CHECK_TOOL;
pub use template::{LATEX_TEMPLATE_TOOL, TemplateKind, TemplateLanguage};

use crate::capture::BoundedCapture;
use crate::limits::{ShellLimits, launch_command};
use harw_authority::{Permission, SandboxSpec};
use harw_command::{CommandEnd, CommandRequest, CommandSandbox, Persistence};
use harw_sandbox::{BwrapLauncher, HostPathBinding};
use harw_tools::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolOutput, ToolsError,
    schema::{AdditionalProperties, JsonSchema, JsonSchemaType},
    spec::{FunctionToolSpec, ToolName, ToolSpec},
};
use harw_types::cancel::CancelToken;
use serde::Deserialize;
use serde_json::{Value, json};
use std::os::unix::process::ExitStatusExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    io::Read,
    path::{Path, PathBuf},
    process::ExitStatus,
    time::{Duration, SystemTime},
};
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
/// Das bevorzugte Build-Programm (Bauen und Aufräumen). Fehlt es, baut
/// `latex.build` direkt mit der Engine (Runde 7, Teil T3).
const LATEXMK: &str = "latexmk";
/// Bibliographie-Programm für biblatex (nur im direkten Rückfall).
const BIBER: &str = "biber";
/// Höchstzahl der Engine-Läufe im direkten Rückfall (zwei feste Läufe plus
/// einer bei „Rerun“-Hinweis).
const MAX_DIRECT_ENGINE_RUNS: usize = 3;

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

/// Baut das feste argv (ohne Programm) für einen direkten Engine-Lauf
/// (Rückfall ohne `latexmk`, Runde 7 Teil T3).
///
/// # Beschreibung
/// Rein und ohne Prozessstart testbar. Es gibt kein `-cd` wie bei `latexmk`:
/// der Lauf startet stattdessen im Ordner der Datei, deshalb steht hier nur
/// der Dateiname.
///
/// # Argumente
/// - `file_name` (`&OsStr`): Dateiname der `.tex`-Datei (ohne Ordner).
///
/// # Rückgabe
/// Die Argumente in Startreihenfolge.
#[must_use]
pub fn direct_engine_args(file_name: &OsStr) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-interaction=nonstopmode",
        "-halt-on-error",
        "-file-line-error",
        "-no-shell-escape",
    ]
    .map(OsString::from)
    .to_vec();
    args.push(file_name.to_owned());
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

/// Sandbox-Umgebung für gefundene Programme: `PATH` und zusätzliche, nur
/// lesend gebundene Installationswurzeln.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProgramEnv {
    /// `PATH` für die Sandbox: Programmverzeichnisse plus [`MINIMAL_PATH`].
    sandbox_path: String,
    /// Zusätzliche, nur lesende Wurzeln (TeX-Live außerhalb von `/usr`).
    install_roots: Vec<PathBuf>,
}

impl ProgramEnv {
    /// Baut `PATH` und Installationswurzeln aus den gefundenen Programmen.
    ///
    /// # Argumente
    /// - `programs` (`&[&Path]`): absolute Pfade der gefundenen Programme.
    ///
    /// # Rückgabe
    /// Die Umgebung; Verzeichnisse ohne Duplikate in Fundreihenfolge.
    fn of(programs: &[&Path]) -> Self {
        let mut dirs: Vec<String> = Vec::new();
        let mut install_roots: Vec<PathBuf> = Vec::new();
        for path in programs {
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
        Self {
            sandbox_path: dirs.join(":"),
            install_roots,
        }
    }
}

/// Womit gebaut wird (Runde 7, Teil T3).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Builder {
    /// `latexmk` (bevorzugt; beim Aufräumen Pflicht).
    Latexmk(PathBuf),
    /// Rückfall ohne `latexmk`: die Engine direkt, `biber` falls vorhanden.
    Direct {
        /// Absoluter Pfad der Engine.
        engine: PathBuf,
        /// Absoluter Pfad von `biber`, falls gefunden.
        biber: Option<PathBuf>,
    },
}

impl Builder {
    /// Name für das Ergebnisfeld `builder`.
    const fn label(&self) -> &'static str {
        match self {
            Self::Latexmk(_) => "latexmk",
            Self::Direct { .. } => "direct",
        }
    }
}

/// Ergebnis der Programmsuche.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Toolchain {
    /// Build-Weg samt Programmpfaden.
    builder: Builder,
    /// Sandbox-Umgebung der gefundenen Programme.
    env: ProgramEnv,
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

/// Sucht `latexmk`, die Engine und (im Rückfall) `biber`.
///
/// # Beschreibung
/// - Aufräumen (`clean`) braucht `latexmk`; ohne gibt es keinen Rückfall.
/// - Bauen braucht die Engine. Mit `latexmk` baut [`Builder::Latexmk`],
///   ohne `latexmk` baut [`Builder::Direct`] (Runde 7, Teil T3); `biber`
///   ist dort optional.
///
/// # Rückgabe
/// `Ok(toolchain)` oder `Err(missing)` mit den fehlenden Programmnamen in
/// fester Reihenfolge (`latexmk` zuerst, falls es mitfehlt).
fn resolve_toolchain(
    search_path: &OsStr,
    engine: LatexEngine,
    clean: bool,
) -> Result<Toolchain, Vec<String>> {
    let latexmk = find_in_path(search_path, LATEXMK);
    if clean {
        let Some(latexmk) = latexmk else {
            return Err(vec![LATEXMK.to_owned()]);
        };
        let env = ProgramEnv::of(&[latexmk.as_path()]);
        return Ok(Toolchain {
            builder: Builder::Latexmk(latexmk),
            env,
        });
    }
    let Some(engine_path) = find_in_path(search_path, engine.binary()) else {
        let mut missing = Vec::new();
        if latexmk.is_none() {
            missing.push(LATEXMK.to_owned());
        }
        missing.push(engine.binary().to_owned());
        return Err(missing);
    };
    match latexmk {
        Some(latexmk) => {
            let env = ProgramEnv::of(&[latexmk.as_path(), engine_path.as_path()]);
            Ok(Toolchain {
                builder: Builder::Latexmk(latexmk),
                env,
            })
        }
        None => {
            let biber = find_in_path(search_path, BIBER);
            let mut programs = vec![engine_path.as_path()];
            if let Some(biber) = &biber {
                programs.push(biber.as_path());
            }
            let env = ProgramEnv::of(&programs);
            Ok(Toolchain {
                builder: Builder::Direct {
                    engine: engine_path,
                    biber,
                },
                env,
            })
        }
    }
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
    const MARKERS: [&str; 16] = [
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
        // Runde 7, Teil T3: Sprach-, Schrift- und Bibliographie-Hinweise.
        "hyphenation patterns",
        "babel Warning",
        "polyglossia Warning",
        "fontspec Warning",
        "Font shape",
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
/// Schrift), `fontspec`-Fehler und -Warnungen, „Font shape“, „not found“
/// (Schrift/Datei), babel-/polyglossia-Warnungen und fehlende Trennmuster
/// (Runde 7, Teil T3) sowie `LaTeX Warning`/Rerun-Hinweise
/// — in Log-Reihenfolge, ohne Duplikate, höchstens [`MAX_EXCERPT_LINES`]
/// Zeilen. Overfull-/Underfull-Boxen stehen nicht im Auszug, sondern
/// strukturiert im Ergebnis ([`parse_build_log`]).
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

/// Ein fertig geplanter Start (bwrap-argv bzw. Test-Direktstart).
struct PreparedLaunch {
    program: OsString,
    args: Vec<OsString>,
    cwd: PathBuf,
    /// Zusätzliche Variablen über der geerbten Umgebung.
    env: Vec<(String, String)>,
}

/// Wie ein Prozess endete.
enum RunEnd {
    /// Regulär beendet (oder wegen Ausgabeüberlauf getötet).
    Finished(Option<ExitStatus>),
    /// Zeitlimit überschritten; Prozessbaum getötet.
    TimedOut,
}

impl RunEnd {
    /// `true` für einen regulären Lauf mit Exit-Code 0.
    fn succeeded(&self) -> bool {
        matches!(self, Self::Finished(Some(status)) if status.success())
    }
}

/// Ein fester Programmaufruf: absolutes Programm, argv, Arbeitsverzeichnis
/// (Runde 7, Teil T3: verallgemeinert von „immer `latexmk`“).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProgramCall {
    /// Absoluter Pfad des Programms.
    program: PathBuf,
    /// Argumente ohne Programm.
    args: Vec<OsString>,
    /// Arbeitsverzeichnis: die Workspace-Wurzel oder ein Ordner darunter.
    cwd: PathBuf,
}

/// Ersetzt das Ziel von `--chdir` in einem Bubblewrap-argv.
///
/// # Beschreibung
/// Der Sandbox-Plan wechselt immer in die Workspace-Wurzel. Der direkte
/// Engine-Lauf (Runde 7, Teil T3) braucht den Ordner der `.tex`-Datei als
/// Arbeitsverzeichnis, damit relative `\input`-Pfade und die Hilfsdateien
/// wie bei `latexmk -cd` funktionieren. Gesucht wird nur vor dem Trenner
/// `--` (danach folgt das Programm-argv).
///
/// # Argumente
/// - `args` (`&[OsString]`): das argv des Plans (ohne `bwrap` selbst).
/// - `dir` (`&Path`): neues Arbeitsverzeichnis (kanonisch, im Workspace).
///
/// # Rückgabe
/// Das geänderte argv oder `None`, wenn kein `--chdir` vor `--` steht.
fn retarget_chdir(args: &[OsString], dir: &Path) -> Option<Vec<OsString>> {
    let separator = args.iter().position(|arg| arg == "--")?;
    let chdir = args[..separator].iter().rposition(|arg| arg == "--chdir")?;
    if chdir + 1 >= separator {
        return None;
    }
    let mut retargeted = args.to_vec();
    retargeted[chdir + 1] = dir.as_os_str().to_owned();
    Some(retargeted)
}

/// Gemeinsame Startkonfiguration der LaTeX-Werkzeuge mit Prozessstart
/// (`latex.build`, `latex.check`): Ausgabebudget, rlimits, zusätzliche
/// Lesepfade.
#[derive(Clone)]
struct SandboxRunner {
    max_output_bytes: usize,
    limits: ShellLimits,
    extra_read_only: Vec<PathBuf>,
    #[cfg(test)]
    launch_directly: bool,
}

impl SandboxRunner {
    /// Führt `launch` über den Command-Port der Job-Runtime aus, sammelt die
    /// Ausgabe gekappt und beachtet die gemeinsame `deadline` und den Abbruch.
    /// Start, Prozessgruppe, Frist und Kill gehören der Runtime (PL-93).
    ///
    /// # Argumente
    /// - `tool` (`&'static str`): Werkzeugname für Meldungen.
    /// - `launch` (`&PreparedLaunch`): der vorbereitete Befehl.
    /// - `cancel` (`Option<&CancelToken>`): Abbruchsignal.
    /// - `deadline` (`tokio::time::Instant`): gemeinsame Frist aller Läufe
    ///   eines Aufrufs (Runde 7, Teil T3).
    ///
    /// # Errors
    /// `Err(Some(ToolOutput))` für Start-/I/O-Fehler, `Err(None)` bei Abbruch.
    async fn collect(
        &self,
        tool: &'static str,
        launch: &PreparedLaunch,
        cancel: Option<&CancelToken>,
        deadline: tokio::time::Instant,
    ) -> Result<(RunEnd, BoundedCapture), Option<ToolOutput>> {
        let Some(port) = harw_command::installed() else {
            return Err(Some(ToolOutput::error(format!(
                "{tool}: keine Job-Runtime montiert; Programme laufen nur über die Job-Runtime"
            ))));
        };
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok((RunEnd::TimedOut, BoundedCapture::new(self.max_output_bytes)));
        }
        let mut request = CommandRequest::new(
            launch.program.to_string_lossy().into_owned(),
            &launch.cwd,
            remaining,
        );
        request.args = launch
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        request.env = std::env::vars().collect();
        for (key, value) in &launch.env {
            request.env.retain(|(name, _)| name != key);
            request.env.push((key.clone(), value.clone()));
        }
        request.max_output_bytes = self.max_output_bytes;
        request.sandbox = CommandSandbox::Host;
        request.persistence = Persistence::Ephemeral;

        let done = port.run(request, cancel.cloned().unwrap_or_default()).await;
        let exceeded = done.end == CommandEnd::OutputLimit;
        let capture = BoundedCapture::from_parts(
            done.stdout.clone(),
            done.stderr.clone(),
            self.max_output_bytes,
            exceeded,
        );
        match done.end {
            CommandEnd::Cancelled => Err(None),
            CommandEnd::TimedOut => Ok((RunEnd::TimedOut, capture)),
            // TeX-Programme schreiben begrenzt; wer das Budget sprengt, läuft
            // aus dem Ruder — die Runtime hat den Baum beendet.
            CommandEnd::OutputLimit => Ok((RunEnd::Finished(None), capture)),
            CommandEnd::Exited => Ok((
                RunEnd::Finished(Some(ExitStatus::from_raw(done.exit_code << 8))),
                capture,
            )),
            CommandEnd::Signaled(signal) => Ok((
                RunEnd::Finished(Some(ExitStatus::from_raw(signal))),
                capture,
            )),
            CommandEnd::Failed(message) => {
                warn!(error = %message, tool, "LaTeX-Werkzeug: Start fehlgeschlagen");
                Err(Some(ToolOutput::error(format!(
                    "{tool}: Start fehlgeschlagen: {message}"
                ))))
            }
        }
    }

    /// Baut den Startbefehl: `prlimit … -- bwrap <plan> -- <programm> <args>`.
    ///
    /// # Beschreibung
    /// Liegt `call.cwd` nicht auf der Workspace-Wurzel, wird das `--chdir`
    /// des Plans darauf umgelenkt ([`retarget_chdir`]); `cwd` muss dafür
    /// kanonisch unter der Workspace-Wurzel liegen (dort ist der Workspace
    /// unter demselben Pfad gebunden).
    ///
    /// # Errors
    /// Eine Fehlermeldung, wenn Limits, `bwrap`, der Plan oder das
    /// Arbeitsverzeichnis scheitern — es gibt keinen Rückfall auf den Host.
    fn sandboxed_command(
        &self,
        tool: &'static str,
        sandbox: &SandboxSpec,
        env: &ProgramEnv,
        call: &ProgramCall,
    ) -> Result<PreparedLaunch, String> {
        self.limits
            .validate()
            .map_err(|err| format!("{tool}: Ressourcengrenzen: {err}"))?;
        let tmpfs_size = self
            .limits
            .tmpfs_size()
            .map_err(|err| format!("{tool}: Ressourcengrenzen: {err}"))?;
        let prlimit = self
            .limits
            .resolve_prlimit()
            .map_err(|err| format!("{tool}: Ressourcengrenzen: {err}"))?;
        let launcher = BwrapLauncher::discover()
            .map_err(|err| format!("{tool}: Sandbox nicht verfügbar: {err}"))?;
        let mut read_only: Vec<PathBuf> = TEX_READ_ONLY_ROOTS.iter().map(PathBuf::from).collect();
        read_only.extend(env.install_roots.iter().cloned());
        read_only.extend(self.extra_read_only.iter().cloned());
        let launcher = launcher
            .with_tmpfs_size(tmpfs_size)
            .with_host_path(HostPathBinding {
                path: env.sandbox_path.clone(),
                ..HostPathBinding::default()
            })
            .with_read_only_paths(read_only);
        let mut command_line = vec![call.program.as_os_str().to_owned()];
        command_line.extend(call.args.iter().cloned());
        let plan = launcher
            .plan(sandbox, &command_line)
            .map_err(|err| format!("{tool}: Sandbox-Plan abgelehnt: {err}"))?;
        let root = sandbox.workspace().canonical_root();
        let plan_args = if call.cwd.as_path() == root {
            plan.args().to_vec()
        } else {
            if !call.cwd.starts_with(root) {
                return Err(format!(
                    "{tool}: Arbeitsverzeichnis liegt außerhalb des Workspaces"
                ));
            }
            retarget_chdir(plan.args(), &call.cwd).ok_or_else(|| {
                format!("{tool}: Sandbox-Plan ohne --chdir, Arbeitsverzeichnis nicht setzbar")
            })?
        };
        let launch = launch_command(
            prlimit.as_deref(),
            &self.limits,
            launcher.executable(),
            &plan_args,
        );
        Ok(PreparedLaunch {
            program: launch.program.into_os_string(),
            args: launch.args,
            cwd: root.to_path_buf(),
            env: Vec::new(),
        })
    }

    /// Bereitet `call` vor (in Tests wahlweise ohne Sandbox) und führt ihn
    /// bis zur `deadline` aus.
    ///
    /// # Errors
    /// `Err(Some(ToolOutput))` für Vorbereitungs-, Start- und I/O-Fehler,
    /// `Err(None)` bei Abbruch.
    async fn run_program(
        &self,
        tool: &'static str,
        sandbox: &SandboxSpec,
        env: &ProgramEnv,
        call: &ProgramCall,
        cancel: Option<&CancelToken>,
        deadline: tokio::time::Instant,
    ) -> Result<(RunEnd, BoundedCapture), Option<ToolOutput>> {
        // Tests run the programs through the job runtime as well.
        #[cfg(test)]
        crate::test_support::install_host_port();
        #[cfg(test)]
        let prepared = if self.launch_directly {
            Ok(PreparedLaunch {
                program: call.program.clone().into_os_string(),
                args: call.args.clone(),
                cwd: call.cwd.clone(),
                env: vec![("PATH".to_owned(), env.sandbox_path.clone())],
            })
        } else {
            self.sandboxed_command(tool, sandbox, env, call)
        };
        #[cfg(not(test))]
        let prepared = self.sandboxed_command(tool, sandbox, env, call);
        let launch = prepared.map_err(|message| {
            warn!(%message, tool, "LaTeX-Werkzeug: Sandbox-Start nicht möglich");
            Some(ToolOutput::error(message))
        })?;
        self.collect(tool, &launch, cancel, deadline).await
    }
}

/// Ergebnis aller Läufe eines Builds.
struct BuildOutcome {
    /// Ende des letzten Laufs.
    end: RunEnd,
    /// Ausgabe des letzten Laufs.
    capture: BoundedCapture,
    /// `true`, wenn irgendein Lauf das Ausgabebudget sprengte.
    truncated: bool,
    /// Die Läufe in Reihenfolge (`latexmk` bzw. Engine/`biber`).
    runs: Vec<String>,
    /// Hinweise für das Modell (z. B. fehlendes `biber`).
    notes: Vec<String>,
}

/// Eingaben des direkten Rückfalls (Runde 7, Teil T3).
struct DirectPlan {
    /// Gewählte Engine (für die Laufnamen).
    engine: LatexEngine,
    /// Absoluter Pfad der Engine.
    engine_path: PathBuf,
    /// Absoluter Pfad von `biber`, falls gefunden.
    biber: Option<PathBuf>,
    /// Kanonische Workspace-Wurzel.
    root: PathBuf,
    /// Kanonische `.tex`-Datei.
    file: PathBuf,
    /// Startzeitpunkt (für „frische“ Log-Dateien).
    started: SystemTime,
}

/// `true`, wenn das Log einen weiteren Engine-Lauf verlangt (Verweise,
/// Inhaltsverzeichnis).
fn needs_rerun(log: &str) -> bool {
    ["Rerun to get", "Label(s) may have changed", "Rerun LaTeX"]
        .iter()
        .any(|marker| log.contains(marker))
}

/// Ausführer eines `latex.build`-Aufrufs; Konfiguration vom Provider.
struct LatexBuildExecutor {
    timeout_secs: u64,
    search_path: Option<OsString>,
    runner: SandboxRunner,
}

impl LatexBuildExecutor {
    /// Rückfall ohne `latexmk`: Engine, ggf. `biber`, Engine (und ein
    /// dritter Engine-Lauf bei „Rerun“-Hinweis), alle bis zur gemeinsamen
    /// `deadline`. Ein fehlgeschlagener Engine-Lauf beendet die Kette.
    ///
    /// # Errors
    /// `Err(Some(ToolOutput))` für Start-/I/O-Fehler, `Err(None)` bei Abbruch.
    async fn run_direct(
        &self,
        sandbox: &SandboxSpec,
        env: &ProgramEnv,
        plan: &DirectPlan,
        cancel: Option<&CancelToken>,
        deadline: tokio::time::Instant,
    ) -> Result<BuildOutcome, Option<ToolOutput>> {
        let (Some(dir), Some(file_name), Some(stem)) = (
            plan.file.parent(),
            plan.file.file_name(),
            plan.file.file_stem(),
        ) else {
            return Err(Some(ToolOutput::error(
                "latex.build: Dateiname oder Ordner der .tex-Datei fehlt",
            )));
        };
        let engine_call = ProgramCall {
            program: plan.engine_path.clone(),
            args: direct_engine_args(file_name),
            cwd: dir.to_path_buf(),
        };
        let mut runs: Vec<String> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        let mut truncated = false;
        let mut engine_runs = 0usize;
        loop {
            let (end, capture) = self
                .runner
                .run_program(
                    LATEX_BUILD_TOOL,
                    sandbox,
                    env,
                    &engine_call,
                    cancel,
                    deadline,
                )
                .await?;
            engine_runs += 1;
            runs.push(plan.engine.binary().to_owned());
            truncated |= capture.limit_exceeded();
            let continue_chain = end.succeeded() && !capture.limit_exceeded();
            if !continue_chain || engine_runs >= MAX_DIRECT_ENGINE_RUNS {
                return Ok(BuildOutcome {
                    end,
                    capture,
                    truncated,
                    runs,
                    notes,
                });
            }
            if engine_runs == 1 {
                if plan.file.with_extension("bcf").is_file() {
                    match &plan.biber {
                        Some(biber) => {
                            let biber_call = ProgramCall {
                                program: biber.clone(),
                                args: vec![stem.to_owned()],
                                cwd: dir.to_path_buf(),
                            };
                            let (biber_end, biber_capture) = self
                                .runner
                                .run_program(
                                    LATEX_BUILD_TOOL,
                                    sandbox,
                                    env,
                                    &biber_call,
                                    cancel,
                                    deadline,
                                )
                                .await?;
                            runs.push(BIBER.to_owned());
                            truncated |= biber_capture.limit_exceeded();
                            match biber_end {
                                RunEnd::TimedOut => {
                                    return Ok(BuildOutcome {
                                        end: RunEnd::TimedOut,
                                        capture: biber_capture,
                                        truncated,
                                        runs,
                                        notes,
                                    });
                                }
                                RunEnd::Finished(status) => {
                                    if !status.is_some_and(|status| status.success()) {
                                        let code =
                                            status.and_then(|status| status.code()).unwrap_or(-1);
                                        notes.push(format!(
                                            "biber endete mit Code {code}; das \
                                             Literaturverzeichnis ist unvollständig \
                                             (Ausgabe: {}).",
                                            output_tail(&biber_capture)
                                        ));
                                    }
                                }
                            }
                        }
                        None => notes.push(
                            "biblatex verlangt biber, biber ist aber nicht installiert; \
                             das Literaturverzeichnis bleibt leer."
                                .to_owned(),
                        ),
                    }
                }
                continue;
            }
            // Zwei Engine-Läufe sind durch; ein dritter nur bei Rerun-Hinweis.
            let rerun = read_fresh_log(&plan.root, &plan.file, plan.started)
                .is_some_and(|log| needs_rerun(&log));
            if !rerun {
                return Ok(BuildOutcome {
                    end,
                    capture,
                    truncated,
                    runs,
                    notes,
                });
            }
        }
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

        let started = SystemTime::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        // Runde 7, Teil T3: eine Frist für alle Läufe dieses Aufrufs.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(self.timeout_secs);
        let outcome = match &toolchain.builder {
            Builder::Latexmk(latexmk) => {
                let call = ProgramCall {
                    program: latexmk.clone(),
                    args: latexmk_args(engine, clean, &file),
                    cwd: root.clone(),
                };
                self.runner
                    .run_program(
                        LATEX_BUILD_TOOL,
                        sandbox,
                        &toolchain.env,
                        &call,
                        cancel,
                        deadline,
                    )
                    .await
                    .map(|(end, capture)| BuildOutcome {
                        truncated: capture.limit_exceeded(),
                        end,
                        capture,
                        runs: vec![LATEXMK.to_owned()],
                        notes: Vec::new(),
                    })
            }
            Builder::Direct {
                engine: engine_path,
                biber,
            } => {
                let plan = DirectPlan {
                    engine,
                    engine_path: engine_path.clone(),
                    biber: biber.clone(),
                    root: root.clone(),
                    file: file.clone(),
                    started,
                };
                self.run_direct(sandbox, &toolchain.env, &plan, cancel, deadline)
                    .await
            }
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(Some(output)) => return Ok(output),
            Err(None) => return Err(ToolsError::Cancelled),
        };

        let tail = output_tail(&outcome.capture);
        let truncated = outcome.truncated;
        let log = read_fresh_log(&root, &file, started);
        let log_text = log.as_deref().unwrap_or(&tail);
        let log_excerpt = extract_log_excerpt(log_text);
        let file_rel = relative(&root, &file);
        let mut result = BTreeMap::<&str, Value>::new();
        result.insert("engine", json!(engine.binary()));
        result.insert("builder", json!(toolchain.builder.label()));
        result.insert("runs", json!(outcome.runs));
        result.insert("file", json!(file_rel));
        result.insert("output_tail", json!(tail));
        result.insert("truncated", json!(truncated));
        if !outcome.notes.is_empty() {
            result.insert("notes", json!(outcome.notes));
        }
        match outcome.end {
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
                    let report = parse_build_log(log_text);
                    let pdf = file.with_extension("pdf");
                    let pdf_rel = (ok && pdf.is_file()).then(|| relative(&root, &pdf));
                    let status = match (ok, report.has_warnings()) {
                        (false, _) => "failed",
                        (true, true) => "ok_with_warnings",
                        (true, false) => "ok",
                    };
                    result.insert("status", json!(status));
                    result.insert("pdf", json!(pdf_rel));
                    result.insert("pages", json!(report.pages));
                    result.insert("overfull", json!(report.overfull));
                    result.insert("overfull_total", json!(report.overfull_total));
                    result.insert("underfull_count", json!(report.underfull_count));
                    result.insert("missing_chars", json!(report.missing_chars));
                    result.insert("language_warnings", json!(report.language_warnings));
                }
                info!(
                    exit_code,
                    clean,
                    builder = toolchain.builder.label(),
                    "latex.build completed"
                );
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

/// Registriert die Werkzeuge `latex.build`, `latex.template` und
/// `latex.check`.
///
/// # Beschreibung
/// Vorgaben: Zeitlimit 120 s (`latex.check`: höchstens 30 s),
/// Ausgabebudget 64 KiB je Lauf, [`ShellLimits`] mit 120 s CPU-Zeit und
/// 4 GiB Adressraum (TeX-Läufe mit großen Schriften). Die Programmsuche
/// nutzt den `PATH` des Harness-Prozesses. Die Vorlagen für
/// `latex.template` sind eingebettet (Runde 7, Teil T2).
///
/// # Nebenläufigkeit
/// `Send + Sync`; [`ToolProvider::parallel_safe`] ist für alle drei
/// Werkzeuge `false` (Builds schreiben in dieselben Hilfsdateien,
/// `latex.template` legt Dateien an).
///
/// # Beispiele
/// ```rust
/// use harw_extension_api::contributors::ToolProvider;
/// use harw_tool_shell::LatexToolProvider;
/// use harw_tools::spec::ToolName;
///
/// let provider = LatexToolProvider::new();
/// assert_eq!(provider.tools().len(), 3);
/// assert!(provider.executor(&ToolName::new("latex.build")).is_some());
/// assert!(provider.executor(&ToolName::new("latex.template")).is_some());
/// assert!(provider.executor(&ToolName::new("latex.check")).is_some());
/// ```
pub struct LatexToolProvider {
    /// Zeitlimit eines Aufrufs in Sekunden (alle Läufe zusammen).
    pub timeout_secs: u64,
    /// Gemeinsames Budget für stdout+stderr in Bytes (je Lauf).
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

    /// Setzt den Suchpfad für `latexmk`, die Engine, `biber`, `kpsewhich`
    /// und `fc-list` (statt `PATH` des Prozesses) — für Betreiber mit TeX
    /// außerhalb des `PATH` und für Tests.
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

    /// Die gemeinsame Startkonfiguration für die Executor.
    fn runner(&self) -> SandboxRunner {
        SandboxRunner {
            max_output_bytes: self.max_output_bytes,
            limits: self.limits,
            extra_read_only: self.extra_read_only.clone(),
            #[cfg(test)]
            launch_directly: self.launch_directly,
        }
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

// Die drei LaTeX-Werkzeuge. Spezifikationen stammen aus den Provider-Funktionen
// (`parameter_schema`, `template::tool_spec`, `check::tool_spec`); die
// Executor tragen Zeitlimit, Suchpfad und Runner des Providers.
// `parallel_safe: none` — Builds schreiben ins Projekt, Aufrufer serialisieren.
harw_tools::tool_provider! {
    impl for LatexToolProvider as provider, parallel_safe: none {
        LATEX_BUILD_TOOL => {
            spec: ToolSpec::Function(FunctionToolSpec {
                name: ToolName::new(LATEX_BUILD_TOOL),
                description: "Build a LaTeX document inside the isolated project sandbox \
                    (no network, no shell escape, .latexmkrc ignored). Uses latexmk; without \
                    latexmk the engine runs directly twice (biber in between when needed). \
                    Only the .tex file, the engine (xelatex|pdflatex|lualatex) and clean are \
                    selectable. Returns status (ok|ok_with_warnings|failed|timeout|cleaned|\
                    not_installed), builder, the PDF path, pages, overfull boxes > 1pt with \
                    source line and context, underfull_count, missing_chars, \
                    language_warnings (missing hyphenation patterns) and the first log errors. \
                    Fix ok_with_warnings findings before reporting success. On not_installed \
                    stop and pass user_message to the user verbatim. Requires ExecuteProcess \
                    and WriteWorkspace."
                    .to_owned(),
                parameters: LatexToolProvider::parameter_schema(),
                strict: true,
            }),
            executor: LatexBuildExecutor {
                timeout_secs: provider.timeout_secs,
                search_path: provider.search_path.clone(),
                runner: provider.runner(),
            },
        },
        LATEX_TEMPLATE_TOOL => {
            spec: template::tool_spec(),
            executor: template::LatexTemplateExecutor::bundled(),
        },
        LATEX_CHECK_TOOL => {
            spec: check::tool_spec(),
            executor: check::LatexCheckExecutor {
                timeout_secs: provider.timeout_secs.min(check::CHECK_TIMEOUT_SECS),
                search_path: provider.search_path.clone(),
                runner: provider.runner(),
            },
        },
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
    use harw_extension_api::contributors::ToolProvider;
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

    /// Runde 7, Teil T3: Sprach- und Schriftwarnungen landen im Auszug,
    /// Overfull-Boxen nicht (die stehen strukturiert im Ergebnis).
    #[test]
    fn test_log_excerpt_keeps_language_and_font_warnings() {
        let log = "Package babel Warning: No hyphenation patterns were preloaded for\n\
                   LaTeX Font Warning: Font shape `TU/DejaVuSerif(0)/m/sc' undefined\n\
                   Package fontspec Warning: Font \"Fehlschrift\" does not contain script\n\
                   Overfull \\hbox (20.0pt too wide) in paragraph at lines 1--2\n";
        let excerpt = extract_log_excerpt(log);
        assert_eq!(excerpt.len(), 3, "{excerpt:?}");
        assert!(excerpt[0].contains("hyphenation patterns"));
        assert!(excerpt[1].contains("Font shape"));
        assert!(excerpt[2].contains("fontspec Warning"));
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
        assert_eq!(toolchain.builder, Builder::Latexmk(bin.join("latexmk")));
        assert!(toolchain.env.sandbox_path.ends_with(MINIMAL_PATH));
        let root = dir
            .path()
            .join("texlive/2025")
            .canonicalize()
            .map_err(ctx("canon"))?;
        assert_eq!(toolchain.env.install_roots, vec![root]);
        // Aufräumen braucht die Engine nicht.
        let clean_bin = dir.path().join("clean-bin");
        fs::create_dir_all(&clean_bin).map_err(ctx("clean bin"))?;
        fake_binary(&clean_bin, "latexmk", "exit 0")?;
        assert!(resolve_toolchain(clean_bin.as_os_str(), LatexEngine::Xelatex, true).is_ok());
        Ok(())
    }

    /// Runde 7, Teil T3: ohne `latexmk` wählt die Suche den direkten Weg
    /// (mit `biber`, falls vorhanden); Aufräumen braucht weiter `latexmk`.
    #[test]
    fn test_toolchain_falls_back_to_direct_engine_without_latexmk() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).map_err(ctx("bin"))?;
        fake_binary(&bin, "xelatex", "exit 0")?;
        let toolchain = resolve_toolchain(bin.as_os_str(), LatexEngine::Xelatex, false)
            .map_err(|missing| TestError::Unexpected(format!("{missing:?}")))?;
        assert_eq!(
            toolchain.builder,
            Builder::Direct {
                engine: bin.join("xelatex"),
                biber: None,
            }
        );
        assert_eq!(toolchain.builder.label(), "direct");
        fake_binary(&bin, "biber", "exit 0")?;
        let toolchain = resolve_toolchain(bin.as_os_str(), LatexEngine::Xelatex, false)
            .map_err(|missing| TestError::Unexpected(format!("{missing:?}")))?;
        assert_eq!(
            toolchain.builder,
            Builder::Direct {
                engine: bin.join("xelatex"),
                biber: Some(bin.join("biber")),
            }
        );
        assert_eq!(
            resolve_toolchain(bin.as_os_str(), LatexEngine::Xelatex, true),
            Err(vec!["latexmk".to_owned()])
        );
        Ok(())
    }

    #[test]
    fn test_direct_engine_argv_is_fixed_without_shell_escape() {
        let args: Vec<String> = direct_engine_args(OsStr::new("main.tex"))
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-interaction=nonstopmode",
                "-halt-on-error",
                "-file-line-error",
                "-no-shell-escape",
                "main.tex",
            ]
        );
    }

    #[test]
    fn test_retarget_chdir_replaces_only_the_bwrap_chdir() {
        let args: Vec<OsString> = [
            "--unshare-all",
            "--chdir",
            "/ws",
            "--",
            "/usr/bin/xelatex",
            "--chdir",
            "x",
        ]
        .map(OsString::from)
        .to_vec();
        let retargeted = retarget_chdir(&args, Path::new("/ws/doc"));
        let expected: Vec<OsString> = [
            "--unshare-all",
            "--chdir",
            "/ws/doc",
            "--",
            "/usr/bin/xelatex",
            "--chdir",
            "x",
        ]
        .map(OsString::from)
        .to_vec();
        assert_eq!(retargeted, Some(expected));
        let without: Vec<OsString> = ["--unshare-all", "--", "/bin/true"]
            .map(OsString::from)
            .to_vec();
        assert_eq!(retarget_chdir(&without, Path::new("/ws/doc")), None);
    }

    /// Runde 7, Teil T3: Build ohne `latexmk` über den Rückfall. Die
    /// Fake-Engine protokolliert argv und Arbeitsverzeichnis, legt im ersten
    /// Lauf eine `.bcf` an und schreibt Log und PDF; `biber` läuft zwischen
    /// den beiden Engine-Läufen.
    #[tokio::test]
    async fn test_fake_engine_builds_without_latexmk_via_direct_fallback() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(&dir, &full_rights())?;
        let root = spec.workspace().canonical_root().to_path_buf();
        fs::create_dir_all(root.join("doc")).map_err(ctx("doc"))?;
        fs::write(root.join("doc/main.tex"), "x").map_err(ctx("tex"))?;
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).map_err(ctx("bin"))?;
        let trace = dir.path().join("trace.txt");
        fake_binary(
            &bin,
            "xelatex",
            &format!(
                "echo \"xelatex $(pwd) $*\" >> '{trace}'\n\
                 for a in \"$@\"; do f=\"$a\"; done\n\
                 base=\"${{f%.tex}}\"\n\
                 printf 'x' > \"$base.bcf\"\n\
                 printf 'Overfull \\\\hbox (12.5pt too wide) in paragraph at lines 7--8\\n[]Zu breit\\nOutput written on %s.pdf (3 pages).\\n' \"$base\" > \"$base.log\"\n\
                 printf '%%%%PDF-1.5' > \"$base.pdf\"",
                trace = trace.display()
            ),
        )?;
        fake_binary(
            &bin,
            "biber",
            &format!(
                "echo \"biber $(pwd) $*\" >> '{trace}'",
                trace = trace.display()
            ),
        )?;
        let mut provider = LatexToolProvider::new().with_search_path(bin.as_os_str());
        provider.launch_directly = true;
        let output = run(&provider, spec, json!({ "file": "doc/main.tex" })).await?;
        let value = json_of(&output)?;
        assert_eq!(value["builder"], "direct", "{value}");
        assert_eq!(value["runs"], json!(["xelatex", "biber", "xelatex"]));
        assert_eq!(value["status"], "ok_with_warnings", "{value}");
        assert_eq!(value["pdf"], "doc/main.pdf");
        assert_eq!(value["pages"], 3);
        assert_eq!(value["overfull"][0]["line"], 7);
        assert_eq!(value["overfull"][0]["pt"], 12.5);
        let trace = fs::read_to_string(&trace).map_err(ctx("trace"))?;
        let doc_dir = root.join("doc");
        let lines: Vec<&str> = trace.lines().collect();
        assert_eq!(lines.len(), 3, "{trace}");
        let expected_engine = format!(
            "xelatex {} -interaction=nonstopmode -halt-on-error -file-line-error \
             -no-shell-escape main.tex",
            doc_dir.display()
        );
        assert_eq!(lines[0], expected_engine);
        assert_eq!(lines[1], format!("biber {} main", doc_dir.display()));
        assert_eq!(lines[2], expected_engine);
        Ok(())
    }

    /// Ein fehlgeschlagener erster Engine-Lauf beendet die Kette; ohne
    /// `biber` gibt es einen Hinweis statt eines Laufs.
    #[tokio::test]
    async fn test_direct_fallback_stops_after_failed_engine_run() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(&dir, &full_rights())?;
        let root = spec.workspace().canonical_root().to_path_buf();
        fs::write(root.join("main.tex"), "x").map_err(ctx("tex"))?;
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).map_err(ctx("bin"))?;
        fake_binary(
            &bin,
            "xelatex",
            "echo './main.tex:3: Undefined control sequence.'\nexit 1",
        )?;
        let mut provider = LatexToolProvider::new().with_search_path(bin.as_os_str());
        provider.launch_directly = true;
        let output = run(&provider, spec, json!({ "file": "main.tex" })).await?;
        let value = json_of(&output)?;
        assert_eq!(value["status"], "failed", "{value}");
        assert_eq!(value["builder"], "direct");
        assert_eq!(value["runs"], json!(["xelatex"]));
        assert_eq!(value["exit_code"], 1);
        Ok(())
    }

    /// Mit `latexmk`: fehlende Zeichen im Log machen aus „ok“
    /// „ok_with_warnings“; Seitenzahl und Underfull-Zählung stehen im
    /// Ergebnis.
    #[tokio::test]
    async fn test_latexmk_build_reports_pages_and_warnings() -> TestResult {
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
             base=\"${f%.tex}\"\n\
             printf 'Underfull \\\\hbox (badness 10000) in paragraph at lines 1--2\\n\
             Missing character: There is no \\342\\234\\223 (U+2713) in font cmr10!\\n\
             Output written on main.pdf (2 pages).\\n' > \"$base.log\"\n\
             printf '%%PDF-1.5' > \"$base.pdf\"",
        )?;
        let mut provider = LatexToolProvider::new().with_search_path(bin.as_os_str());
        provider.launch_directly = true;
        let output = run(&provider, spec, json!({ "file": "main.tex" })).await?;
        let value = json_of(&output)?;
        assert_eq!(value["builder"], "latexmk", "{value}");
        assert_eq!(value["status"], "ok_with_warnings", "{value}");
        assert_eq!(value["pages"], 2);
        assert_eq!(value["underfull_count"], 1);
        assert_eq!(value["missing_chars"], json!(["✓ (U+2713) in font cmr10"]));
        assert_eq!(value["overfull"], json!([]));
        Ok(())
    }

    #[test]
    fn test_provider_lists_three_strict_tools_and_is_not_parallel_safe() {
        let provider = LatexToolProvider::new();
        let tools = provider.tools();
        let names: Vec<&str> = tools.iter().map(ToolSpec::name).collect();
        assert_eq!(
            names,
            [LATEX_BUILD_TOOL, LATEX_TEMPLATE_TOOL, LATEX_CHECK_TOOL]
        );
        for name in [LATEX_BUILD_TOOL, LATEX_TEMPLATE_TOOL, LATEX_CHECK_TOOL] {
            assert!(!provider.parallel_safe(&ToolName::new(name)), "{name}");
            assert!(provider.executor(&ToolName::new(name)).is_some(), "{name}");
        }
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
