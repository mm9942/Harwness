//! Werkzeug `latex.check` — Vorabprüfung eines LaTeX-Dokuments (Runde 7,
//! Teil T4).
//!
//! # Zweck
//! Bevor der LaTeX-Worker baut, soll er wissen, ob Klasse, Pakete, Schriften
//! und Sprachen auf diesem Rechner vorhanden sind — und der Nutzerin einen
//! konkreten Installationshinweis geben können, statt nach drei
//! fehlgeschlagenen Builds aufzugeben.
//!
//! # Ablauf
//! 1. Die `.tex`-Datei (kanonisch im Workspace, wie bei `latex.build`) und
//!    rekursiv jede lokal eingebundene `.sty`/`.cls` aus demselben Ordner
//!    lesen, Kommentare entfernen.
//! 2. `\documentclass`/`\LoadClass`, `\usepackage`/`\RequirePackage`,
//!    fontspec-Schriften (`\setmainfont`, `\setsansfont`, `\setmonofont`,
//!    `\newfontfamily`) und Sprachen (babel-Optionen, `\babelprovide`,
//!    polyglossia) sammeln. Einfache Makros (`\newcommand{\x}{Wert}`,
//!    `\def\x{Wert}`, `\harw@default{\x}{Wert}`) werden aufgelöst, damit
//!    `\setmainfont{\harwMainFont}` aus `harw-report.sty` geprüft werden kann.
//! 3. In derselben Bubblewrap-Sandbox wie `latex.build` prüfen: ein
//!    `kpsewhich`-Lauf für alle `.cls`/`.sty`/`.ldf`/Trennmuster-Dateien, je
//!    Schrift ein `fc-list :family=<Name> family`.
//!
//! # Rückgabe
//! `{status: "ok"|"missing"|"not_installed"|"failed", missing: [{kind, name,
//! hint}], checked: {...}, user_message}`. `hint` ist ein Vorschlag für ein
//! Debian/Ubuntu-Paket (z. B. `texlive-latex-extra`, `texlive-lang-german`,
//! `fonts-dejavu`).
//!
//! # Sicherheit
//! Die Wirkung ist rein lesend, der Sandbox-Start braucht aber
//! `ExecuteProcess` — das Werkzeug läuft deshalb wie `latex.build` durch die
//! Freigabe und steht nicht in der Auto-Freigabe. Namen aus der Datei gehen
//! nur nach Prüfung (feste Zeichenmenge, kein führendes `-`) als argv an
//! `kpsewhich`/`fc-list`; es gibt keine Shell.

use super::{
    ProgramCall, ProgramEnv, RunEnd, SandboxRunner, find_in_path, not_installed_message, relative,
    resolve_tex_file,
};
use harw_authority::{Permission, SandboxSpec};
use harw_tools::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolOutput, ToolsError,
    schema::{AdditionalProperties, JsonSchema, JsonSchemaType},
    spec::{FunctionToolSpec, ToolName, ToolSpec},
};
use harw_types::cancel::CancelToken;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use tracing::{info, warn};

/// Name des Werkzeugs.
pub const LATEX_CHECK_TOOL: &str = "latex.check";
/// Zeitlimit aller Prüfläufe eines Aufrufs zusammen (Sekunden).
pub(crate) const CHECK_TIMEOUT_SECS: u64 = 30;
/// Höchstens so viele Bytes je gelesener Quelldatei.
const MAX_SOURCE_BYTES: u64 = 1024 * 1024;
/// Höchstens so viele lokale `.sty`/`.cls`-Dateien werden mitgelesen.
const MAX_LOCAL_SOURCES: usize = 16;
/// Höchstens so viele Schriften werden per `fc-list` geprüft.
const MAX_FONT_CHECKS: usize = 16;
/// Programm für die TeX-Dateisuche.
const KPSEWHICH: &str = "kpsewhich";
/// Programm für die Schriftsuche.
const FC_LIST: &str = "fc-list";

// ── Datenmodell ───────────────────────────────────────────────────────────────

/// Art einer Voraussetzung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RequirementKind {
    /// Dokumentklasse (`.cls`).
    Class,
    /// Paket (`.sty`).
    Package,
    /// Schrift (fontconfig-Familie oder Schriftdatei).
    Font,
    /// Sprache (babel-`.ldf` bzw. Trennmuster).
    Language,
    /// Prüfprogramm (`kpsewhich`, `fc-list`).
    Program,
}

/// Eine fehlende Voraussetzung.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissingItem {
    /// Art.
    pub kind: RequirementKind,
    /// Name wie im Dokument (Paket, Klasse, Schrift, Sprache, Programm).
    pub name: String,
    /// Vorschlag für ein Debian/Ubuntu-Paket.
    pub hint: String,
}

/// Woher eine Sprache kommt (bestimmt, welche Dateien geprüft werden).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LanguageSource {
    /// Klassische babel-Option (braucht `<sprache>.ldf` und Trennmuster).
    BabelOption,
    /// `\babelprovide` (ini-Datei aus babel, braucht nur Trennmuster).
    BabelProvide,
    /// polyglossia (braucht nur Trennmuster).
    Polyglossia,
}

/// Die gesammelten Voraussetzungen eines Dokuments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Requirements {
    /// Dokumentklassen (`\documentclass`, `\LoadClass`), ohne lokale.
    pub classes: Vec<String>,
    /// Pakete, ohne lokale `.sty` aus dem Dokumentordner.
    pub packages: Vec<String>,
    /// Schriftfamilien bzw. Schriftdateien.
    pub fonts: Vec<String>,
    /// Sprachen samt Herkunft.
    pub languages: Vec<(String, LanguageSource)>,
    /// Mitgelesene lokale Dateien (Name mit Endung).
    pub local_files: Vec<String>,
    /// Namen, die nicht geprüft werden konnten (z. B. unaufgelöste Makros).
    pub unchecked: Vec<String>,
}

impl Requirements {
    fn push_unique(list: &mut Vec<String>, value: &str) {
        if !list.iter().any(|known| known == value) {
            list.push(value.to_owned());
        }
    }

