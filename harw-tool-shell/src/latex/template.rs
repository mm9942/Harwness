//! Werkzeug `latex.template` (Runde 7, Teil T2).
//!
//! # Zweck
//! Legt für den LaTeX-Worker der UIA ein fertiges Gerüst an: das Stilpaket
//! `harw-report.sty` (KOMA `scrartcl`, fontspec, babel, tcolorbox-Kästen,
//! `L`-Spalten, TikZ-Stile, Umbruch-Einstellungen) neben der Hauptdatei und
//! die Hauptdatei selbst aus einer der drei Vorlagen `bericht`,
//! `business-paper` oder `handbuch`. Die Vorlagen stammen aus dem Skill
//! `latex-report` (`harw-home/assets/skills/latex-report/templates/`) und
//! sind per `include_str!` eingebettet.
//!
//! # Platzhalter
//! Die Vorlagen enthalten `%%TITLE%%`, `%%SUBTITLE%%`, `%%AUTHOR%%`,
//! `%%DATE%%`, `%%ACCENT%%`, `%%WARN%%`, `%%MAINFONT%%`, `%%SANSFONT%%`,
//! `%%MONOFONT%%` und `%%LANGUAGE%%`. Sie werden in **einem** Durchgang
//! ersetzt ([`fill_placeholders`]); ein Wert, der selbst wie ein Platzhalter
//! aussieht, wird deshalb nie ein zweites Mal ersetzt.
//!
//! # Sicherheit
//! - **Recht:** `WriteWorkspace`; das Werkzeug steht nicht in der
//!   Auto-Freigabe, jeder Aufruf läuft durch die Freigabe.
//! - **Nie überschreiben:** existiert die Hauptdatei oder `harw-report.sty`
//!   bereits (auch als Symlink), gibt es einen Fehler. Geschrieben wird mit
//!   `create_new` (`O_EXCL`, folgt keinem Symlink).
//! - **Pfad:** relativ zur Workspace-Wurzel oder absolut darunter, nur
//!   normale Pfadbestandteile (kein `..`), Endung `.tex`; der nächste
//!   existierende Vorfahr wird kanonisiert und muss im Workspace liegen.
//! - **Werte:** Titel, Untertitel, Autorin und Datum werden LaTeX-escaped;
//!   Farben müssen sechs Hex-Zeichen sein; Schriftnamen dürfen nur
//!   Buchstaben, Ziffern, Leerzeichen, `-` und `.` enthalten (kein Escaping
//!   nötig, fontspec bekommt den Namen unverändert).

use harw_authority::Permission;
use harw_tools::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolOutput, ToolsError,
    schema::{AdditionalProperties, JsonSchema, JsonSchemaType},
    spec::{FunctionToolSpec, ToolName, ToolSpec},
};
use serde::{Deserialize, Deserializer};
use serde_json::json;
use std::{
    borrow::Cow,
    collections::BTreeMap,
    io::Write,
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use tracing::{info, warn};

/// Name des Werkzeugs.
pub const LATEX_TEMPLATE_TOOL: &str = "latex.template";
/// Dateiname des Stilpakets neben der Hauptdatei.
pub const STYLE_FILE_NAME: &str = "harw-report.sty";
/// Vorgabe der Akzentfarbe (Hex ohne `#`).
pub const DEFAULT_ACCENT: &str = "714B67";
/// Vorgabe der Warnfarbe (Hex ohne `#`).
pub const DEFAULT_WARN: &str = "C0392B";
/// Vorgabe der Grundschrift.
pub const DEFAULT_MAIN_FONT: &str = "DejaVu Serif";
/// Vorgabe der serifenlosen Schrift.
pub const DEFAULT_SANS_FONT: &str = "DejaVu Sans";
/// Vorgabe der Festbreitenschrift.
pub const DEFAULT_MONO_FONT: &str = "DejaVu Sans Mono";
/// Höchstlänge von Titel, Untertitel, Autorin und Datum (Zeichen).
const MAX_TEXT_CHARS: usize = 300;
/// Höchstlänge eines Schriftnamens (Zeichen).
const MAX_FONT_CHARS: usize = 64;

/// Eingebettete Vorlagen aus dem Skill `latex-report`.
const BUNDLED_STYLE: &str =
    include_str!("../../../harw-home/assets/skills/latex-report/templates/harw-report.sty");
const BUNDLED_BERICHT: &str =
    include_str!("../../../harw-home/assets/skills/latex-report/templates/bericht.tex");
const BUNDLED_BUSINESS_PAPER: &str =
    include_str!("../../../harw-home/assets/skills/latex-report/templates/business-paper.tex");
const BUNDLED_HANDBUCH: &str =
    include_str!("../../../harw-home/assets/skills/latex-report/templates/handbuch.tex");

// ── Argumente ─────────────────────────────────────────────────────────────────

/// Die Art des Dokuments — wählt die Vorlage der Hauptdatei.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TemplateKind {
    /// Bericht (Überblick, Stand, Glossar, Quellen).
    Bericht,
    /// Business-Paper nach dem Pyramidenprinzip.
    BusinessPaper,
    /// Handbuch (Überblick, Schritt für Schritt, Referenz).
    Handbuch,
}

