//! Grammatik von `harw agent`: Agentin, Skills und Plugins verwalten, und
//! der Agenten-Compiler (#22 Welle 2B: `check`, `build`, `inspect`, `graph`,
//! `explain`, `new`, `fmt`, `diff`, `test`, `run`, `versions`, `use`,
//! `clean`, `doctor`). Jeder Befehl kennt das globale `--json`.

use std::path::PathBuf;

use clap::{Subcommand, ValueHint};

/// Aktionen des `harw agent`-Subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum AgentAction {
    /// Richtet interaktiv eine neue Benutzeroberflächen-Agentin ein.
    UiaNew,
    /// Listet die startbaren Agenten (eingebaut und benutzerdefiniert) mit
    /// Rolle, Herkunft und Beschreibung; optional gefiltert.
    List {
        /// Optionaler Suchbegriff (Name oder Beschreibung).
        #[arg(value_name = "SUCHE", value_hint = ValueHint::Other)]
        query: Option<String>,
    },
    /// Verwaltet die Skills des aktiven Profils.
    Skills {
        /// Argumente für die Skill-Verwaltung (z. B. `list`).
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            num_args = 0..,
            value_name = "ARGS",
            value_hint = ValueHint::Other
        )]
        args: Vec<String>,
    },
    /// Prüft Definitionen (Parsen, Auflösen, Senken, Compiler-Pässe) und
    /// zeigt Diagnosen mit Code, Datei:Zeile, Hilfe und Ausschnitt; Exit-Code 1
    /// bei Fehlern. Ohne Argument: alle eigenen Definitionen.
    Check {
        /// Definitionsnamen oder Pfade (Ordner mit `definition.toml` oder Datei).
        #[arg(value_name = "NAME|PFAD", value_hint = ValueHint::AnyPath)]
        targets: Vec<String>,
    },
    /// Kompiliert einen Agenten nach `~/.harw/bin/<name>` (versioniert).
    Build {
        /// Definitionsname oder Pfad.
        #[arg(value_name = "NAME|PFAD", value_hint = ValueHint::AnyPath)]
        agent: String,
        /// Eingebaute Schnittstellen (überschreibt `[binary].interfaces`):
        /// cli, repl, mcp, http, tui.
        #[arg(long = "interface", value_delimiter = ',', value_name = "LISTE")]
        interface: Vec<String>,
        /// Natives Backend: erzeugte Crate plus `cargo build --release`.
        #[arg(long, conflicts_with = "artifact_only")]
        native: bool,
        /// Nur das Artefakt (`.harwa`), ohne Runner.
        #[arg(long)]
        artifact_only: bool,
        /// Runner-Binary (Vorgabe: neben `harw`, dann `~/.harw`).
        #[arg(long, value_name = "PFAD", value_hint = ValueHint::FilePath)]
        runner: Option<PathBuf>,
        /// Zusätzliche Kopie des Ergebnisses (Datei oder Ordner).
        #[arg(short = 'o', long = "output", value_name = "PFAD", value_hint = ValueHint::AnyPath)]
        output: Option<PathBuf>,
        /// Ziel-Triple (Vorgabe: der Host).
        #[arg(long = "target", value_name = "TRIPLE")]
        target_triple: Option<String>,
        /// harw-Quellen für `--native` (Vorgabe: `HARW_SRC`, dann der
        /// Installationsvermerk von `make install`).
        #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
        harw_src: Option<PathBuf>,
    },
    /// Zeigt Manifest, Hashes, Schnittstellen, Skills, Kinder und Modelle
    /// eines Binaries, Artefakts oder installierten Agenten.
    Inspect {
        /// Datei oder Name in `~/.harw/bin`.
        #[arg(value_name = "BINARY|ARTEFAKT|NAME", value_hint = ValueHint::AnyPath)]
        target: String,
    },
    /// Delegations-, Auflösungs- und Rechte-Graphen.
    Graph {
        /// Definitionsname oder Pfad.
        #[arg(value_name = "NAME|PFAD", required_unless_present = "all")]
        agent: Option<String>,
        /// Alle eigenen Definitionen.
        #[arg(long)]
        all: bool,
        /// text, dot, mermaid oder json.
        #[arg(long, default_value = "text", value_name = "FORMAT")]
        format: String,
        /// delegation, resolution, rights oder all.
        #[arg(long, default_value = "all", value_name = "ART")]
        kind: String,
    },
    /// Erklärt die Herkunft von IR-Werten (Schicht, Datei:Zeile, Patch) oder
    /// einen Diagnose-Code (`HARW-PATCH-003`).
    Explain {
        /// Definitionsname, Pfad oder Diagnose-Code.
        #[arg(value_name = "NAME|CODE")]
        target: String,
        /// Feldpfad (`tools.admitted`, `spawn.max_depth`, `tool:fs.read`).
        #[arg(value_name = "FELD")]
        field: Option<String>,
    },
    /// Legt eine kommentierte `definition.toml` samt `system.md` an.
    New {
        /// Agentenname (`[a-z0-9-]`).
        #[arg(value_name = "NAME")]
        name: String,
        /// worker oder child-orchestrator.
        #[arg(long, default_value = "worker", value_name = "ROLLE")]
        role: String,
        /// Basisdefinition (Vorgabe: die Basis der Rolle).
        #[arg(long, value_name = "ID")]
        extends: Option<String>,
        /// Zielordner (Vorgabe: `~/.harw/agents/<name>`).
        #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
        dir: Option<PathBuf>,
    },
    /// Formatiert Definitionen kanonisch (Kommentare bleiben erhalten).
    Fmt {
        /// Dateien oder Ordner (Vorgabe: `agents/` aller Layer).
        #[arg(value_name = "PFAD", value_hint = ValueHint::AnyPath)]
        paths: Vec<PathBuf>,
        /// Nur prüfen; Exit-Code 1, wenn etwas neu formatiert würde.
        #[arg(long)]
        check: bool,
    },
    /// IR-Differenz zweier Definitionen, Versionen (`name@version`) oder
    /// Artefakte, Rechte-Delta zuerst.
    Diff {
        /// Linke Seite.
        #[arg(value_name = "A")]
        left: String,
        /// Rechte Seite.
        #[arg(value_name = "B")]
        right: String,
    },
    /// Validiert, baut im Speicher und führt die Beispielfälle aus `tests/*.toml` aus.
    Test {
        /// Definitionsname oder Pfad (Vorgabe: alle mit `tests/`).
        #[arg(value_name = "NAME|PFAD")]
        agent: Option<String>,
    },
    /// Führt ein Artefakt direkt aus (braucht den Runner, #22 Welle 3).
    Run {
        /// Artefakt oder Name.
        #[arg(value_name = "ARTEFAKT|NAME")]
        target: String,
        /// Auftrag.
        #[arg(trailing_var_arg = true, num_args = 0.., value_name = "PROMPT")]
        prompt: Vec<String>,
    },
    /// Listet die installierten Versionen eines kompilierten Agenten.
    Versions {
        /// Name in `~/.harw/bin`.
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Schaltet `~/.harw/bin/<name>` auf eine andere Version um.
    Use {
        /// Name in `~/.harw/bin`.
        #[arg(value_name = "NAME")]
        name: String,
        /// Version, Digest-Präfix oder Versionsordner.
        #[arg(value_name = "VERSION|DIGEST")]
        version: String,
    },
    /// Räumt den nativen Build-Cache und alte Versionen auf.
    Clean {
        /// Alles außer den aktuellen Versionen.
        #[arg(long)]
        all: bool,
        /// Cache-Einträge, die so viele Tage unbenutzt sind.
        #[arg(long, value_name = "TAGE")]
        older_than: Option<u64>,
        /// Ältere Versionen je Agent, die bleiben (Vorgabe 3).
        #[arg(long, value_name = "N")]
        keep: Option<usize>,
        /// Nur anzeigen, was entfernt würde.
        #[arg(long)]
        dry_run: bool,
    },
    /// Prüft Runner, native Voraussetzungen, Installationsvermerk, Cache
    /// und `~/.harw/bin` im PATH.
    Doctor,
    /// Schreibt den Installationsvermerk `~/.harw/install.toml` (für `make install`).
    #[command(hide = true)]
    InstallRecord {
        /// Die harw-Quellen.
        #[arg(long, value_name = "DIR")]
        source_dir: Option<PathBuf>,
        /// Wohin harw installiert wurde.
        #[arg(long, value_name = "DIR")]
        bindir: Option<PathBuf>,
    },
    /// Baut die aktive UIA automatisch, falls nötig (Start, `make install`).
    #[command(hide = true)]
    AutoBuildUia {
        /// Die Sperre des Hintergrundstarts danach freigeben.
        #[arg(long)]
        release_lock: bool,
    },
    /// Verwaltet die Plugins des aktiven Profils.
    Plugins {
        /// Argumente für die Plugin-Verwaltung (z. B. `list`).
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            num_args = 0..,
            value_name = "ARGS",
            value_hint = ValueHint::Other
        )]
        args: Vec<String>,
    },
}