    fn push_language(&mut self, name: &str, source: LanguageSource) {
        if !self
            .languages
            .iter()
            .any(|(known, known_source)| known == name && *known_source == source)
        {
            self.languages.push((name.to_owned(), source));
        }
    }
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Entfernt LaTeX-Kommentare (`%` bis Zeilenende, nicht nach `\`).
#[must_use]
pub fn strip_comments(source: &str) -> String {
    let mut stripped = String::with_capacity(source.len());
    for line in source.lines() {
        let mut backslashes = 0usize;
        for c in line.chars() {
            if c == '%' && backslashes % 2 == 0 {
                break;
            }
            backslashes = if c == '\\' { backslashes + 1 } else { 0 };
            stripped.push(c);
        }
        stripped.push('\n');
    }
    stripped
}

/// Überspringt Leerraum ab `pos`.
fn skip_ws(text: &str, pos: usize) -> usize {
    text[pos..]
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map_or(text.len(), |(offset, _)| pos + offset)
}

/// Liest eine Gruppe `open … close` (verschachtelt) ab `pos`.
///
/// # Rückgabe
/// Inhalt ohne Klammern und die Position hinter der schließenden Klammer,
/// oder `None`, wenn bei `pos` keine Gruppe beginnt oder sie nicht endet.
fn read_group(text: &str, pos: usize, open: char, close: char) -> Option<(String, usize)> {
    let rest = text.get(pos..)?;
    if !rest.starts_with(open) {
        return None;
    }
    let mut depth = 0usize;
    let mut escaped = false;
    for (offset, c) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
        } else if c == open {
            depth += 1;
        } else if c == close {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                let inner = &rest[open.len_utf8()..offset];
                return Some((inner.to_owned(), pos + offset + close.len_utf8()));
            }
        }
    }
    None
}

/// `true` für Buchstaben eines Befehlsnamens (inklusive `@`).
fn is_cs_letter(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '@'
}

/// Liest einen Befehlsnamen `\name` oder `{\name}` ab `pos`.
fn read_cs(text: &str, pos: usize) -> Option<(String, usize)> {
    let rest = text.get(pos..)?;
    if let Some(after) = rest.strip_prefix('\\') {
        let name: String = after.chars().take_while(|c| is_cs_letter(*c)).collect();
        if name.is_empty() {
            return None;
        }
        return Some((format!("\\{name}"), pos + 1 + name.len()));
    }
    let (inner, end) = read_group(text, pos, '{', '}')?;
    let inner = inner.trim();
    let name = inner.strip_prefix('\\')?;
    (!name.is_empty() && name.chars().all(is_cs_letter)).then(|| (inner.to_owned(), end))
}

/// Ein gefundener Befehlsaufruf.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandUse {
    /// Inhalt der ersten optionalen `[…]`-Gruppe.
    options: Option<String>,
    /// Inhalt der ersten Pflichtgruppe `{…}`.
    argument: String,
}

/// Findet alle Aufrufe von `\name[opt]{arg}`.
///
/// # Argumente
/// - `text` (`&str`): Quelltext ohne Kommentare.
/// - `name` (`&str`): Befehlsname ohne `\`.
/// - `skip_cs` (`bool`): `true`, wenn vor den Gruppen ein Befehlsname steht
///   (`\newfontfamily\x[opt]{Name}`).
fn find_commands(text: &str, name: &str, skip_cs: bool) -> Vec<CommandUse> {
    let pattern = format!("\\{name}");
    let mut uses = Vec::new();
    for (start, _) in text.match_indices(&pattern) {
        let mut pos = start + pattern.len();
        if text[pos..].chars().next().is_some_and(is_cs_letter) {
            continue;
        }
        pos = skip_ws(text, pos);
        if skip_cs {
            let Some((_, end)) = read_cs(text, pos) else {
                continue;
            };
            pos = skip_ws(text, end);
        }
        let mut options = None;
        if let Some((inner, end)) = read_group(text, pos, '[', ']') {
            options = Some(inner);
            pos = skip_ws(text, end);
        }
        if let Some((argument, _)) = read_group(text, pos, '{', '}') {
            uses.push(CommandUse { options, argument });
        }
    }
    uses
}

/// Sammelt einfache Makrodefinitionen (`\newcommand{\x}{Wert}`,
/// `\renewcommand`, `\providecommand`, `\def\x{Wert}`,
/// `\harw@default{\x}{Wert}`). Ein bereits gesetzter, nicht leerer Wert
/// bleibt (die Hauptdatei definiert vor dem Laden des Stils).
fn collect_definitions(text: &str, definitions: &mut BTreeMap<String, String>) {
    for command in [
        "newcommand",
        "renewcommand",
        "providecommand",
        "def",
        "harw@default",
    ] {
        let pattern = format!("\\{command}");
        for (start, _) in text.match_indices(&pattern) {
            let pos = start + pattern.len();
            if text[pos..].chars().next().is_some_and(is_cs_letter) {
                continue;
            }
            let pos = skip_ws(text, pos);
            let Some((name, end)) = read_cs(text, pos) else {
                continue;
            };
            let pos = skip_ws(text, end);
            // Makros mit Parametern (`[1]`, `#1`) sind keine Konstanten.
            let Some((value, _)) = read_group(text, pos, '{', '}') else {
                continue;
            };
            let value = value.trim().to_owned();
            if value.contains('#') {
                continue;
            }
            let keep_existing = definitions
                .get(&name)
                .is_some_and(|existing| !existing.is_empty());
            if !keep_existing {
                definitions.insert(name, value);
            }
        }
    }
}

/// Löst einen Wert auf, der nur aus einem Makro besteht (bis zu drei Stufen).
fn resolve_macro(value: &str, definitions: &BTreeMap<String, String>) -> String {
    let mut current = value.trim().to_owned();
    for _ in 0..3 {
        match definitions.get(&current) {
            Some(next) if current.starts_with('\\') => current = next.trim().to_owned(),
            _ => break,
        }
    }
    current
}

/// `true` für einen sicheren Paket-/Klassen-/Sprach-/Dateinamen.
fn is_plain_name(name: &str) -> bool {
    let mut chars = name.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
    first_ok
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// `true` für einen sicheren Schriftnamen (Familie).
fn is_font_family_name(name: &str) -> bool {
    let first_ok = name.chars().next().is_some_and(char::is_alphanumeric);
    first_ok
        && name.chars().count() <= 64
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == ' ' || c == '-' || c == '.')
}