impl TemplateKind {
    /// Name wie im Werkzeugschema.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bericht => "bericht",
            Self::BusinessPaper => "business-paper",
            Self::Handbuch => "handbuch",
        }
    }
}

/// Die Dokumentsprache (babel-Sprache und Datumsformat).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TemplateLanguage {
    /// Deutsch (Vorgabe).
    #[default]
    German,
    /// Englisch.
    English,
}

impl TemplateLanguage {
    /// Der babel-Name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::German => "german",
            Self::English => "english",
        }
    }
}

/// Liest ein optionales Textfeld; leere Werte und der String `"null"`
/// (Modelle schicken ihn gelegentlich statt eines fehlenden Felds) gelten
/// als nicht gesetzt.
fn optional_text<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value: Option<String> = Option::deserialize(deserializer)?;
    Ok(value.and_then(|text| {
        let trimmed = text.trim();
        (!trimmed.is_empty() && trimmed != "null").then(|| trimmed.to_owned())
    }))
}

/// Argumente eines `latex.template`-Aufrufs. Unbekannte Felder werden
/// abgelehnt.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LatexTemplateArgs {
    /// Pfad der neuen Hauptdatei (`.tex`).
    file: String,
    /// Vorlage.
    kind: TemplateKind,
    #[serde(default, deserialize_with = "optional_text")]
    title: Option<String>,
    #[serde(default, deserialize_with = "optional_text")]
    subtitle: Option<String>,
    #[serde(default, deserialize_with = "optional_text")]
    author: Option<String>,
    #[serde(default, deserialize_with = "optional_text")]
    date: Option<String>,
    #[serde(default, deserialize_with = "optional_text")]
    accent: Option<String>,
    #[serde(default, deserialize_with = "optional_text")]
    warn: Option<String>,
    #[serde(default, deserialize_with = "optional_text")]
    main_font: Option<String>,
    #[serde(default, deserialize_with = "optional_text")]
    sans_font: Option<String>,
    #[serde(default, deserialize_with = "optional_text")]
    mono_font: Option<String>,
    #[serde(default)]
    language: Option<TemplateLanguage>,
}

// ── Reine Helfer ──────────────────────────────────────────────────────────────

/// Escaped Text für LaTeX-Fließtext (Titel, Autorin, Datum).
///
/// # Beschreibung
/// Ersetzt die zehn Sonderzeichen (`\ { } $ & # _ % ~ ^`) durch ihre
/// Textbefehle und macht aus Zeilenumbrüchen und Steuerzeichen Leerzeichen.
///
/// # Argumente
/// - `text` (`&str`): der Rohtext.
///
/// # Rückgabe
/// Der escapte Text.
#[must_use]
pub fn latex_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\\' => escaped.push_str("\\textbackslash{}"),
            '{' => escaped.push_str("\\{"),
            '}' => escaped.push_str("\\}"),
            '$' => escaped.push_str("\\$"),
            '&' => escaped.push_str("\\&"),
            '#' => escaped.push_str("\\#"),
            '_' => escaped.push_str("\\_"),
            '%' => escaped.push_str("\\%"),
            '~' => escaped.push_str("\\textasciitilde{}"),
            '^' => escaped.push_str("\\textasciicircum{}"),
            c if c.is_control() => escaped.push(' '),
            c => escaped.push(c),
        }
    }
    escaped
}

/// Prüft eine Hex-Farbe (sechs Hex-Zeichen, optional mit `#`).
///
/// # Rückgabe
/// Die Farbe in Großbuchstaben ohne `#`.
///
/// # Errors
/// Eine lesbare Meldung, wenn der Wert keine sechs Hex-Zeichen hat.
pub fn normalize_hex_color(field: &str, value: &str) -> Result<String, String> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(hex.to_ascii_uppercase())
    } else {
        Err(format!(
            "{field}: „{value}“ ist keine Hex-Farbe (sechs Zeichen 0-9/A-F, z. B. 714B67)"
        ))
    }
}