/// `true`, wenn der Schriftname eine Datei meint (`.otf`/`.ttf`/`.ttc`).
fn is_font_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".otf", ".ttf", ".ttc"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Teilt eine Komma-Liste (`a, b,c`).
fn split_list(list: &str) -> impl Iterator<Item = &str> {
    list.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
}

/// Liest die Voraussetzungen aus bereits kommentarfreien Quelltexten.
///
/// # Beschreibung
/// `sources` enthält die Hauptdatei zuerst, danach die lokalen Stil- und
/// Klassendateien. Makrodefinitionen aller Quellen werden zuerst gesammelt,
/// dann die Befehle ausgewertet. Namen in `local_names` (lokale `.sty`/
/// `.cls` im Dokumentordner) werden nicht als Paket bzw. Klasse geführt.
///
/// # Argumente
/// - `sources` (`&[String]`): Quelltexte ohne Kommentare.
/// - `local_names` (`&BTreeSet<String>`): lokale Dateinamen mit Endung.
///
/// # Rückgabe
/// Die [`Requirements`] ohne `local_files`.
#[must_use]
pub fn parse_requirements(sources: &[String], local_names: &BTreeSet<String>) -> Requirements {
    let mut definitions = BTreeMap::new();
    for source in sources {
        collect_definitions(source, &mut definitions);
    }
    let mut requirements = Requirements::default();
    let mut babel_loaded = false;
    let mut class_options: Vec<String> = Vec::new();
    for source in sources {
        for (command, is_main_class) in [("documentclass", true), ("LoadClass", false)] {
            for found in find_commands(source, command, false) {
                let class = resolve_macro(&found.argument, &definitions);
                if is_main_class && let Some(options) = &found.options {
                    class_options.extend(split_list(options).map(str::to_owned));
                }
                if !is_plain_name(&class) {
                    Requirements::push_unique(&mut requirements.unchecked, &class);
                } else if !local_names.contains(&format!("{class}.cls")) {
                    Requirements::push_unique(&mut requirements.classes, &class);
                }
            }
        }
        for command in ["usepackage", "RequirePackage"] {
            for found in find_commands(source, command, false) {
                for package in split_list(&found.argument) {
                    let package = resolve_macro(package, &definitions);
                    if !is_plain_name(&package) {
                        Requirements::push_unique(&mut requirements.unchecked, &package);
                        continue;
                    }
                    if package == "babel" {
                        babel_loaded = true;
                        for option in found
                            .options
                            .as_deref()
                            .map(split_list)
                            .into_iter()
                            .flatten()
                        {
                            let language = option.strip_prefix("main=").unwrap_or(option).trim();
                            if language_entry(language).is_some() {
                                requirements.push_language(language, LanguageSource::BabelOption);
                            }
                        }
                    }
                    if !local_names.contains(&format!("{package}.sty")) {
                        Requirements::push_unique(&mut requirements.packages, &package);
                    }
                }
            }
        }
        for (command, skip_cs) in [
            ("setmainfont", false),
            ("setsansfont", false),
            ("setmonofont", false),
            ("newfontfamily", true),
        ] {
            for found in find_commands(source, command, skip_cs) {
                let font = resolve_macro(&found.argument, &definitions);
                let valid = if is_font_file(&font) {
                    is_plain_name(&font)
                } else {
                    is_font_family_name(&font)
                };
                if valid {
                    Requirements::push_unique(&mut requirements.fonts, &font);
                } else {
                    Requirements::push_unique(&mut requirements.unchecked, &font);
                }
            }
        }
        for found in find_commands(source, "babelprovide", false) {
            let language = resolve_macro(&found.argument, &definitions);
            if is_plain_name(&language) {
                requirements.push_language(&language, LanguageSource::BabelProvide);
            } else {
                Requirements::push_unique(&mut requirements.unchecked, &language);
            }
        }
        for command in [
            "setdefaultlanguage",
            "setmainlanguage",
            "setotherlanguage",
            "setotherlanguages",
        ] {
            for found in find_commands(source, command, false) {
                for language in split_list(&found.argument) {
                    let language = resolve_macro(language, &definitions);
                    if is_plain_name(&language) {
                        requirements.push_language(&language, LanguageSource::Polyglossia);
                    }
                }
            }
        }
    }
    // Klassenoptionen sind für babel globale Sprachoptionen.
    if babel_loaded {
        for option in &class_options {
            if language_entry(option).is_some() {
                requirements.push_language(option, LanguageSource::BabelOption);
            }
        }
    }
    requirements
}

// ── Hinweise ──────────────────────────────────────────────────────────────────

/// Sprachfamilie mit Trennmusterdatei und Debian/Ubuntu-Paket.
struct LanguageEntry {
    /// Namen (klein geschrieben) dieser Familie.
    names: &'static [&'static str],
    /// Trennmusterdatei; `None` für Englisch (immer im Format).
    patterns: Option<&'static str>,
    /// Debian/Ubuntu-Paket.
    package: &'static str,
}

/// Die bekannten Sprachen. Andere Sprachen werden nicht geprüft.
const LANGUAGES: &[LanguageEntry] = &[
    LanguageEntry {
        names: &[
            "german",
            "ngerman",
            "austrian",
            "naustrian",
            "swissgerman",
            "nswissgerman",
        ],
        patterns: Some("hyph-de-1996.tex"),
        package: "texlive-lang-german",
    },
    LanguageEntry {
        names: &["english", "american", "usenglish"],
        patterns: None,
        package: "texlive-lang-english",
    },
    LanguageEntry {
        names: &["british", "ukenglish"],
        patterns: Some("hyph-en-gb.tex"),
        package: "texlive-lang-english",
    },
    LanguageEntry {
        names: &["french", "francais", "frenchb"],
        patterns: Some("hyph-fr.tex"),
        package: "texlive-lang-french",
    },
    LanguageEntry {
        names: &["spanish"],
        patterns: Some("hyph-es.tex"),
        package: "texlive-lang-spanish",
    },
    LanguageEntry {
        names: &["italian"],
        patterns: Some("hyph-it.tex"),
        package: "texlive-lang-italian",
    },
    LanguageEntry {
        names: &["dutch"],
        patterns: Some("hyph-nl.tex"),
        package: "texlive-lang-european",
    },
    LanguageEntry {
        names: &["portuguese", "portuges", "brazilian", "brazil"],
        patterns: Some("hyph-pt.tex"),
        package: "texlive-lang-portuguese",
    },
    LanguageEntry {
        names: &["polish"],
        patterns: Some("hyph-pl.tex"),
        package: "texlive-lang-polish",
    },
];