/// Prüft einen Schriftnamen (Buchstaben, Ziffern, Leerzeichen, `-`, `.`).
///
/// # Errors
/// Eine lesbare Meldung bei leerem, zu langem oder unzulässigem Namen.
pub fn validate_font_name(field: &str, value: &str) -> Result<String, String> {
    let name = value.trim();
    let allowed = |c: char| c.is_alphanumeric() || c == ' ' || c == '-' || c == '.';
    if name.is_empty() || name.chars().count() > MAX_FONT_CHARS || !name.chars().all(allowed) {
        return Err(format!(
            "{field}: „{value}“ ist kein zulässiger Schriftname (Buchstaben, Ziffern, \
             Leerzeichen, - und ., höchstens {MAX_FONT_CHARS} Zeichen)"
        ));
    }
    Ok(name.to_owned())
}

/// Prüft die Länge eines Textfelds und escaped es.
fn escaped_text(field: &str, value: &str) -> Result<String, String> {
    if value.chars().count() > MAX_TEXT_CHARS {
        return Err(format!(
            "{field}: höchstens {MAX_TEXT_CHARS} Zeichen erlaubt"
        ));
    }
    Ok(latex_escape(value))
}

/// Formatiert ein Datum ausgeschrieben: „24. September 2026“ (Deutsch) bzw.
/// „September 24, 2026“ (Englisch).
///
/// # Argumente
/// - `date` (`jiff::civil::Date`): das Datum.
/// - `language` (`TemplateLanguage`): Sprache der Monatsnamen.
#[must_use]
pub fn format_long_date(date: jiff::civil::Date, language: TemplateLanguage) -> String {
    const GERMAN: [&str; 12] = [
        "Januar",
        "Februar",
        "März",
        "April",
        "Mai",
        "Juni",
        "Juli",
        "August",
        "September",
        "Oktober",
        "November",
        "Dezember",
    ];
    const ENGLISH: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let index = usize::try_from(date.month()).unwrap_or(1).clamp(1, 12) - 1;
    match language {
        TemplateLanguage::German => {
            format!("{}. {} {}", date.day(), GERMAN[index], date.year())
        }
        TemplateLanguage::English => {
            format!("{} {}, {}", ENGLISH[index], date.day(), date.year())
        }
    }
}

/// Ersetzt `%%NAME%%`-Platzhalter in einem Durchgang.
///
/// # Beschreibung
/// Nur Namen aus `values` werden ersetzt; alles andere (auch `%%`-Kommentare
/// in LaTeX) bleibt stehen. Eingesetzte Werte werden nicht erneut
/// durchsucht.
///
/// # Argumente
/// - `template` (`&str`): Vorlagentext.
/// - `values` (`&BTreeMap<&str, String>`): Name (ohne `%%`) → Wert.
///
/// # Rückgabe
/// Der gefüllte Text.
#[must_use]
pub fn fill_placeholders(template: &str, values: &BTreeMap<&str, String>) -> String {
    let mut filled = String::with_capacity(template.len() + 256);
    let mut rest = template;
    while let Some(start) = rest.find("%%") {
        filled.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        if let Some(end) = after.find("%%")
            && let Some(value) = values.get(&after[..end])
        {
            filled.push_str(value);
            rest = &after[end + 2..];
            continue;
        }
        filled.push_str("%%");
        rest = after;
    }
    filled.push_str(rest);
    filled
}

/// Prüft den Zielpfad einer neuen `.tex`-Datei.
///
/// # Beschreibung
/// Erlaubt nur normale Pfadbestandteile (kein `..`, kein `.`); ein absoluter
/// Pfad muss unter `root` liegen. Der nächste existierende Vorfahr wird
/// kanonisiert und muss ein Ordner unter `root` sein (kein Symlink-Ausbruch).
///
/// # Argumente
/// - `root` (`&Path`): kanonische Workspace-Wurzel.
/// - `file` (`&str`): Pfad aus dem Aufruf.
///
/// # Rückgabe
/// Den (noch nicht existierenden) Zielpfad unter `root`.
///
/// # Errors
/// Eine lesbare Meldung bei leerem Pfad, falscher Endung, `..`, einem Pfad
/// außerhalb des Workspaces oder einem Vorfahr, der kein Ordner ist.
pub fn resolve_new_tex_file(root: &Path, file: &str) -> Result<PathBuf, String> {
    let trimmed = file.trim();
    if trimmed.is_empty() {
        return Err("file darf nicht leer sein".to_owned());
    }
    let candidate = Path::new(trimmed);
    let relative = if candidate.is_absolute() {
        candidate
            .strip_prefix(root)
            .map_err(|_| format!("{trimmed}: liegt außerhalb des Workspaces"))?
    } else {
        candidate
    };
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "{trimmed}: nur normale Pfadbestandteile erlaubt (kein .., kein .)"
        ));
    }
    let is_tex = relative
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tex"));
    if !is_tex {
        return Err(format!("{trimmed}: die Hauptdatei muss auf .tex enden"));
    }
    let target = root.join(relative);
    let mut ancestor = target.parent();
    while let Some(dir) = ancestor {
        if dir.symlink_metadata().is_ok() {
            let canonical = dir
                .canonicalize()
                .map_err(|err| format!("{trimmed}: Ordner nicht auflösbar ({err})"))?;
            if !canonical.starts_with(root) {
                return Err(format!(
                    "{trimmed}: liegt außerhalb des Workspaces (auch Symlinks werden aufgelöst)"
                ));
            }
            if !canonical.is_dir() {
                return Err(format!("{trimmed}: {} ist kein Ordner", dir.display()));
            }
            return Ok(target);
        }
        ancestor = dir.parent();
    }
    Err(format!("{trimmed}: kein existierender Ordner im Pfad"))
}