/// Der Tabelleneintrag zu einer Sprache (Groß-/Kleinschreibung egal).
fn language_entry(name: &str) -> Option<&'static LanguageEntry> {
    let lower = name.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|entry| entry.names.contains(&lower.as_str()))
}

/// Debian/Ubuntu-Paketvorschlag für eine Klasse.
fn class_hint(class: &str) -> &'static str {
    match class {
        "article" | "report" | "book" | "letter" | "slides" | "minimal" => "texlive-latex-base",
        c if c.starts_with("scr") => "texlive-latex-recommended",
        "beamer" => "texlive-latex-recommended",
        _ => "texlive-latex-extra",
    }
}

/// Debian/Ubuntu-Paketvorschlag für ein Paket.
fn package_hint(package: &str) -> &'static str {
    match package {
        "fontspec" | "listings" | "microtype" | "booktabs" | "xcolor" | "geometry" | "hyperref"
        | "babel" | "csquotes" | "enumitem" | "caption" | "fancyhdr" | "tocbasic"
        | "scrlayer-scrpage" | "typearea" | "scrextend" => "texlive-latex-recommended",
        "longtable" | "tabularx" | "array" | "graphicx" | "amsmath" | "fontenc" | "inputenc" => {
            "texlive-latex-base"
        }
        "tikz" | "pgf" | "pgfplots" => "texlive-pictures",
        "biblatex" => "texlive-bibtex-extra",
        "siunitx" | "mhchem" => "texlive-science",
        "polyglossia" => "texlive-xetex",
        "fontawesome5" | "fontawesome" => "texlive-fonts-extra",
        "lmodern" => "lmodern",
        _ => "texlive-latex-extra",
    }
}

/// Debian/Ubuntu-Paketvorschlag für eine Schrift.
fn font_hint(font: &str) -> &'static str {
    let lower = font.to_lowercase();
    let table: [(&str, &str); 10] = [
        ("dejavu", "fonts-dejavu"),
        ("liberation", "fonts-liberation"),
        ("noto", "fonts-noto"),
        ("tex gyre", "fonts-texgyre"),
        ("texgyre", "fonts-texgyre"),
        ("latin modern", "fonts-lmodern"),
        ("linux libertine", "fonts-linuxlibertine"),
        ("linux biolinum", "fonts-linuxlibertine"),
        ("ubuntu", "fonts-ubuntu"),
        ("open sans", "fonts-open-sans"),
    ];
    table
        .iter()
        .find(|(prefix, _)| lower.starts_with(prefix))
        .map_or("texlive-fonts-extra", |(_, package)| package)
}

/// Menschlich lesbare Bezeichnung einer fehlenden Voraussetzung.
fn describe(item: &MissingItem) -> String {
    match item.kind {
        RequirementKind::Class => format!("Klasse „{}“", item.name),
        RequirementKind::Package => format!("Paket „{}“", item.name),
        RequirementKind::Font => format!("Schrift „{}“", item.name),
        RequirementKind::Language => format!("Sprache/Trennmuster „{}“", item.name),
        RequirementKind::Program => format!("Programm „{}“", item.name),
    }
}

/// Die Meldung für die Nutzerin zu fehlenden Voraussetzungen.
///
/// # Argumente
/// - `file` (`&str`): Dokument relativ zum Workspace.
/// - `missing` (`&[MissingItem]`): die fehlenden Voraussetzungen.
///
/// # Rückgabe
/// Deutscher Text mit Installationsvorschlag; nichts wird installiert.
#[must_use]
pub fn missing_message(file: &str, missing: &[MissingItem]) -> String {
    if missing.is_empty() {
        return format!(
            "{file}: alle geprüften Klassen, Pakete, Schriften und Sprachen sind vorhanden."
        );
    }
    let listed: Vec<String> = missing.iter().map(describe).collect();
    let packages: BTreeSet<&str> = missing.iter().map(|item| item.hint.as_str()).collect();
    let packages: Vec<&str> = packages.into_iter().collect();
    format!(
        "Für {file} fehlt auf diesem Rechner: {}. Harwness installiert nichts selbst. \
         Vorschlag für Debian/Ubuntu: sudo apt install {} (genaue Zuordnung einer Datei: \
         apt-file search <name>.sty). TeX Live von tug.org: tlmgr install <paket>. \
         Schriften im Home-Verzeichnis sieht die Build-Sandbox nicht; sie müssen \
         systemweit installiert sein. Alternativ lässt sich die Vorgabe im Dokument \
         durch eine vorhandene Schrift bzw. ein vorhandenes Paket ersetzen.",
        listed.join(", "),
        packages.join(" ")
    )
}

/// Der Hinweis zu Warnungen (fehlende Trennmuster).
///
/// # Argumente
/// - `warnings` (`&[MissingItem]`): Sprachen ohne Trennmuster.
///
/// # Rückgabe
/// Deutscher Text; baut trotzdem, aber mit falscher Silbentrennung.
#[must_use]
pub fn warning_message(warnings: &[MissingItem]) -> String {
    let languages: Vec<String> = warnings
        .iter()
        .map(|item| format!("„{}“", item.name))
        .collect();
    let packages: BTreeSet<&str> = warnings.iter().map(|item| item.hint.as_str()).collect();
    let packages: Vec<&str> = packages.into_iter().collect();
    format!(
        "Hinweis: für {} fehlen die Trennmuster. Das Dokument baut trotzdem, trennt aber \
         nach englischen Regeln (mehr Overfull-Boxen). Vorschlag für Debian/Ubuntu: \
         sudo apt install {}.",
        languages.join(", "),
        packages.join(" ")
    )
}

// ── Ausführung ────────────────────────────────────────────────────────────────

/// Argumente eines `latex.check`-Aufrufs.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LatexCheckArgs {
    /// `.tex`-Datei im Workspace.
    file: String,
}

/// Liest höchstens [`MAX_SOURCE_BYTES`] einer Datei als Text.
fn read_source(path: &Path) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_SOURCE_BYTES)
        .read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Eine lokale `.sty`/`.cls` im Dokumentordner, falls sie existiert und im
/// Workspace liegt.
fn local_source(root: &Path, dir: &Path, file_name: &str) -> Option<PathBuf> {
    let canonical = dir.join(file_name).canonicalize().ok()?;
    (canonical.starts_with(root) && canonical.is_file()).then_some(canonical)
}

/// Liest die Hauptdatei und rekursiv die lokal eingebundenen Stil- und
/// Klassendateien (ohne Kommentare).
///
/// # Rückgabe
/// Die Quelltexte (Hauptdatei zuerst) und die Namen der lokalen Dateien.
///
/// # Errors
/// Eine Meldung, wenn die Hauptdatei nicht lesbar ist.
fn gather_sources(root: &Path, file: &Path) -> Result<(Vec<String>, BTreeSet<String>), String> {
    let main =
        read_source(file).map_err(|err| format!("{}: nicht lesbar ({err})", file.display()))?;
    let dir = file.parent().unwrap_or(root);
    let mut sources = vec![strip_comments(&main)];
    let mut local_names: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<usize> = VecDeque::from([0]);
    while let Some(index) = queue.pop_front() {
        let Some(source) = sources.get(index).cloned() else {
            continue;
        };
        let mut candidates: Vec<String> = Vec::new();
        for command in ["usepackage", "RequirePackage"] {
            for found in find_commands(&source, command, false) {
                candidates.extend(split_list(&found.argument).map(|name| format!("{name}.sty")));
            }
        }
        for command in ["documentclass", "LoadClass"] {
            for found in find_commands(&source, command, false) {
                candidates.push(format!("{}.cls", found.argument.trim()));
            }
        }
        for candidate in candidates {
            if local_names.len() >= MAX_LOCAL_SOURCES
                || local_names.contains(&candidate)
                || !is_plain_name(&candidate)
            {
                continue;
            }
            let Some(path) = local_source(root, dir, &candidate) else {
                continue;
            };
            match read_source(&path) {
                Ok(text) => {
                    local_names.insert(candidate);
                    sources.push(strip_comments(&text));
                    queue.push_back(sources.len() - 1);
                }
                Err(err) => {
                    warn!(error = %err, file = %candidate, "latex.check: lokale Datei nicht lesbar");
                }
            }
        }
    }
    Ok((sources, local_names))
}

/// Maskiert einen Wert für ein fontconfig-Muster (`\`, `-`, `:`, `,`).
fn fontconfig_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\\' | '-' | ':' | ',' | '=') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// Eine zu prüfende TeX-Datei und wofür sie steht.
struct FileCheck {
    /// Dateiname für `kpsewhich`.
    file: String,
    /// Die fehlende Voraussetzung, falls die Datei fehlt.
    missing: MissingItem,
    /// `true`, wenn das Fehlen den Build nicht verhindert (nur
    /// Trennmuster): dann Warnung statt Fehler.
    warning_only: bool,
}

/// Plant die `kpsewhich`-Prüfungen.
fn plan_file_checks(requirements: &Requirements) -> Vec<FileCheck> {
    let mut checks: Vec<FileCheck> = Vec::new();
    let mut push = |file: String, kind: RequirementKind, name: &str, hint: &str| {
        if !checks.iter().any(|check| check.file == file) {
            // Fehlende Trennmuster brechen den Build nicht ab (babel fällt
            // mit Warnung auf die Muster von `\language=0` zurück).
            let warning_only = file.starts_with("hyph-");
            checks.push(FileCheck {
                file,
                missing: MissingItem {
                    kind,
                    name: name.to_owned(),
                    hint: hint.to_owned(),
                },
                warning_only,
            });
        }
    };
    for class in &requirements.classes {
        push(
            format!("{class}.cls"),
            RequirementKind::Class,
            class,
            class_hint(class),
        );
    }
    for package in &requirements.packages {
        push(
            format!("{package}.sty"),
            RequirementKind::Package,
            package,
            package_hint(package),
        );
    }
    for font in requirements.fonts.iter().filter(|font| is_font_file(font)) {
        push(font.clone(), RequirementKind::Font, font, font_hint(font));
    }
    for (language, source) in &requirements.languages {
        let Some(entry) = language_entry(language) else {
            continue;
        };
        if *source == LanguageSource::BabelOption && entry.patterns.is_some() {
            push(
                format!("{}.ldf", language.to_ascii_lowercase()),
                RequirementKind::Language,
                language,
                entry.package,
            );
        }
        if let Some(patterns) = entry.patterns {
            push(
                patterns.to_owned(),
                RequirementKind::Language,
                language,
                entry.package,
            );
        }
    }
    checks
}

/// Ausführer eines `latex.check`-Aufrufs; Konfiguration vom Provider.
pub(super) struct LatexCheckExecutor {
    pub(super) timeout_secs: u64,
    pub(super) search_path: Option<OsString>,
    pub(super) runner: SandboxRunner,
}