/// Schreibt `contents` in eine **neue** Datei (`create_new`).
fn write_new(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut handle = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    handle.write_all(contents.as_bytes())?;
    handle.sync_all()
}

// ── Ausführung ────────────────────────────────────────────────────────────────

/// Die Vorlagentexte (eingebettet oder, in Tests, eigene).
#[derive(Debug, Clone)]
pub(crate) struct TemplateSources {
    /// `harw-report.sty`.
    pub(crate) style: Cow<'static, str>,
    /// Hauptdatei `bericht`.
    pub(crate) bericht: Cow<'static, str>,
    /// Hauptdatei `business-paper`.
    pub(crate) business_paper: Cow<'static, str>,
    /// Hauptdatei `handbuch`.
    pub(crate) handbuch: Cow<'static, str>,
}

impl TemplateSources {
    /// Die eingebetteten Vorlagen des Skills `latex-report`.
    fn bundled() -> Self {
        Self {
            style: Cow::Borrowed(BUNDLED_STYLE),
            bericht: Cow::Borrowed(BUNDLED_BERICHT),
            business_paper: Cow::Borrowed(BUNDLED_BUSINESS_PAPER),
            handbuch: Cow::Borrowed(BUNDLED_HANDBUCH),
        }
    }

    /// Die Vorlage der Hauptdatei zu `kind`.
    fn main_for(&self, kind: TemplateKind) -> &str {
        match kind {
            TemplateKind::Bericht => &self.bericht,
            TemplateKind::BusinessPaper => &self.business_paper,
            TemplateKind::Handbuch => &self.handbuch,
        }
    }
}

/// Die geprüften, eingesetzten Werte eines Aufrufs.
struct TemplateValues {
    placeholders: BTreeMap<&'static str, String>,
    language: TemplateLanguage,
}

/// Prüft die Argumente und baut die Platzhalterwerte.
///
/// # Errors
/// Eine lesbare Meldung für die erste ungültige Angabe.
fn template_values(
    args: &LatexTemplateArgs,
    today: jiff::civil::Date,
) -> Result<TemplateValues, String> {
    let language = args.language.unwrap_or_default();
    let default_title = match language {
        TemplateLanguage::German => "Titel des Dokuments",
        TemplateLanguage::English => "Document title",
    };
    let title = escaped_text("title", args.title.as_deref().unwrap_or(default_title))?;
    let subtitle = escaped_text("subtitle", args.subtitle.as_deref().unwrap_or_default())?;
    let author = escaped_text("author", args.author.as_deref().unwrap_or_default())?;
    let date = match &args.date {
        Some(date) => escaped_text("date", date)?,
        None => format_long_date(today, language),
    };
    let accent = normalize_hex_color("accent", args.accent.as_deref().unwrap_or(DEFAULT_ACCENT))?;
    let warn = normalize_hex_color("warn", args.warn.as_deref().unwrap_or(DEFAULT_WARN))?;
    let main_font = validate_font_name(
        "main_font",
        args.main_font.as_deref().unwrap_or(DEFAULT_MAIN_FONT),
    )?;
    let sans_font = validate_font_name(
        "sans_font",
        args.sans_font.as_deref().unwrap_or(DEFAULT_SANS_FONT),
    )?;
    let mono_font = validate_font_name(
        "mono_font",
        args.mono_font.as_deref().unwrap_or(DEFAULT_MONO_FONT),
    )?;
    let placeholders = BTreeMap::from([
        ("TITLE", title),
        ("SUBTITLE", subtitle),
        ("AUTHOR", author),
        ("DATE", date),
        ("ACCENT", accent),
        ("WARN", warn),
        ("MAINFONT", main_font),
        ("SANSFONT", sans_font),
        ("MONOFONT", mono_font),
        ("LANGUAGE", language.as_str().to_owned()),
    ]);
    Ok(TemplateValues {
        placeholders,
        language,
    })
}