impl LatexCheckExecutor {
    /// Der eigentliche Ablauf nach dem Parsen.
    async fn run(
        &self,
        args: LatexCheckArgs,
        sandbox: &SandboxSpec,
        cancel: Option<&CancelToken>,
    ) -> Result<ToolOutput, ToolsError> {
        let root = sandbox.workspace().canonical_root().to_path_buf();
        let file = match resolve_tex_file(&root, &args.file) {
            Ok(file) => file,
            Err(message) => return Ok(ToolOutput::error(format!("{LATEX_CHECK_TOOL}: {message}"))),
        };
        let file_rel = relative(&root, &file);
        let (sources, local_names) = match gather_sources(&root, &file) {
            Ok(gathered) => gathered,
            Err(message) => return Ok(ToolOutput::error(format!("{LATEX_CHECK_TOOL}: {message}"))),
        };
        let mut requirements = parse_requirements(&sources, &local_names);
        requirements.local_files = local_names.into_iter().collect();
        let file_checks = plan_file_checks(&requirements);
        let font_families: Vec<&String> = requirements
            .fonts
            .iter()
            .filter(|font| !is_font_file(font))
            .take(MAX_FONT_CHECKS)
            .collect();

        let search_path = self
            .search_path
            .clone()
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_default();
        let kpsewhich = find_in_path(&search_path, KPSEWHICH);
        let fc_list = find_in_path(&search_path, FC_LIST);
        let mut missing: Vec<MissingItem> = Vec::new();
        // Runde 7, Teil T4: fehlende Trennmuster sind nur eine Warnung.
        let mut warnings: Vec<MissingItem> = Vec::new();
        let mut missing_programs: Vec<String> = Vec::new();
        if kpsewhich.is_none() && !file_checks.is_empty() {
            missing_programs.push(KPSEWHICH.to_owned());
            missing.push(MissingItem {
                kind: RequirementKind::Program,
                name: KPSEWHICH.to_owned(),
                hint: "texlive-binaries".to_owned(),
            });
        }
        if fc_list.is_none() && !font_families.is_empty() {
            missing_programs.push(FC_LIST.to_owned());
            missing.push(MissingItem {
                kind: RequirementKind::Program,
                name: FC_LIST.to_owned(),
                hint: "fontconfig".to_owned(),
            });
        }
        let programs: Vec<&Path> = [kpsewhich.as_deref(), fc_list.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        let env = ProgramEnv::of(&programs);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(self.timeout_secs);

        if let Some(kpsewhich) = &kpsewhich
            && !file_checks.is_empty()
        {
            let call = ProgramCall {
                program: kpsewhich.clone(),
                args: file_checks
                    .iter()
                    .map(|check| OsString::from(&check.file))
                    .collect(),
                cwd: root.clone(),
            };
            let (end, capture) = match self
                .runner
                .run_program(LATEX_CHECK_TOOL, sandbox, &env, &call, cancel, deadline)
                .await
            {
                Ok(result) => result,
                Err(Some(output)) => return Ok(output),
                Err(None) => return Err(ToolsError::Cancelled),
            };
            if matches!(end, RunEnd::TimedOut) {
                return Ok(failed_output(
                    &file_rel,
                    "kpsewhich überschritt das Zeitlimit",
                ));
            }
            let stdout = String::from_utf8_lossy(capture.stdout());
            let found: BTreeSet<String> = stdout
                .lines()
                .filter_map(|line| {
                    Path::new(line.trim())
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_owned)
                })
                .collect();
            let stderr = String::from_utf8_lossy(capture.stderr());
            if found.is_empty() && !stderr.trim().is_empty() {
                return Ok(failed_output(
                    &file_rel,
                    &format!("kpsewhich meldet einen Fehler: {}", stderr.trim()),
                ));
            }
            for check in &file_checks {
                if found.contains(&check.file) {
                    continue;
                }
                let target = if check.warning_only {
                    &mut warnings
                } else {
                    &mut missing
                };
                if !target
                    .iter()
                    .any(|item| item.kind == check.missing.kind && item.name == check.missing.name)
                {
                    target.push(check.missing.clone());
                }
            }
        }

        if let Some(fc_list) = &fc_list {
            for font in &font_families {
                let call = ProgramCall {
                    program: fc_list.clone(),
                    args: vec![
                        OsString::from(format!(":family={}", fontconfig_escape(font))),
                        OsString::from("family"),
                    ],
                    cwd: root.clone(),
                };
                let (end, capture) = match self
                    .runner
                    .run_program(LATEX_CHECK_TOOL, sandbox, &env, &call, cancel, deadline)
                    .await
                {
                    Ok(result) => result,
                    Err(Some(output)) => return Ok(output),
                    Err(None) => return Err(ToolsError::Cancelled),
                };
                if matches!(end, RunEnd::TimedOut) {
                    return Ok(failed_output(
                        &file_rel,
                        "fc-list überschritt das Zeitlimit",
                    ));
                }
                if String::from_utf8_lossy(capture.stdout()).trim().is_empty() {
                    missing.push(MissingItem {
                        kind: RequirementKind::Font,
                        name: (*font).clone(),
                        hint: font_hint(font).to_owned(),
                    });
                }
            }
        }

        let status = if !missing_programs.is_empty() {
            "not_installed"
        } else if !missing.is_empty() {
            "missing"
        } else if !warnings.is_empty() {
            "ok_with_warnings"
        } else {
            "ok"
        };
        let mut user_message = if missing_programs.is_empty() {
            missing_message(&file_rel, &missing)
        } else {
            format!(
                "{} Ohne {} lässt sich nicht vorab prüfen. {}",
                not_installed_message(&missing_programs),
                missing_programs.join("/"),
                missing_message(&file_rel, &missing)
            )
        };
        if !warnings.is_empty() {
            user_message.push(' ');
            user_message.push_str(&warning_message(&warnings));
        }
        info!(status, missing = missing.len(), "latex.check completed");
        let languages: Vec<&String> = requirements
            .languages
            .iter()
            .map(|(name, _)| name)
            .collect();
        Ok(ToolOutput::json(json!({
            "status": status,
            "file": file_rel,
            "missing": missing,
            "warnings": warnings,
            "checked": {
                "classes": requirements.classes,
                "packages": requirements.packages,
                "fonts": requirements.fonts,
                "languages": languages,
                "local_files": requirements.local_files,
                "unchecked": requirements.unchecked,
            },
            "user_message": user_message,
        })))
    }
}

/// Ergebnis für einen gescheiterten Prüflauf.
fn failed_output(file: &str, reason: &str) -> ToolOutput {
    ToolOutput::json(json!({
        "status": "failed",
        "file": file,
        "missing": [],
        "user_message": format!("{file}: Vorabprüfung nicht möglich: {reason}"),
    }))
}

impl ToolExecutor for LatexCheckExecutor {
    /// Führt `latex.check` aus.
    ///
    /// # Beschreibung
    /// 1. Argumente parsen (unbekannte Felder werden abgelehnt).
    /// 2. `ReadWorkspace` und `ExecuteProcess` prüfen.
    /// 3. Quellen lesen, Voraussetzungen sammeln, in der Sandbox prüfen.
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
            let args: LatexCheckArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|err| {
                    ToolsError::InvalidArguments {
                        name: LATEX_CHECK_TOOL.to_owned(),
                        reason: err.to_string(),
                    }
                })?;
            for permission in [Permission::ReadWorkspace, Permission::ExecuteProcess] {
                if let Some(denied) = harw_tools::sandbox_guard::require_permission(
                    context,
                    permission,
                    LATEX_CHECK_TOOL,
                ) {
                    warn!(?permission, "latex.check denied");
                    return Ok(denied);
                }
            }
            self.run(args, context.sandbox(), context.cancel()).await
        })
    }
}

/// Die Werkzeugbeschreibung für das Modell.
pub(crate) fn tool_spec() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "file".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Path of the main .tex file inside the workspace (relative to the workspace root)."
                    .to_owned(),
            ),
            ..Default::default()
        },
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(LATEX_CHECK_TOOL),
        description: "Pre-flight check of a LaTeX document before latex.build: reads the \
            document class, packages (also from local .sty files next to it), fontspec fonts \
            and babel/polyglossia languages and checks them with kpsewhich and fc-list inside \
            the sandbox. Returns status (ok|missing|not_installed|failed), missing \
            [{kind, name, hint}] with a Debian/Ubuntu package suggestion and user_message. \
            On missing or not_installed pass user_message to the user; never install \
            anything. Requires ExecuteProcess."
            .to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(properties),
            required: Some(vec!["file".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        },
        strict: true,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::latex::LatexToolProvider;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
    use harw_extension_api::contributors::ToolProvider;
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use serde_json::Value;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    const MAIN: &str = "\\documentclass[11pt,ngerman]{scrartcl}\n\
        \\newcommand{\\harwMainFont}{Fehlschrift Pro}\n\
        \\usepackage[T1]{fontenc} % \\usepackage{auskommentiert}\n\
        \\usepackage{fontspec, tcolorbox}\n\
        \\usepackage{harw-report}\n\
        \\setsansfont[Scale=0.9]{DejaVu Sans}\n\
        \\newfontfamily\\headingfont[Color=red]{DejaVu Sans}\n\
        \\begin{document}x 50\\% mehr\\end{document}\n";

    const STYLE: &str = "\\ProvidesPackage{harw-report}\n\
        \\harw@default{\\harwMainFont}{DejaVu Serif}\n\
        \\harw@default{\\harwLanguage}{german}\n\
        \\RequirePackage{booktabs}\n\
        \\setmainfont{\\harwMainFont}\n\
        \\RequirePackage{babel}\n\
        \\begingroup\\edef\\harw@tmp{\\endgroup\\noexpand\\babelprovide[import,main]{\\harwLanguage}}\\harw@tmp\n";

    fn sources() -> Vec<String> {
        vec![strip_comments(MAIN), strip_comments(STYLE)]
    }

    #[test]
    fn test_parse_requirements_reads_class_packages_fonts_and_languages() {
        let local: BTreeSet<String> = ["harw-report.sty".to_owned()].into();
        let requirements = parse_requirements(&sources(), &local);
        assert_eq!(requirements.classes, ["scrartcl"]);
        assert_eq!(
            requirements.packages,
            ["fontenc", "fontspec", "tcolorbox", "booktabs", "babel"]
        );
        assert_eq!(requirements.fonts, ["DejaVu Sans", "Fehlschrift Pro"]);
        assert_eq!(
            requirements.languages,
            [
                ("german".to_owned(), LanguageSource::BabelProvide),
                ("ngerman".to_owned(), LanguageSource::BabelOption),
            ]
        );
        assert!(requirements.unchecked.is_empty(), "{requirements:?}");
    }

    #[test]
    fn test_strip_comments_keeps_escaped_percent() {
        assert_eq!(
            strip_comments("a 50\\% b % weg\n\\\\% auch weg\n"),
            "a 50\\% b \n\\\\\n"
        );
    }

    #[test]
    fn test_missing_message_lists_items_and_apt_hint() {
        let missing = [
            MissingItem {
                kind: RequirementKind::Package,
                name: "tcolorbox".to_owned(),
                hint: package_hint("tcolorbox").to_owned(),
            },
            MissingItem {
                kind: RequirementKind::Font,
                name: "DejaVu Serif".to_owned(),
                hint: font_hint("DejaVu Serif").to_owned(),
            },
            MissingItem {
                kind: RequirementKind::Language,
                name: "german".to_owned(),
                hint: "texlive-lang-german".to_owned(),
            },
        ];
        let message = missing_message("doc/main.tex", &missing);
        for needle in [
            "Paket „tcolorbox“",
            "Schrift „DejaVu Serif“",
            "sudo apt install fonts-dejavu texlive-lang-german texlive-latex-extra",
            "installiert nichts selbst",
        ] {
            assert!(message.contains(needle), "{needle}: {message}");
        }
    }

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

    fn fake_binary(dir: &Path, name: &str, body: &str) -> TestResult {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).map_err(ctx("fake binary"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .map_err(ctx("fake binary mode"))?;
        Ok(())
    }

    async fn run(provider: &LatexToolProvider, spec: SandboxSpec) -> TestResult<Value> {
        let context = ToolExecutionContext::new(SessionId::new(), TurnId::new(), spec);
        let executor = provider
            .executor(&ToolName::new(LATEX_CHECK_TOOL))
            .ok_or(TestError::Missing("executor"))?;
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(LATEX_CHECK_TOOL),
            arguments: json!({ "file": "doc/main.tex" }),
        };
        match executor
            .execute(&context, &call)
            .await
            .map_err(ctx("execute"))?
        {
            ToolOutput::Json { content } => Ok(content),
            other => Err(TestError::Unexpected(format!("kein JSON: {other:?}"))),
        }
    }

    /// Fake-`kpsewhich` kennt nur die Dateien in `tree/`, Fake-`fc-list`
    /// nur DejaVu: `tcolorbox`, die deutschen Trennmuster samt `ngerman.ldf`
    /// und „Fehlschrift Pro“ fehlen; die lokale `harw-report.sty` wird
    /// mitgelesen, aber nicht als Paket geprüft.
    #[tokio::test]
    async fn test_check_reports_missing_package_font_and_language() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(
            &dir,
            &[Permission::ReadWorkspace, Permission::ExecuteProcess],
        )?;
        let root = spec.workspace().canonical_root().to_path_buf();
        fs::create_dir_all(root.join("doc")).map_err(ctx("doc"))?;
        fs::write(root.join("doc/main.tex"), MAIN).map_err(ctx("main"))?;
        fs::write(root.join("doc/harw-report.sty"), STYLE).map_err(ctx("sty"))?;
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).map_err(ctx("tree"))?;
        for present in [
            "scrartcl.cls",
            "fontenc.sty",
            "fontspec.sty",
            "booktabs.sty",
            "babel.sty",
        ] {
            fs::write(tree.join(present), "").map_err(ctx("tree file"))?;
        }
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).map_err(ctx("bin"))?;
        fake_binary(
            &bin,
            "kpsewhich",
            &format!(
                "for a in \"$@\"; do if [ -f '{tree}'/\"$a\" ]; then echo '{tree}'/\"$a\"; fi; done",
                tree = tree.display()
            ),
        )?;
        fake_binary(
            &bin,
            "fc-list",
            "case \"$1\" in *DejaVu*) echo 'DejaVu Sans' ;; esac",
        )?;
        let mut provider = LatexToolProvider::new().with_search_path(bin.as_os_str());
        provider.launch_directly = true;
        let value = run(&provider, spec).await?;
        assert_eq!(value["status"], "missing", "{value}");
        assert_eq!(
            value["missing"],
            json!([
                { "kind": "package", "name": "tcolorbox", "hint": "texlive-latex-extra" },
                { "kind": "language", "name": "ngerman", "hint": "texlive-lang-german" },
                { "kind": "font", "name": "Fehlschrift Pro", "hint": "texlive-fonts-extra" },
            ])
        );
        // Nur fehlende Trennmuster (`\babelprovide{german}`) sind eine Warnung.
        assert_eq!(
            value["warnings"],
            json!([
                { "kind": "language", "name": "german", "hint": "texlive-lang-german" },
            ])
        );
        assert_eq!(value["checked"]["local_files"], json!(["harw-report.sty"]));
        let message = value["user_message"].as_str().unwrap_or_default();
        assert!(message.contains("Paket „tcolorbox“"), "{message}");
        assert!(message.contains("sudo apt install"), "{message}");
        Ok(())
    }

    /// Fehlen nur Trennmuster, baut das Dokument trotzdem:
    /// `ok_with_warnings` statt `missing`.
    #[tokio::test]
    async fn test_check_missing_patterns_only_is_a_warning() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(
            &dir,
            &[Permission::ReadWorkspace, Permission::ExecuteProcess],
        )?;
        let root = spec.workspace().canonical_root().to_path_buf();
        fs::create_dir_all(root.join("doc")).map_err(ctx("doc"))?;
        fs::write(
            root.join("doc/main.tex"),
            "\\documentclass{article}\n\\usepackage{babel}\n\\babelprovide[import,main]{german}\n",
        )
        .map_err(ctx("main"))?;
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).map_err(ctx("tree"))?;
        for present in ["article.cls", "babel.sty"] {
            fs::write(tree.join(present), "").map_err(ctx("tree file"))?;
        }
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).map_err(ctx("bin"))?;
        fake_binary(
            &bin,
            "kpsewhich",
            &format!(
                "for a in \"$@\"; do if [ -f '{tree}'/\"$a\" ]; then echo '{tree}'/\"$a\"; fi; done",
                tree = tree.display()
            ),
        )?;
        let mut provider = LatexToolProvider::new().with_search_path(bin.as_os_str());
        provider.launch_directly = true;
        let value = run(&provider, spec).await?;
        assert_eq!(value["status"], "ok_with_warnings", "{value}");
        assert_eq!(value["missing"], json!([]));
        assert_eq!(value["warnings"][0]["name"], "german");
        let message = value["user_message"].as_str().unwrap_or_default();
        assert!(message.contains("Trennmuster"), "{message}");
        Ok(())
    }

    /// Ein leeres Anpassungsmakro der Hauptdatei fällt auf die Vorgabe des
    /// Stils zurück (`\harw@default`), wie in `harw-report.sty`.
    #[test]
    fn test_empty_font_macro_falls_back_to_style_default() {
        let main = strip_comments(
            "\\documentclass{scrartcl}\n\\newcommand{\\harwMainFont}{}\n\\usepackage{harw-report}\n",
        );
        let local: BTreeSet<String> = ["harw-report.sty".to_owned()].into();
        let requirements = parse_requirements(&[main, strip_comments(STYLE)], &local);
        assert_eq!(requirements.fonts, ["DejaVu Serif"]);
    }

    #[tokio::test]
    async fn test_check_without_kpsewhich_reports_not_installed() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(
            &dir,
            &[Permission::ReadWorkspace, Permission::ExecuteProcess],
        )?;
        let root = spec.workspace().canonical_root().to_path_buf();
        fs::create_dir_all(root.join("doc")).map_err(ctx("doc"))?;
        fs::write(root.join("doc/main.tex"), "\\documentclass{article}\n").map_err(ctx("main"))?;
        let bin = dir.path().join("empty-bin");
        fs::create_dir_all(&bin).map_err(ctx("bin"))?;
        let mut provider = LatexToolProvider::new().with_search_path(bin.as_os_str());
        provider.launch_directly = true;
        let value = run(&provider, spec).await?;
        assert_eq!(value["status"], "not_installed", "{value}");
        assert_eq!(value["missing"][0]["kind"], "program");
        assert_eq!(value["missing"][0]["name"], "kpsewhich");
        Ok(())
    }

    #[tokio::test]
    async fn test_check_requires_execute_process() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(&dir, &[Permission::ReadWorkspace])?;
        let context = ToolExecutionContext::new(SessionId::new(), TurnId::new(), spec);
        let executor = LatexToolProvider::new()
            .executor(&ToolName::new(LATEX_CHECK_TOOL))
            .ok_or(TestError::Missing("executor"))?;
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(LATEX_CHECK_TOOL),
            arguments: json!({ "file": "main.tex" }),
        };
        let output = executor
            .execute(&context, &call)
            .await
            .map_err(ctx("execute"))?;
        assert!(
            matches!(&output, ToolOutput::Error { message } if message.contains("ExecuteProcess")),
            "{output:?}"
        );
        Ok(())
    }

    #[test]
    fn test_fontconfig_escape_and_name_checks() {
        assert_eq!(fontconfig_escape("Noto Sans-CJK"), "Noto Sans\\-CJK");
        assert!(is_plain_name("scrlayer-scrpage"));
        assert!(!is_plain_name("-shell-escape"));
        assert!(!is_plain_name("../x"));
        assert!(!is_font_family_name("\\harwMainFont"));
        assert!(is_font_family_name("DejaVu Sans Mono"));
    }
}