/// Ausführer eines `latex.template`-Aufrufs.
pub(crate) struct LatexTemplateExecutor {
    templates: Arc<TemplateSources>,
    /// Festes „heute“ für Tests; `None` nimmt das lokale Datum.
    today: Option<jiff::civil::Date>,
}

impl LatexTemplateExecutor {
    /// Ausführer mit den eingebetteten Vorlagen.
    pub(crate) fn bundled() -> Self {
        Self {
            templates: Arc::new(TemplateSources::bundled()),
            today: None,
        }
    }

    /// Ausführer mit eigenen Vorlagen und festem Datum (Tests).
    #[cfg(test)]
    pub(crate) fn with_templates(templates: TemplateSources, today: jiff::civil::Date) -> Self {
        Self {
            templates: Arc::new(templates),
            today: Some(today),
        }
    }

    /// Der eigentliche Ablauf nach Parsen und Rechteprüfung.
    fn run(&self, args: &LatexTemplateArgs, root: &Path) -> ToolOutput {
        let error =
            |message: String| ToolOutput::error(format!("{LATEX_TEMPLATE_TOOL}: {message}"));
        let today = self.today.unwrap_or_else(|| jiff::Zoned::now().date());
        let values = match template_values(args, today) {
            Ok(values) => values,
            Err(message) => return error(message),
        };
        let target = match resolve_new_tex_file(root, &args.file) {
            Ok(target) => target,
            Err(message) => return error(message),
        };
        let (Some(parent), Some(file_name)) = (target.parent(), target.file_name()) else {
            return error(format!("{}: ungültiger Pfad", args.file));
        };
        if let Err(err) = std::fs::create_dir_all(parent) {
            return error(format!("Ordner {} nicht anlegbar: {err}", parent.display()));
        }
        // Nach dem Anlegen erneut kanonisieren: der Ordner muss weiter im
        // Workspace liegen (Schutz gegen einen zwischenzeitlichen Symlink).
        let parent = match parent.canonicalize() {
            Ok(parent) if parent.starts_with(root) => parent,
            Ok(_) => return error(format!("{}: liegt außerhalb des Workspaces", args.file)),
            Err(err) => return error(format!("Ordner nicht auflösbar: {err}")),
        };
        let main_path = parent.join(file_name);
        let style_path = parent.join(STYLE_FILE_NAME);
        for path in [&main_path, &style_path] {
            if path.symlink_metadata().is_ok() {
                return error(format!(
                    "{} existiert bereits; latex.template überschreibt nie. Anderen \
                     Dateinamen oder einen neuen Ordner wählen.",
                    super::relative(root, path)
                ));
            }
        }
        let style = fill_placeholders(&self.templates.style, &values.placeholders);
        let main = fill_placeholders(self.templates.main_for(args.kind), &values.placeholders);
        if let Err(err) = write_new(&style_path, &style) {
            warn!(error = %err, "latex.template: Stil nicht schreibbar");
            return error(format!(
                "{} nicht schreibbar: {err}",
                super::relative(root, &style_path)
            ));
        }
        if let Err(err) = write_new(&main_path, &main) {
            warn!(error = %err, "latex.template: Hauptdatei nicht schreibbar");
            // Nichts halb Angelegtes zurücklassen: die eben geschriebene
            // Stildatei wieder entfernen.
            if let Err(remove_err) = std::fs::remove_file(&style_path) {
                warn!(error = %remove_err, "latex.template: Aufräumen fehlgeschlagen");
            }
            return error(format!(
                "{} nicht schreibbar: {err}",
                super::relative(root, &main_path)
            ));
        }
        let main_rel = super::relative(root, &main_path);
        info!(kind = args.kind.as_str(), file = %main_rel, "latex.template geschrieben");
        let placeholders = &values.placeholders;
        ToolOutput::json(json!({
            "status": "ok",
            "kind": args.kind.as_str(),
            "main": main_rel,
            "style": super::relative(root, &style_path),
            "language": values.language.as_str(),
            "date": placeholders.get("DATE"),
            "accent": placeholders.get("ACCENT"),
            "warn": placeholders.get("WARN"),
            "fonts": {
                "main": placeholders.get("MAINFONT"),
                "sans": placeholders.get("SANSFONT"),
                "mono": placeholders.get("MONOFONT"),
            },
            "next_steps": "Fill the placeholder text (keep % TODO: for facts the task does \
                not provide), run latex.check on the main file, then latex.build.",
        }))
    }
}

impl ToolExecutor for LatexTemplateExecutor {
    /// Führt `latex.template` aus.
    ///
    /// # Beschreibung
    /// 1. Argumente parsen (unbekannte Felder werden abgelehnt).
    /// 2. `WriteWorkspace` prüfen.
    /// 3. Werte prüfen, Pfad prüfen, beide Dateien neu anlegen.
    ///
    /// # Errors
    /// [`ToolsError::InvalidArguments`] bei unpassenden Argumenten. Alles
    /// andere kommt als [`ToolOutput`] zurück.
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let args: LatexTemplateArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|err| {
                    ToolsError::InvalidArguments {
                        name: LATEX_TEMPLATE_TOOL.to_owned(),
                        reason: err.to_string(),
                    }
                })?;
            if let Some(denied) = harw_tools::sandbox_guard::require_permission(
                context,
                Permission::WriteWorkspace,
                LATEX_TEMPLATE_TOOL,
            ) {
                warn!("latex.template denied");
                return Ok(denied);
            }
            let root = context.sandbox().workspace().canonical_root().to_path_buf();
            Ok(self.run(&args, &root))
        })
    }
}

// ── Schema ────────────────────────────────────────────────────────────────────

/// Ein optionales String-Feld fürs Schema.
fn string_property(description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::String),
        description: Some(description.to_owned()),
        ..Default::default()
    }
}

/// Die Werkzeugbeschreibung für das Modell.
pub(crate) fn tool_spec() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "file".to_owned(),
        string_property(
            "Path of the NEW main .tex file inside the workspace (relative to the workspace \
             root). harw-report.sty is written next to it. Neither file may exist yet.",
        ),
    );
    properties.insert(
        "kind".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Document type: bericht (report), business-paper, handbuch (manual).".to_owned(),
            ),
            enum_values: Some(vec![
                json!("bericht"),
                json!("business-paper"),
                json!("handbuch"),
            ]),
            ..Default::default()
        },
    );
    for (name, description) in [
        (
            "title",
            "Document title (plain text, escaped). Only from the task.",
        ),
        ("subtitle", "Subtitle (plain text, escaped); default empty."),
        (
            "author",
            "Author (plain text, escaped); only from the task, default empty.",
        ),
        (
            "date",
            "Date as text; default today, e.g. 24. September 2026.",
        ),
        (
            "accent",
            "Accent colour as 6 hex digits; default 714B67. Only if requested.",
        ),
        (
            "warn",
            "Warning colour as 6 hex digits; default C0392B. Only if requested.",
        ),
        (
            "main_font",
            "Main font family; default DejaVu Serif. Only if requested.",
        ),
        (
            "sans_font",
            "Sans font family; default DejaVu Sans. Only if requested.",
        ),
        (
            "mono_font",
            "Monospace font family; default DejaVu Sans Mono. Only if requested.",
        ),
    ] {
        properties.insert(name.to_owned(), string_property(description));
    }
    properties.insert(
        "language".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some("Document language; default german.".to_owned()),
            enum_values: Some(vec![json!("german"), json!("english")]),
            ..Default::default()
        },
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(LATEX_TEMPLATE_TOOL),
        description: "Create a new LaTeX document from the bundled Harwness report template: \
            writes harw-report.sty (KOMA scrartcl, fontspec, babel, boxes merke/achtung/\
            beispiel, L columns, TikZ styles) and the main file skeleton for kind \
            bericht|business-paper|handbuch. Never overwrites (error if either file exists). \
            Colours and fonts only when the user asked for them, otherwise the defaults apply. \
            Requires WriteWorkspace."
            .to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(properties),
            required: Some(vec!["file".to_owned(), "kind".to_owned()]),
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
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use serde_json::Value;
    use std::fs;
    use tempfile::TempDir;

    const TEST_STYLE: &str = "% stil\n\\ProvidesPackage{harw-report}\n";
    const TEST_MAIN: &str = "%% Kommentar\n\\newcommand{\\harwLanguage}{%%LANGUAGE%%}\n\
        \\newcommand{\\harwAccentHex}{%%ACCENT%%}\n\\newcommand{\\harwWarnHex}{%%WARN%%}\n\
        \\newcommand{\\harwMainFont}{%%MAINFONT%%}\n\\newcommand{\\harwSansFont}{%%SANSFONT%%}\n\
        \\newcommand{\\harwMonoFont}{%%MONOFONT%%}\n\\usepackage{harw-report}\n\
        \\title{%%TITLE%%}\n\\subtitle{%%SUBTITLE%%}\n\\author{%%AUTHOR%%}\n\\date{%%DATE%%}\n\
        % %%UNBEKANNT%% bleibt\n";

    fn test_sources() -> TemplateSources {
        TemplateSources {
            style: Cow::Borrowed(TEST_STYLE),
            bericht: Cow::Owned(format!("% bericht\n{TEST_MAIN}")),
            business_paper: Cow::Owned(format!("% business-paper\n{TEST_MAIN}")),
            handbuch: Cow::Owned(format!("% handbuch\n{TEST_MAIN}")),
        }
    }

    fn today() -> TestResult<jiff::civil::Date> {
        jiff::civil::Date::new(2026, 9, 24).map_err(ctx("date"))
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

    async fn run(
        executor: &LatexTemplateExecutor,
        spec: SandboxSpec,
        arguments: Value,
    ) -> Result<ToolOutput, ToolsError> {
        let context = ToolExecutionContext::new(SessionId::new(), TurnId::new(), spec);
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(LATEX_TEMPLATE_TOOL),
            arguments,
        };
        executor.execute(&context, &call).await
    }

    fn rights() -> [Permission; 2] {
        [Permission::ReadWorkspace, Permission::WriteWorkspace]
    }

    #[tokio::test]
    async fn test_template_writes_style_and_skeleton_with_colours_and_fonts() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(&dir, &rights())?;
        let root = spec.workspace().canonical_root().to_path_buf();
        let executor = LatexTemplateExecutor::with_templates(test_sources(), today()?);
        let output = run(
            &executor,
            spec,
            json!({
                "file": "docs/paper.tex",
                "kind": "business-paper",
                "title": "Kosten & Nutzen: 50 % mehr_Tempo",
                "author": "null",
                "accent": "#1a2B3c",
                "main_font": "Noto Serif",
                "language": "english",
            }),
        )
        .await
        .map_err(ctx("execute"))?;
        let ToolOutput::Json { content } = &output else {
            return Err(TestError::Unexpected(format!("{output:?}")));
        };
        assert_eq!(content["status"], "ok");
        assert_eq!(content["main"], "docs/paper.tex");
        assert_eq!(content["style"], "docs/harw-report.sty");
        let style = fs::read_to_string(root.join("docs/harw-report.sty")).map_err(ctx("sty"))?;
        assert_eq!(style, TEST_STYLE);
        let main = fs::read_to_string(root.join("docs/paper.tex")).map_err(ctx("main"))?;
        for needle in [
            "% business-paper",
            "%% Kommentar",
            "\\newcommand{\\harwLanguage}{english}",
            "\\newcommand{\\harwAccentHex}{1A2B3C}",
            "\\newcommand{\\harwWarnHex}{C0392B}",
            "\\newcommand{\\harwMainFont}{Noto Serif}",
            "\\newcommand{\\harwSansFont}{DejaVu Sans}",
            "\\newcommand{\\harwMonoFont}{DejaVu Sans Mono}",
            "\\title{Kosten \\& Nutzen: 50 \\% mehr\\_Tempo}",
            "\\subtitle{}",
            "\\author{}",
            "\\date{September 24, 2026}",
            "% %%UNBEKANNT%% bleibt",
        ] {
            assert!(main.contains(needle), "{needle}:\n{main}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_template_never_overwrites_existing_files() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(&dir, &rights())?;
        let root = spec.workspace().canonical_root().to_path_buf();
        let executor = LatexTemplateExecutor::with_templates(test_sources(), today()?);
        // Hauptdatei existiert → Fehler, Stil wird nicht angelegt.
        fs::write(root.join("bericht.tex"), "eigen").map_err(ctx("tex"))?;
        let output = run(
            &executor,
            spec.clone(),
            json!({ "file": "bericht.tex", "kind": "bericht" }),
        )
        .await
        .map_err(ctx("execute"))?;
        assert!(
            matches!(&output, ToolOutput::Error { message } if message.contains("überschreibt nie")),
            "{output:?}"
        );
        assert_eq!(
            fs::read_to_string(root.join("bericht.tex")).map_err(ctx("read"))?,
            "eigen"
        );
        assert!(!root.join(STYLE_FILE_NAME).exists());
        // Stil existiert → Fehler, Hauptdatei wird nicht angelegt.
        fs::write(root.join(STYLE_FILE_NAME), "eigener stil").map_err(ctx("sty"))?;
        let output = run(
            &executor,
            spec,
            json!({ "file": "neu.tex", "kind": "handbuch" }),
        )
        .await
        .map_err(ctx("execute"))?;
        assert!(matches!(output, ToolOutput::Error { .. }), "{output:?}");
        assert!(!root.join("neu.tex").exists());
        assert_eq!(
            fs::read_to_string(root.join(STYLE_FILE_NAME)).map_err(ctx("read"))?,
            "eigener stil"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_template_rejects_bad_values_paths_and_missing_permission() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spec = sandbox(&dir, &rights())?;
        let executor = LatexTemplateExecutor::with_templates(test_sources(), today()?);
        for arguments in [
            json!({ "file": "a.tex", "kind": "bericht", "accent": "violet" }),
            json!({ "file": "a.tex", "kind": "bericht", "warn": "12345" }),
            json!({ "file": "a.tex", "kind": "bericht", "main_font": "Evil}\\input{x" }),
            json!({ "file": "../a.tex", "kind": "bericht" }),
            json!({ "file": "a.txt", "kind": "bericht" }),
            json!({ "file": "/etc/a.tex", "kind": "bericht" }),
        ] {
            let output = run(&executor, spec.clone(), arguments.clone())
                .await
                .map_err(ctx("execute"))?;
            assert!(
                matches!(output, ToolOutput::Error { .. }),
                "{arguments}: {output:?}"
            );
        }
        let result = run(
            &executor,
            spec,
            json!({ "file": "a.tex", "kind": "poster" }),
        )
        .await;
        assert!(
            matches!(result, Err(ToolsError::InvalidArguments { .. })),
            "{result:?}"
        );
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let read_only = sandbox(&dir, &[Permission::ReadWorkspace])?;
        let output = run(
            &executor,
            read_only,
            json!({ "file": "a.tex", "kind": "bericht" }),
        )
        .await
        .map_err(ctx("execute"))?;
        assert!(
            matches!(&output, ToolOutput::Error { message } if message.contains("WriteWorkspace")),
            "{output:?}"
        );
        Ok(())
    }

    #[test]
    fn test_template_symlinked_folder_outside_workspace_is_rejected() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let root = dir.path().join("ws");
        fs::create_dir_all(&root).map_err(ctx("root"))?;
        let root = root.canonicalize().map_err(ctx("canon"))?;
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).map_err(ctx("outside"))?;
        std::os::unix::fs::symlink(&outside, root.join("link")).map_err(ctx("symlink"))?;
        assert!(resolve_new_tex_file(&root, "link/a.tex").is_err());
        assert!(resolve_new_tex_file(&root, "neu/tief/a.tex").is_ok());
        assert!(resolve_new_tex_file(&root, "./a.tex").is_err());
        Ok(())
    }

    #[test]
    fn test_fill_placeholders_is_single_pass() {
        let values = BTreeMap::from([
            ("TITLE", "%%AUTHOR%%".to_owned()),
            ("AUTHOR", "Autorin".to_owned()),
        ]);
        assert_eq!(
            fill_placeholders("%%TITLE%% / %%AUTHOR%% %% x", &values),
            "%%AUTHOR%% / Autorin %% x"
        );
    }

    #[test]
    fn test_latex_escape_and_dates() -> TestResult {
        assert_eq!(
            latex_escape("a\\b{c}$&#_%~^\nz"),
            "a\\textbackslash{}b\\{c\\}\\$\\&\\#\\_\\%\\textasciitilde{}\\textasciicircum{} z"
        );
        let date = today()?;
        assert_eq!(
            format_long_date(date, TemplateLanguage::German),
            "24. September 2026"
        );
        assert_eq!(
            format_long_date(date, TemplateLanguage::English),
            "September 24, 2026"
        );
        let march = jiff::civil::Date::new(2027, 3, 1).map_err(ctx("date"))?;
        assert_eq!(
            format_long_date(march, TemplateLanguage::German),
            "1. März 2027"
        );
        Ok(())
    }

    /// Die eingebetteten Vorlagen stammen aus dem Skill und tragen alle
    /// Platzhalter; der Stil ist platzhalterfrei.
    #[test]
    fn test_bundled_templates_carry_all_placeholders() {
        let sources = TemplateSources::bundled();
        for kind in [
            TemplateKind::Bericht,
            TemplateKind::BusinessPaper,
            TemplateKind::Handbuch,
        ] {
            let main = sources.main_for(kind);
            for name in [
                "TITLE", "SUBTITLE", "AUTHOR", "DATE", "ACCENT", "WARN", "MAINFONT", "SANSFONT",
                "MONOFONT", "LANGUAGE",
            ] {
                let placeholder = format!("%%{name}%%");
                assert!(main.contains(&placeholder), "{kind:?}: {placeholder}");
            }
            assert!(main.contains("\\usepackage{harw-report}"), "{kind:?}");
        }
        assert!(sources.style.contains("\\ProvidesPackage{harw-report}"));
    }
}
