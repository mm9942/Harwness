//! `/skills` — Skill-Katalog und Aktivierungszustand.
//!
//! `list` und `show` lesen den konfigurierten Skill-Katalog aus dem
//! `Arc<ResolvedConfig>`-Service in [`OpContext`]. Ohne diesen Service bleibt
//! die Operation fail-closed ([`OpError::NotAvailable`]).
//!
//! `activate` und `deactivate` schalten das `enabled`-Feld des Skill-Manifests
//! (`skills/<dir>/skill.toml`) dauerhaft um. Geschrieben wird über
//! [`harw_config::ConfigWriter`] (kommentarerhaltend, atomar, mit Backup) —
//! derselbe Persistenzweg, den `/permissions` und `/model switch` nutzen.
//!
//! `proposals`, `review <id>`, `accept <id>` und `reject <id> [grund]` sind
//! die Operator-Fläche der Skill-Vorschläge (Welle 4): sie lesen und
//! entscheiden die unter `<profil>/skills/.proposals/` abgelegten Vorschläge
//! aus `skills.propose`
//! ([`harw_registry_defaults::skill_proposal_tools::SkillProposalStore`]).
//! `accept` ist die Nutzerbestätigung selbst und schreibt
//! `<profil>/skills/<name>/`; einem Agenten zugewiesen wird der Skill dadurch
//! nicht.
//!
//! # Root-Space
//! Ohne injizierten Dienst lesen und schreiben `activate`/`deactivate` und die
//! Vorschlags-Aktionen ausschließlich im an die Sitzung gebundenen Root-Space
//! (`Arc<harw_home::ResolvedHomeContext>` in der `ServiceMap`, von der
//! Laufzeit auf jeder Oberfläche eingetragen); `<profil>` ist dessen
//! Profilverzeichnis. Fehlt die Bindung, antworten sie mit
//! [`OpError::NotAvailable`] — es gibt keinen Rückfall auf `HARW_HOME` oder
//! `~/.harw`.
//!
//! # Verantwortungsbereich
//! Implementiert die `skills`-Operation ausschließlich als `/skills`-Command
//! (`channel_reduced`); sie wird nicht als Model-Tool exponiert, weil das
//! Modell seinen eigenen Skill-Zuschnitt nicht selbst umschalten darf.
//!
//! # Schlüsseltypen
//! - [`SkillsArgs`] — typisierte Felder für Action (`list` | `activate` |
//!   `deactivate` | `show`) und optionalen Skill-Namen (`target`).
//! - [`SkillsOperation`] — generiertes Unit-Struct (via `#[operation]`-Makro)
//! - [`SkillStatePersistence`] — austauschbarer Dienst für das Umschalten
//!   des `enabled`-Felds; per `Arc<dyn SkillStatePersistence>` in der
//!   `ServiceMap` injizierbar.
//! - [`LayeredSkillStatePersistence`] — Standard-Implementierung über die
//!   vertrauten Config-Layer des gebundenen Root-Space
//!   ([`LayeredSkillStatePersistence::from_home_context`]).
//! - [`SkillStateOutcome`] — Ergebnis einer Umschaltung (Manifest-Pfad,
//!   ob tatsächlich geschrieben wurde).
//!
//! # Wirksamkeit
//! Die `ResolvedConfig` einer laufenden Sitzung ist ein unveränderlicher
//! Snapshot. Ein Umschalten wirkt daher ab dem nächsten Config-Laden
//! (nächste Sitzung); `/skills list` zeigt bis dahin den Snapshot-Stand.
//!
//! # Manifest-Auflösung
//! Wie `harw_config::discover_config` gewinnt der **letzte** vertraute Layer,
//! und innerhalb eines Layers das nach Verzeichnisnamen letzte Manifest, dessen
//! `name`-Feld passt. Genau diese Datei wird geändert — also die, aus der die
//! geladene Konfiguration den Skill tatsächlich bezieht. Die Layer stammen aus
//! dem gebundenen Root-Space (Root, dessen Profil, vertrauter Repo-Layer);
//! nicht vertraute Repo-Layer liefert die Layer-Auflösung gar nicht erst.
//!
//! # Surface-Matrix
//! | Surface | Sichtbarkeit      |
//! |---------|-------------------|
//! | Command | `channel_reduced` |
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_ops::skills::SkillsOperation;
//! use harw_operations::Operation;
//!
//! let op = SkillsOperation;
//! assert_eq!(op.meta().name, "skills");
//! ```

use harw_config::{ResolvedConfig, SkillToml};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_registry_defaults::skill_proposal_tools::{
    CommitAuthority, LoadedSkillProposal, SkillProposalError, SkillProposalListing,
    SkillProposalStore,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Dateiname des Skill-Manifests innerhalb eines Skill-Verzeichnisses.
const SKILL_MANIFEST: &str = "skill.toml";

// ── Args ──────────────────────────────────────────────────────────────────────

/// Argumente für die `/skills`-Operation.
///
/// # Beschreibung
/// Trägt das Sub-Kommando (`action`) und einen optionalen Skill-Namen (`target`).
/// Die Felder werden durch das [`harw_macros::FromRawArgs`]-Derive direkt aus
/// den tokenisierten TUI-Rohargumenten befüllt — jedes Feld erhält genau das
/// Token an der entsprechenden Position (0-basiert).
///
/// - `None` oder `action = Some("list")` → Skills auflisten (Standard-Verhalten).
/// - `action = Some("show")` mit `target` → Details eines Skills.
/// - `action = Some("activate")`/`Some("deactivate")` mit `target` → `enabled`
///   im Skill-Manifest dauerhaft umschalten.
/// - `action = Some("proposals")` → offene und entschiedene Skill-Vorschläge.
/// - `action = Some("review")`/`Some("accept")` mit `target` (Vorschlags-ID) →
///   Vorschlag ansehen bzw. übernehmen; `Some("reject")` mit `target` und
///   optionalem Grund in `value` → verwerfen.
///
/// # Felder
/// - `action` (`Option<String>`): Token 0 — Sub-Kommando: `"list"` (Standard),
///   `"activate"`, `"deactivate"`, `"show"`. `None` wird wie `"list"` behandelt.
/// - `target` (`Option<String>`): Token 1 — Skill-Name. Pflicht für `activate`,
///   `deactivate` und `show`; bei `list` ignoriert.
/// - `value` (`Option<String>`): Token 2 — dritter Parameter (reserviert für
///   zukünftige Erweiterungen; aktuell ungenutzt).
///
/// # Deserialisierung
/// Das `#[operation]`-Makro deserialisiert `input.json_args` via `serde_json`
/// in diesen Typ. Bei Command-Aufrufen ist `json_args == Null`, weshalb
/// [`Default::default`] greift und `action = None` ergibt. Die Operation ist
/// ausschließlich über den `/skills`-Command erreichbar.
///
/// # Spec-Referenz
/// Spec-Abschnitt: `/skills` — Args.
///
/// # Beispiel
/// ```rust
/// use harw_ops::skills::SkillsArgs;
/// use harw_operations::FromRawArgs;
///
/// let list_args = SkillsArgs::default();
/// assert!(list_args.action.is_none());
///
/// // "/skills activate my-skill" → tokens = ["activate", "my-skill"]
/// let args = SkillsArgs::from_raw_args(&["activate".to_owned(), "my-skill".to_owned()]).unwrap();
/// assert_eq!(args.action.as_deref(), Some("activate"));
/// assert_eq!(args.target.as_deref(), Some("my-skill"));
/// ```
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct SkillsArgs {
    /// Sub-Kommando: `"list"` (Standard), `"activate"`, `"deactivate"`, `"show"`. Token 0.
    ///
    /// `None` wird wie `"list"` behandelt.
    #[serde(default)]
    #[raw(first)]
    pub action: Option<String>,
    /// Skill-Name (z. B. `"my-skill"`). Token 1. `None` wenn nicht angegeben.
    #[serde(default)]
    #[raw(nth = 1)]
    pub target: Option<String>,
    /// Dritter Parameter (reserviert). Token 2. `None` wenn nicht angegeben.
    #[serde(default)]
    #[raw(nth = 2)]
    pub value: Option<String>,
}

// ── Persistenz ────────────────────────────────────────────────────────────────

/// Ergebnis einer [`SkillStatePersistence::set_skill_enabled`]-Umschaltung.
///
/// # Felder
/// - `manifest` (`PathBuf`): Pfad des Skill-Manifests, das den Skill in der
///   geladenen Konfiguration definiert.
/// - `changed` (`bool`): `true`, wenn das Manifest geschrieben wurde; `false`,
///   wenn es den gewünschten Zustand bereits trug (keine Schreiboperation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillStateOutcome {
    /// Pfad des betroffenen `skill.toml`.
    pub manifest: PathBuf,
    /// Ob tatsächlich geschrieben wurde.
    pub changed: bool,
}

/// Austauschbarer Dienst, der den Aktivierungszustand eines Skills dauerhaft setzt.
///
/// # Beschreibung
/// Die `/skills`-Operation fragt zuerst `Arc<dyn SkillStatePersistence>` aus der
/// [`harw_operations::context::ServiceMap`] ab. Ohne registrierten Dienst baut
/// sie [`LayeredSkillStatePersistence::from_home_context`] über den an die
/// Sitzung gebundenen Root-Space (`Arc<harw_home::ResolvedHomeContext>`); ohne
/// Bindung ist die Umschaltung [`OpError::NotAvailable`], ein Rückfall auf
/// `HARW_HOME` findet nicht statt. Tests und Oberflächen mit eigener
/// Config-Wurzel injizieren eine eigene Implementierung.
///
/// # Nebenläufigkeit
/// `Send + Sync`; Implementierungen sichern parallele Schreibzugriffe auf
/// dieselbe Datei nicht zwingend ab (analog [`harw_config::ConfigWriter`]).
pub trait SkillStatePersistence: Send + Sync {
    /// Setzt `enabled` des Skills `name` dauerhaft auf `enabled`.
    ///
    /// # Argumente
    /// - `name` (`&str`): Skill-Name (Feld `name` im Manifest).
    /// - `enabled` (`bool`): gewünschter Zustand.
    ///
    /// # Rückgabe
    /// [`SkillStateOutcome`] mit Manifest-Pfad und Änderungsflag.
    ///
    /// # Fehler
    /// - [`OpError::InvalidArguments`]: kein Manifest mit diesem Namen gefunden.
    /// - [`OpError::Execution`]: Lese-, Parse- oder Schreibfehler.
    fn set_skill_enabled(&self, name: &str, enabled: bool) -> Result<SkillStateOutcome, OpError>;
}

/// Standard-[`SkillStatePersistence`] über eine Liste vertrauter Config-Layer.
///
/// # Beschreibung
/// Sucht `skills/*/skill.toml` in den Layern in umgekehrter Präzedenz (letzter
/// Layer zuerst, innerhalb eines Layers nach Verzeichnisnamen absteigend) und
/// schreibt das erste Manifest, dessen `name` passt — exakt das Manifest, das
/// `harw_config::discover_config` für diesen Namen zuletzt einliest und damit
/// wirksam macht. Geschrieben wird nur, wenn sich der Zustand ändert; dann über
/// [`harw_config::ConfigWriter`] (Kommentare bleiben erhalten, atomares
/// Schreiben, rotierende `skill.toml.bak.<n>`-Backups, Rechte `0600`, das
/// Skill-Verzeichnis wird dabei auf `0700` gesetzt).
///
/// # Nebenläufigkeit
/// Zustandslos bis auf die unveränderliche Layer-Liste; `Send + Sync`.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_ops::skills::{LayeredSkillStatePersistence, SkillStatePersistence};
///
/// let persistence = LayeredSkillStatePersistence::new(vec!["/home/user/.harw".into()]);
/// let outcome = persistence.set_skill_enabled("review", false)?;
/// println!("{}", outcome.manifest.display());
/// # Ok::<(), harw_operations::OpError>(())
/// ```
#[derive(Debug, Clone)]
pub struct LayeredSkillStatePersistence {
    layers: Vec<PathBuf>,
}

impl LayeredSkillStatePersistence {
    /// Erstellt die Persistenz über die gegebenen Layer (aufsteigende Präzedenz).
    ///
    /// # Argumente
    /// - `layers` (`Vec<PathBuf>`): Config-Layer in derselben Reihenfolge, wie
    ///   sie `harw_config::discover_config` erhält.
    #[must_use]
    pub fn new(layers: Vec<PathBuf>) -> Self {
        Self { layers }
    }

    /// Erstellt die Persistenz über die vertrauten Layer eines gebundenen
    /// Root-Space.
    ///
    /// # Beschreibung
    /// Layer in aufsteigender Präzedenz: Root-Space, dessen Profilverzeichnis
    /// (aus der Bindung, nicht aus `HARW_PROFILE`), dann der vertraute
    /// Repo-Layer, falls freigegeben. Das ist der Standardweg von `/skills`.
    ///
    /// # Argumente
    /// - `home` (`&harw_home::ResolvedHomeContext`): der an die Sitzung
    ///   gebundene Root-Space.
    ///
    /// # Fehler
    /// - [`OpError::Execution`]: Layer nicht auflösbar (Trust-Store unlesbar,
    ///   Projekt-Root nicht kanonisierbar, ungültiger Profilname).
    pub fn from_home_context(home: &harw_home::ResolvedHomeContext) -> Result<Self, OpError> {
        crate::config_util::bound_config_layers(home)
            .map(Self::new)
            .map_err(|error| OpError::Execution(format!("Config-Layer nicht auflösbar: {error}")))
    }

    /// Findet das wirksame Manifest des Skills `name` samt aktuellem `enabled`.
    ///
    /// # Rückgabe
    /// `Some((pfad, enabled))` oder `None`, wenn kein Layer den Skill definiert.
    ///
    /// # Fehler
    /// - [`OpError::Execution`]: Verzeichnis/Datei unlesbar oder Manifest
    ///   kein gültiges `SkillToml` (Discovery würde hier ebenfalls scheitern).
    fn find_manifest(&self, name: &str) -> Result<Option<(PathBuf, bool)>, OpError> {
        for base in self.layers.iter().rev() {
            let skills_dir = base.join("skills");
            if !skills_dir.is_dir() {
                continue;
            }
            let mut entries = std::fs::read_dir(&skills_dir)
                .map_err(|error| read_failed(&skills_dir, error))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| read_failed(&skills_dir, error))?;
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for entry in entries.iter().rev() {
                let entry_path = entry.path();
                let file_type = entry
                    .file_type()
                    .map_err(|error| read_failed(&entry_path, error))?;
                if !file_type.is_dir() {
                    continue;
                }
                let manifest = entry_path.join(SKILL_MANIFEST);
                if !manifest.exists() {
                    continue;
                }
                let content = std::fs::read_to_string(&manifest)
                    .map_err(|error| read_failed(&manifest, error))?;
                let skill: SkillToml = toml::from_str(&content).map_err(|error| {
                    OpError::Execution(format!(
                        "Skill-Manifest '{}' ist ungültig: {error}",
                        manifest.display()
                    ))
                })?;
                if skill.name == name {
                    return Ok(Some((manifest, skill.enabled)));
                }
            }
        }
        Ok(None)
    }
}

impl SkillStatePersistence for LayeredSkillStatePersistence {
    /// Schaltet `enabled` im wirksamen Manifest um (siehe Typ-Doku).
    ///
    /// # Fehler
    /// - [`OpError::InvalidArguments`]: kein Layer definiert den Skill.
    /// - [`OpError::Execution`]: Lese-, Parse- oder Schreibfehler.
    fn set_skill_enabled(&self, name: &str, enabled: bool) -> Result<SkillStateOutcome, OpError> {
        let Some((manifest, current)) = self.find_manifest(name)? else {
            return Err(OpError::InvalidArguments(format!(
                "no manifest for skill '{name}' in any trusted config layer"
            )));
        };
        if current == enabled {
            return Ok(SkillStateOutcome {
                manifest,
                changed: false,
            });
        }
        let write_failed = |error: harw_config::ConfigError| {
            OpError::Execution(format!(
                "Skill-Manifest '{}' konnte nicht geschrieben werden: {error}",
                manifest.display()
            ))
        };
        let mut writer = harw_config::ConfigWriter::open(&manifest).map_err(write_failed)?;
        writer
            .set_value("enabled", toml_edit::value(enabled))
            .map_err(write_failed)?;
        writer.save().map_err(write_failed)?;
        Ok(SkillStateOutcome {
            manifest,
            changed: true,
        })
    }
}

/// Baut den einheitlichen Lesefehler für Verzeichnis- und Dateizugriffe.
fn read_failed(path: &Path, error: std::io::Error) -> OpError {
    OpError::Execution(format!("'{}' ist nicht lesbar: {error}", path.display()))
}

/// Liefert den injizierten Persistenzdienst oder die Layer-Standardimplementierung
/// über den an die Sitzung gebundenen Root-Space.
///
/// # Fehler
/// - [`OpError::NotAvailable`]: kein Dienst registriert und kein Root-Space
///   gebunden (kein Rückfall auf `HARW_HOME`).
/// - [`OpError::Execution`]: Layer des gebundenen Root-Space nicht auflösbar.
fn skill_state_persistence(ctx: &OpContext) -> Result<Arc<dyn SkillStatePersistence>, OpError> {
    if let Some(persistence) = ctx.service::<Arc<dyn SkillStatePersistence>>() {
        return Ok(Arc::clone(persistence));
    }
    let home = crate::config_util::bound_home(ctx)?;
    Ok(Arc::new(LayeredSkillStatePersistence::from_home_context(home)?)
        as Arc<dyn SkillStatePersistence>)
}

// ── Skill-Vorschläge ──────────────────────────────────────────────────────────

/// Liefert die injizierte Vorschlagsablage oder die des Profils im an die
/// Sitzung gebundenen Root-Space (`<profilverzeichnis>/skills`).
///
/// # Fehler
/// - [`OpError::NotAvailable`]: kein Dienst registriert und kein Root-Space
///   gebunden (kein Rückfall auf `HARW_HOME`).
fn skill_proposal_store(ctx: &OpContext) -> Result<Arc<SkillProposalStore>, OpError> {
    if let Some(store) = ctx.service::<Arc<SkillProposalStore>>() {
        return Ok(Arc::clone(store));
    }
    let home = crate::config_util::bound_home(ctx)?;
    Ok(Arc::new(SkillProposalStore::new(
        home.profile_dir.join("skills"),
    )))
}

/// Bildet einen Ablagefehler auf den passenden Operationsfehler ab.
fn proposal_error(error: SkillProposalError) -> OpError {
    match error {
        SkillProposalError::Io(_) => OpError::Execution(error.to_string()),
        SkillProposalError::Invalid(_)
        | SkillProposalError::InvalidId(_)
        | SkillProposalError::NotFound(_)
        | SkillProposalError::Expired(_)
        | SkillProposalError::Conflict(_)
        | SkillProposalError::RequiresUserConfirmation { .. } => {
            OpError::InvalidArguments(error.to_string())
        }
    }
}

/// Verlangt die Vorschlags-ID aus Token 1.
fn required_proposal_id<'a>(action: &str, args: &'a SkillsArgs) -> Result<&'a str, OpError> {
    args.target.as_deref().ok_or_else(|| {
        OpError::InvalidArguments(format!("action '{action}' requires a proposal id"))
    })
}

/// Rendert die Vorschlagsliste für `/skills proposals`.
fn render_proposal_list(listings: &[SkillProposalListing]) -> String {
    if listings.is_empty() {
        return "Keine Skill-Vorschläge.".to_owned();
    }
    let mut lines = vec![format!("{} Skill-Vorschlag/-Vorschläge:", listings.len())];
    for listing in listings {
        match listing {
            SkillProposalListing::Proposal { meta, expired } => {
                let mut line = format!(
                    "- {} [{}] {} · Prüfstufe {} · Evals {}",
                    meta.proposal_id,
                    meta.status.as_str(),
                    meta.name,
                    meta.review_level.as_str(),
                    meta.eval_status()
                );
                if !meta.capability_delta.is_empty() {
                    line.push_str(&format!(
                        " · Delta Werkzeuge [{}] MCPs [{}]",
                        meta.capability_delta.added_tools.join(", "),
                        meta.capability_delta.added_mcps.join(", ")
                    ));
                }
                if *expired {
                    line.push_str(" · abgelaufen");
                }
                lines.push(line);
            }
            SkillProposalListing::Broken { proposal_id, error } => {
                lines.push(format!("- {proposal_id} (defekt: {error})"));
            }
        }
    }
    lines.join("\n")
}

/// Kürzt `text` auf seine erste Zeile mit höchstens `max` Zeichen.
fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() > max {
        let mut cut: String = line.chars().take(max).collect();
        cut.push('…');
        cut
    } else {
        line.to_owned()
    }
}

/// Rendert einen Vorschlag für `/skills review <id>`.
fn render_proposal_review(loaded: &LoadedSkillProposal) -> String {
    let meta = &loaded.meta;
    let mut lines = vec![
        format!("Skill-Vorschlag {} — {}", meta.proposal_id, meta.name),
        format!(
            "Status: {} · läuft ab {}{}",
            meta.status.as_str(),
            meta.expires_at,
            if loaded.expired { " (ABGELAUFEN)" } else { "" }
        ),
        format!(
            "Prüfstufe: {} · Urheber: {} · ersetzt bestehenden Skill: {}",
            meta.review_level.as_str(),
            meta.author_role.as_deref().unwrap_or("unbekannt"),
            if meta.replaces_existing { "ja" } else { "nein" }
        ),
        format!("Beschreibung: {}", meta.description),
    ];
    if meta.capability_delta.is_empty() {
        lines.push("Delta gegenüber Urheber: keines".to_owned());
    } else {
        lines.push(format!(
            "Delta gegenüber Urheber: Werkzeuge [{}], MCPs [{}]",
            meta.capability_delta.added_tools.join(", "),
            meta.capability_delta.added_mcps.join(", ")
        ));
    }
    for warning in &meta.warnings {
        lines.push(format!("Hinweis: {warning}"));
    }
    if let Some(reason) = &meta.reason {
        lines.push(format!("Ablehnungsgrund: {reason}"));
    }
    lines.push("--- skill.toml ---".to_owned());
    lines.push(loaded.skill_toml.trim_end().to_owned());
    if meta.replaces_existing {
        lines.push(format!(
            "--- instructions.md ({} Bytes), Diff gegen Bestand ---",
            loaded.instructions.len()
        ));
        lines.push(meta.instructions_diff.trim_end().to_owned());
    } else {
        // Ohne Bestand ist der Diff nur „new file" — der Operator muss den
        // vollständigen Text sehen, den er übernimmt.
        lines.push(format!(
            "--- instructions.md ({} Bytes), neu ---",
            loaded.instructions.len()
        ));
        lines.push(loaded.instructions.trim_end().to_owned());
    }
    if meta.evals.is_empty() {
        lines.push("Evals: keine".to_owned());
    } else {
        lines.push(format!(
            "Evals ({}, Status {}):",
            meta.evals.len(),
            meta.eval_status()
        ));
        for case in &meta.evals {
            lines.push(format!(
                "- {}: {} ({} Assertion(s))",
                case.id,
                first_line(&case.prompt, 80),
                case.assertions.len()
            ));
        }
    }
    if let Some(benchmark) = &meta.benchmark {
        let delta = benchmark.delta();
        lines.push(format!(
            "Benchmark (Iteration {}, {} Lauf/Läufe je Konfiguration, Baseline {:?}):",
            benchmark.iteration, benchmark.runs_per_configuration, benchmark.baseline
        ));
        let with = &benchmark.with_skill;
        let base = &benchmark.baseline_stats;
        lines.push(format!(
            "  Passrate {:.2}±{:.2} vs {:.2}±{:.2} (Δ {:+.2})",
            with.pass_rate.mean,
            with.pass_rate.stddev,
            base.pass_rate.mean,
            base.pass_rate.stddev,
            delta.pass_rate
        ));
        lines.push(format!(
            "  Dauer s  {:.1}±{:.1} vs {:.1}±{:.1} (Δ {:+.1})",
            with.duration_seconds.mean,
            with.duration_seconds.stddev,
            base.duration_seconds.mean,
            base.duration_seconds.stddev,
            delta.duration_seconds
        ));
        lines.push(format!(
            "  Token    {:.0}±{:.0} vs {:.0}±{:.0} (Δ {:+.0})",
            with.total_tokens.mean,
            with.total_tokens.stddev,
            base.total_tokens.mean,
            base.total_tokens.stddev,
            delta.total_tokens
        ));
        if !benchmark.flaky_assertions.is_empty() {
            lines.push(format!(
                "  schwankend: {}",
                benchmark.flaky_assertions.join("; ")
            ));
        }
        if !benchmark.non_discriminating_assertions.is_empty() {
            lines.push(format!(
                "  nicht unterscheidend: {}",
                benchmark.non_discriminating_assertions.join("; ")
            ));
        }
        for note in &benchmark.notes {
            lines.push(format!("  Notiz: {note}"));
        }
    }
    lines.join("\n")
}

/// Führt die Vorschlags-Aktionen aus (`proposals`, `review`, `accept`,
/// `reject`). Braucht keinen Config-Service, aber eine injizierte Ablage oder
/// einen gebundenen Root-Space.
///
/// # Fehler
/// - [`OpError::NotAvailable`]: keine Ablage injiziert und kein Root-Space
///   gebunden.
/// - [`OpError::InvalidArguments`]: fehlende/ungültige ID, unbekannter,
///   abgelaufener oder nicht offener Vorschlag, ungültiger Kandidat.
/// - [`OpError::Execution`]: Lese-/Schreibfehler der Ablage.
fn run_proposal_action(
    ctx: &OpContext,
    action: &str,
    args: &SkillsArgs,
) -> Result<OpOutput, OpError> {
    let store = skill_proposal_store(ctx)?;
    match action {
        "proposals" => {
            let listings = store.list().map_err(proposal_error)?;
            Ok(OpOutput::from(render_proposal_list(&listings)))
        }
        "review" => {
            let id = required_proposal_id(action, args)?;
            let loaded = store.load(id).map_err(proposal_error)?;
            Ok(OpOutput::from(render_proposal_review(&loaded)))
        }
        "accept" => {
            let id = required_proposal_id(action, args)?;
            let outcome = store
                .commit(id, CommitAuthority::Operator)
                .map_err(proposal_error)?;
            let mut text = format!(
                "Skill '{}' übernommen nach {}. Er ist keinem Agenten zugewiesen; wirksam ab \
                 dem nächsten Config-Laden, sobald ein Agent ihn unter `skills` führt.",
                outcome.meta.name,
                outcome.skill_dir.display()
            );
            if let Some(error) = outcome.status_update_error {
                text.push_str(&format!("\nWarnung: {error}"));
            }
            Ok(OpOutput::from(text))
        }
        "reject" => {
            let id = required_proposal_id(action, args)?;
            let reason = args
                .value
                .as_deref()
                .filter(|reason| !reason.trim().is_empty())
                .unwrap_or("vom Operator abgelehnt");
            let meta = store.reject(id, reason).map_err(proposal_error)?;
            Ok(OpOutput::from(format!(
                "Skill-Vorschlag {} ({}) abgelehnt: {reason}",
                meta.proposal_id, meta.name
            )))
        }
        unknown => Err(OpError::InvalidArguments(format!(
            "unknown /skills action '{unknown}'"
        ))),
    }
}

// ── Operation ─────────────────────────────────────────────────────────────────

/// Liest den Skill-Katalog und schaltet Skills dauerhaft an oder ab.
///
/// # Beschreibung
/// - `list` (Standard): alle konfigurierten Skills mit Status.
/// - `show <name>`: Details eines Skills.
/// - `activate <name>` / `deactivate <name>`: setzt `enabled` im wirksamen
///   Skill-Manifest über [`SkillStatePersistence`]; wirksam ab dem nächsten
///   Config-Laden.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext; `list`/`show`/`activate`/
///   `deactivate` benötigen `Arc<ResolvedConfig>` (oder die Live-Config).
///   `activate`/`deactivate` nutzen einen injizierten
///   `Arc<dyn SkillStatePersistence>`, sonst den gebundenen Root-Space
///   (`Arc<harw_home::ResolvedHomeContext>`); die Vorschlags-Aktionen ebenso
///   eine injizierte `Arc<SkillProposalStore>`, sonst das Profil des
///   gebundenen Root-Space.
/// - `args` (`SkillsArgs`): Typisierte Sub-Kommando-Argumente.
///
/// # Rückgabe
/// - `Ok(OpOutput)`: menschenlesbare Ausgabe.
///
/// # Fehler
/// - [`OpError::NotAvailable`]: kein `Arc<ResolvedConfig>`-Service (statische
///   Meldung ohne Echo der Argumente); bei `activate`/`deactivate` und den
///   Vorschlags-Aktionen außerdem, wenn weder ein Dienst injiziert noch ein
///   Root-Space gebunden ist (kein Rückfall auf `HARW_HOME`).
/// - [`OpError::InvalidArguments`]: fehlender/unbekannter Skill-Name oder
///   unbekanntes Sub-Kommando.
/// - [`OpError::Execution`]: Persistenzfehler bei `activate`/`deactivate`
///   (auch: Layer des gebundenen Root-Space nicht auflösbar) oder
///   Ablagefehler der Vorschlags-Aktionen.
///
/// # Nebenläufigkeit
/// Zustandslos; Schreibzugriffe sind nicht gegen parallele Aufrufe auf
/// dasselbe Manifest gesperrt.
///
/// # Spec-Referenz
/// Spec-Abschnitt: `/skills` — Body, Command-Surface, Soft-Refuse.
///
/// # Beispiel
/// ```rust,no_run
/// // Aufruf erfolgt über Operation::run() — direkte Verwendung nur im Test-Kontext.
/// ```
#[operation(
    name = "skills",
    summary = "Skill-Katalog (list/show/activate/deactivate) und Skill-Vorschläge (proposals/review/accept/reject).",
    domain = "catalog_config",
    permission = "operator",
    command(
        path = "/skills",
        visibility = "channel_reduced",
        busy_subcommands = "-=immediate, list=immediate, show=immediate"
    )
)]
async fn skills(ctx: &OpContext, args: SkillsArgs) -> Result<OpOutput, OpError> {
    if let Some(action @ ("proposals" | "review" | "accept" | "reject")) = args.action.as_deref() {
        return run_proposal_action(ctx, action, &args);
    }
    // Live-Stand zuerst, damit `list`/`show` einen eben umgeschalteten Skill
    // sofort mit dem neuen Zustand zeigen.
    let config: Arc<ResolvedConfig> = match (
        ctx.service::<crate::live_config::SharedLiveConfig>(),
        ctx.service::<Arc<ResolvedConfig>>(),
    ) {
        (Some(live), _) => live.current(),
        (None, Some(config)) => Arc::clone(config),
        (None, None) => {
            return Err(OpError::NotAvailable(
                "skill catalog integration is not available".to_owned(),
            ));
        }
    };

    match args.action.as_deref().unwrap_or("list") {
        "list" => {
            if config.skills.is_empty() {
                return Ok(OpOutput::from("Keine Skills konfiguriert.".to_owned()));
            }
            let mut names: Vec<&String> = config.skills.keys().collect();
            names.sort();
            let mut lines = vec![format!("{} konfigurierte(r) Skill(s):", names.len())];
            for name in names {
                let skill = &config.skills[name];
                let status = if skill.enabled { "enabled" } else { "disabled" };
                lines.push(format!("- {name} ({status}): {}", skill.description));
            }
            Ok(OpOutput::from(lines.join("\n")))
        }
        "show" => {
            let Some(target) = args.target.as_deref() else {
                return Err(OpError::InvalidArguments(
                    "action 'show' requires a skill name".to_owned(),
                ));
            };
            let Some(skill) = config.skills.get(target) else {
                return Err(OpError::InvalidArguments(format!(
                    "unknown skill '{target}'"
                )));
            };
            Ok(OpOutput::from(format!(
                "{} (enabled={}): {}\ntools: {}\nmcps: {}",
                skill.name,
                skill.enabled,
                skill.description,
                skill.tools.join(", "),
                skill.mcps.join(", ")
            )))
        }
        action @ ("activate" | "deactivate") => {
            let enabled = action == "activate";
            let Some(target) = args.target.as_deref() else {
                return Err(OpError::InvalidArguments(format!(
                    "action '{action}' requires a skill name"
                )));
            };
            if !config.skills.contains_key(target) {
                return Err(OpError::InvalidArguments(format!(
                    "unknown skill '{target}'"
                )));
            }
            let outcome = skill_state_persistence(ctx)?.set_skill_enabled(target, enabled)?;
            crate::live_config::mirror(ctx, |live| {
                if let Some(skill) = live.skills.get_mut(target) {
                    skill.enabled = enabled;
                }
            });
            let state = if enabled { "aktiviert" } else { "deaktiviert" };
            let text = if outcome.changed {
                format!(
                    "Skill '{target}' {state} ({}). Wirksam ab dem nächsten Config-Laden (nächste Sitzung).",
                    outcome.manifest.display()
                )
            } else {
                format!(
                    "Skill '{target}' ist bereits {state} ({}).",
                    outcome.manifest.display()
                )
            };
            Ok(OpOutput::from(text))
        }
        unknown => Err(OpError::InvalidArguments(format!(
            "unknown /skills action '{unknown}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{SkillsArgs, SkillsOperation};
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_operations::operation::{CommandVisibility, Surface};
    use harw_operations::{FromRawArgs, OpContext, OpError, Operation, context::ServiceMap};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_context() -> TestResult<(OpContext, std::path::PathBuf)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-skills-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("ws")).map_err(ctx("create test workspace"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: std::path::PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("build workspace registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("resolve workspace binding"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        Ok((
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new()),
            root,
        ))
    }

    #[test]
    fn test_skills_args_from_raw_args_sets_action() -> TestResult {
        let args = SkillsArgs::from_raw_args(&toks(&["list"]))
            .map_err(ctx("SkillsArgs::from_raw_args"))?;
        assert_eq!(args.action.as_deref(), Some("list"));
        assert!(args.target.is_none());
        Ok(())
    }

    #[test]
    fn test_skills_args_from_raw_args_activate_preserves_target() -> TestResult {
        let args = SkillsArgs::from_raw_args(&toks(&["activate", "my-skill"]))
            .map_err(ctx("SkillsArgs::from_raw_args"))?;
        assert_eq!(args.action.as_deref(), Some("activate"));
        assert_eq!(args.target.as_deref(), Some("my-skill"));
        assert!(args.value.is_none());
        Ok(())
    }

    #[test]
    fn test_skills_args_from_raw_args_empty_tokens_sets_action_none() -> TestResult {
        let args =
            SkillsArgs::from_raw_args(&toks(&[])).map_err(ctx("SkillsArgs::from_raw_args"))?;
        assert!(args.action.is_none());
        assert!(args.target.is_none());
        Ok(())
    }

    #[test]
    fn skills_operation_is_command_only() {
        let surfaces = &SkillsOperation.meta().surfaces;

        assert!(
            !surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. }))
        );
        assert!(surfaces.iter().any(|surface| {
            matches!(
                surface,
                Surface::Command {
                    path: "/skills",
                    visibility: CommandVisibility::ChannelReduced,
                }
            )
        }));
    }

    #[tokio::test]
    async fn skills_default_and_list_return_not_available() -> TestResult {
        let (ctx, root) = test_context()?;
        let expected = "skill catalog integration is not available";
        assert!(matches!(
            super::skills(&ctx, SkillsArgs::default()).await,
            Err(OpError::NotAvailable(message)) if message == expected
        ));
        assert!(matches!(
            super::skills(&ctx, SkillsArgs { action: Some("list".to_owned()), target: None, value: None }).await,
            Err(OpError::NotAvailable(message)) if message == expected
        ));
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;
        Ok(())
    }

    #[tokio::test]
    async fn skills_list_with_config_service_reports_configured_skills() -> TestResult {
        use harw_config::SkillToml;
        use harw_operations::context::ServiceMap;
        use std::sync::Arc;

        let (mut ctx, root) = test_context()?;
        let mut config = harw_config::ResolvedConfig::default();
        config.skills.insert(
            "review".to_owned(),
            SkillToml {
                name: "review".to_owned(),
                enabled: true,
                description: "code review".to_owned(),
                instructions_file: None,
                tools: vec![],
                mcps: vec![],
            },
        );
        let mut services = ServiceMap::new();
        services.insert(Arc::new(config));
        ctx = OpContext::new(
            ctx.session_id().clone(),
            ctx.turn_id().clone(),
            ctx.sandbox().clone(),
            services,
        );

        let result = super::skills(&ctx, SkillsArgs::default()).await;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        match result {
            Ok(output) => assert!(output.text.contains("review")),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Ok listing, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn skills_target_bearing_action_returns_not_available_without_input_leakage() -> TestResult
    {
        let (ctx, root) = test_context()?;
        let action = "activate";
        let target = "secret-skill-target";
        let value = "secret-value";
        let result = super::skills(
            &ctx,
            SkillsArgs {
                action: Some(action.to_owned()),
                target: Some(target.to_owned()),
                value: Some(value.to_owned()),
            },
        )
        .await;
        std::fs::remove_dir_all(root).map_err(crate::test_support::ctx("remove test workspace"))?;

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert_eq!(message, "skill catalog integration is not available");
                assert!(!message.contains(action));
                assert!(!message.contains(target));
                assert!(!message.contains(value));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    // ── activate / deactivate ────────────────────────────────────────────────

    use super::{LayeredSkillStatePersistence, SkillStateOutcome, SkillStatePersistence};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    /// Aufzeichnende Persistenz: merkt sich jeden Aufruf, schreibt nichts.
    #[derive(Default)]
    struct RecordingPersistence {
        calls: Mutex<Vec<(String, bool)>>,
    }

    impl SkillStatePersistence for RecordingPersistence {
        fn set_skill_enabled(
            &self,
            name: &str,
            enabled: bool,
        ) -> Result<SkillStateOutcome, OpError> {
            self.calls
                .lock()
                .map_err(|_| OpError::Execution("poisoned".to_owned()))?
                .push((name.to_owned(), enabled));
            Ok(SkillStateOutcome {
                manifest: PathBuf::from("/recorded/skills/review/skill.toml"),
                changed: true,
            })
        }
    }

    fn review_config() -> harw_config::ResolvedConfig {
        let mut config = harw_config::ResolvedConfig::default();
        config.skills.insert(
            "review".to_owned(),
            harw_config::SkillToml {
                name: "review".to_owned(),
                enabled: true,
                description: "code review".to_owned(),
                instructions_file: None,
                tools: vec![],
                mcps: vec![],
            },
        );
        config
    }

    /// Baut einen Kontext mit Config-Service und optionaler Persistenz.
    fn context_with(
        persistence: Option<Arc<dyn SkillStatePersistence>>,
    ) -> TestResult<(OpContext, PathBuf)> {
        let (base, root) = test_context()?;
        let mut services = ServiceMap::new();
        services.insert(Arc::new(review_config()));
        if let Some(persistence) = persistence {
            services.insert(persistence);
        }
        Ok((
            OpContext::new(
                base.session_id().clone(),
                base.turn_id().clone(),
                base.sandbox().clone(),
                services,
            ),
            root,
        ))
    }

    /// Live-Schnappschuss: nach `deactivate` zeigt `list` im selben Kontext
    /// den Skill sofort als `disabled`.
    #[tokio::test]
    async fn deactivate_is_visible_in_the_following_list() -> TestResult {
        let recorder = Arc::new(RecordingPersistence::default());
        let persistence: Arc<dyn SkillStatePersistence> = recorder.clone();
        let (base, root) = test_context()?;
        let live: crate::live_config::SharedLiveConfig = Arc::new(
            crate::live_config::LiveConfig::new(Arc::new(review_config())),
        );
        let mut services = ServiceMap::new();
        services.insert(Arc::new(review_config()));
        services.insert(persistence);
        services.insert(Arc::clone(&live));
        let op_ctx = OpContext::new(
            base.session_id().clone(),
            base.turn_id().clone(),
            base.sandbox().clone(),
            services,
        );

        let switched = super::skills(&op_ctx, args("deactivate", Some("review"))).await;
        let listed = super::skills(&op_ctx, args("list", None)).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        assert!(switched.is_ok(), "{switched:?}");
        let text = listed
            .map_err(|error| TestError::Unexpected(format!("/skills list: {error}")))?
            .text;
        assert!(text.contains("review (disabled)"), "{text}");
        assert!(
            live.current()
                .skills
                .get("review")
                .is_some_and(|skill| !skill.enabled)
        );
        Ok(())
    }

    fn args(action: &str, target: Option<&str>) -> SkillsArgs {
        SkillsArgs {
            action: Some(action.to_owned()),
            target: target.map(str::to_owned),
            value: None,
        }
    }

    fn write_manifest(layer: &Path, dir: &str, body: &str) -> TestResult<PathBuf> {
        let skill_dir = layer.join("skills").join(dir);
        std::fs::create_dir_all(&skill_dir).map_err(ctx("create skill dir"))?;
        let manifest = skill_dir.join("skill.toml");
        std::fs::write(&manifest, body).map_err(ctx("write skill manifest"))?;
        Ok(manifest)
    }

    fn read_enabled(manifest: &Path) -> TestResult<bool> {
        let content = std::fs::read_to_string(manifest).map_err(ctx("read manifest"))?;
        let skill: harw_config::SkillToml =
            toml::from_str(&content).map_err(ctx("parse manifest"))?;
        Ok(skill.enabled)
    }

    #[tokio::test]
    async fn skills_activate_without_target_is_invalid_arguments() -> TestResult {
        let recorder = Arc::new(RecordingPersistence::default());
        let (ctx_, root) =
            context_with(Some(Arc::clone(&recorder) as Arc<dyn SkillStatePersistence>))?;
        let result = super::skills(&ctx_, args("activate", None)).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;
        assert!(matches!(result, Err(OpError::InvalidArguments(_))));
        let calls = recorder.calls.lock().map_err(ctx("lock calls"))?;
        assert!(calls.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn skills_deactivate_unknown_skill_is_invalid_arguments() -> TestResult {
        let recorder = Arc::new(RecordingPersistence::default());
        let (ctx_, root) =
            context_with(Some(Arc::clone(&recorder) as Arc<dyn SkillStatePersistence>))?;
        let result = super::skills(&ctx_, args("deactivate", Some("missing"))).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;
        assert!(matches!(
            result,
            Err(OpError::InvalidArguments(message)) if message.contains("unknown skill")
        ));
        let calls = recorder.calls.lock().map_err(ctx("lock calls"))?;
        assert!(calls.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn skills_activate_and_deactivate_delegate_to_injected_persistence() -> TestResult {
        let recorder = Arc::new(RecordingPersistence::default());
        let (ctx_, root) =
            context_with(Some(Arc::clone(&recorder) as Arc<dyn SkillStatePersistence>))?;
        let deactivated = super::skills(&ctx_, args("deactivate", Some("review"))).await;
        let activated = super::skills(&ctx_, args("activate", Some("review"))).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        let deactivated = deactivated.map_err(ctx("deactivate"))?;
        assert!(deactivated.text.contains("'review' deaktiviert"));
        let activated = activated.map_err(ctx("activate"))?;
        assert!(activated.text.contains("'review' aktiviert"));
        let calls = recorder.calls.lock().map_err(ctx("lock calls"))?;
        assert_eq!(
            *calls,
            vec![("review".to_owned(), false), ("review".to_owned(), true)]
        );
        Ok(())
    }

    #[test]
    fn layered_persistence_writes_highest_precedence_manifest_and_keeps_comments() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let global = temp.path().join("global");
        let profile = temp.path().join("profile");
        let global_manifest = write_manifest(
            &global,
            "review",
            "name = \"review\"\ndescription = \"global\"\n",
        )?;
        // Verzeichnisname weicht vom Skill-Namen ab: maßgeblich ist das `name`-Feld.
        let profile_manifest = write_manifest(
            &profile,
            "review-override",
            "# Profil-Override\nname = \"review\"\nenabled = true\ndescription = \"profile\"\n",
        )?;
        write_manifest(&profile, "other", "name = \"other\"\n")?;

        let persistence = LayeredSkillStatePersistence::new(vec![global.clone(), profile]);
        let outcome = persistence
            .set_skill_enabled("review", false)
            .map_err(ctx("deactivate review"))?;
        assert_eq!(outcome.manifest, profile_manifest);
        assert!(outcome.changed);
        assert!(!read_enabled(&profile_manifest)?);
        assert!(read_enabled(&global_manifest)?);
        let content =
            std::fs::read_to_string(&profile_manifest).map_err(ctx("read profile manifest"))?;
        assert!(content.contains("# Profil-Override"));
        assert!(content.contains("description = \"profile\""));

        // Zweiter Aufruf mit demselben Zustand schreibt nicht.
        let again = persistence
            .set_skill_enabled("review", false)
            .map_err(ctx("deactivate review again"))?;
        assert!(!again.changed);

        // Fehlendes `enabled` gilt als `true` (Serde-Default): Aktivieren ist ein
        // No-op, Deaktivieren schreibt.
        let global_only = LayeredSkillStatePersistence::new(vec![global]);
        let reactivated = global_only
            .set_skill_enabled("review", true)
            .map_err(ctx("activate global review"))?;
        assert!(!reactivated.changed);
        let deactivated = global_only
            .set_skill_enabled("review", false)
            .map_err(ctx("deactivate global review"))?;
        assert!(deactivated.changed);
        assert!(!read_enabled(&global_manifest)?);
        Ok(())
    }

    #[test]
    fn layered_persistence_rejects_skill_without_manifest() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let layer = temp.path().join("layer");
        write_manifest(&layer, "other", "name = \"other\"\n")?;
        let persistence =
            LayeredSkillStatePersistence::new(vec![layer, temp.path().join("absent")]);
        assert!(matches!(
            persistence.set_skill_enabled("review", false),
            Err(OpError::InvalidArguments(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn skills_deactivate_end_to_end_with_layered_persistence() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let layer = temp.path().join("layer");
        let manifest = write_manifest(&layer, "review", "name = \"review\"\nenabled = true\n")?;
        let persistence: Arc<dyn SkillStatePersistence> =
            Arc::new(LayeredSkillStatePersistence::new(vec![layer]));
        let (ctx_, root) = context_with(Some(persistence))?;

        let first = super::skills(&ctx_, args("deactivate", Some("review"))).await;
        let second = super::skills(&ctx_, args("deactivate", Some("review"))).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        let first = first.map_err(ctx("first deactivate"))?;
        assert!(first.text.contains("deaktiviert"));
        assert!(first.text.contains("nächsten Config-Laden"));
        let second = second.map_err(ctx("second deactivate"))?;
        assert!(second.text.contains("bereits deaktiviert"));
        assert!(!read_enabled(&manifest)?);
        Ok(())
    }

    /// Baut einen Kontext mit genau den übergebenen Diensten.
    fn context_with_services(services: ServiceMap) -> TestResult<(OpContext, PathBuf)> {
        let (base, root) = test_context()?;
        Ok((
            OpContext::new(
                base.session_id().clone(),
                base.turn_id().clone(),
                base.sandbox().clone(),
                services,
            ),
            root,
        ))
    }

    /// Ohne injizierte Persistenz schreibt `deactivate` in die Layer des
    /// gebundenen Root-Space — nie über `HARW_HOME`.
    #[tokio::test]
    async fn skills_deactivate_writes_the_manifest_of_the_bound_home() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        // Root-Layer statt Profil-Layer: unabhängig von `HARW_PROFILE`.
        let manifest = write_manifest(
            temp.path(),
            "review",
            "name = \"review\"\nenabled = true\n",
        )?;
        let mut services = ServiceMap::new();
        services.insert(Arc::new(review_config()));
        services.insert(crate::config_util::test_home_context(temp.path())?);
        let (op_ctx, root) = context_with_services(services)?;

        let result = super::skills(&op_ctx, args("deactivate", Some("review"))).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        let output = result.map_err(ctx("deactivate"))?;
        assert!(output.text.contains("'review' deaktiviert"), "{}", output.text);
        assert!(!read_enabled(&manifest)?);
        Ok(())
    }

    /// Ohne Persistenzdienst und ohne gebundenen Root-Space ist `deactivate`
    /// nicht verfügbar, statt auf den Prozess-Root-Space auszuweichen.
    #[tokio::test]
    async fn skills_deactivate_without_persistence_or_bound_home_is_not_available() -> TestResult
    {
        let (op_ctx, root) = context_with(None)?;
        let result = super::skills(&op_ctx, args("deactivate", Some("review"))).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("Root-Space"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected NotAvailable, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    // ── Skill-Vorschläge ─────────────────────────────────────────────────────

    use harw_registry_defaults::skill_proposal_tools::{
        SkillAuthorCeiling, SkillCandidate, SkillProposalStore,
    };

    /// Kontext nur mit Vorschlagsablage — ohne Config-Service.
    fn proposal_context(store: Arc<SkillProposalStore>) -> TestResult<(OpContext, PathBuf)> {
        let (base, root) = test_context()?;
        let mut services = ServiceMap::new();
        services.insert(store);
        Ok((
            OpContext::new(
                base.session_id().clone(),
                base.turn_id().clone(),
                base.sandbox().clone(),
                services,
            ),
            root,
        ))
    }

    fn propose_review_skill(store: &SkillProposalStore) -> TestResult<String> {
        let meta = store
            .propose(
                &SkillCandidate {
                    skill_toml: "name = \"review\"\ndescription = \"Prüft Diffs gründlich \
                                 vor jedem Commit\"\ntools = [\"fs.read\"]\n"
                        .to_owned(),
                    instructions: "Lies zuerst die Tests.\n".to_owned(),
                    ..SkillCandidate::default()
                },
                &SkillAuthorCeiling {
                    role: harw_agent_dsl::roles::AgentRoleId::UserInterface,
                    tools: std::collections::BTreeSet::new(),
                    mcps: std::collections::BTreeSet::new(),
                },
            )
            .map_err(ctx("propose"))?;
        Ok(meta.proposal_id)
    }

    #[tokio::test]
    async fn skills_proposals_review_and_accept_without_config_service() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let skills_dir = temp.path().join("skills");
        let store = Arc::new(SkillProposalStore::new(skills_dir.clone()));
        let id = propose_review_skill(&store)?;
        let (ctx_, root) = proposal_context(Arc::clone(&store))?;

        let listed = super::skills(&ctx_, args("proposals", None)).await;
        let reviewed = super::skills(&ctx_, args("review", Some(id.as_str()))).await;
        let accepted = super::skills(&ctx_, args("accept", Some(id.as_str()))).await;
        let again = super::skills(&ctx_, args("accept", Some(id.as_str()))).await;
        let missing_id = super::skills(&ctx_, args("review", None)).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        let listed = listed.map_err(ctx("proposals"))?;
        assert!(listed.text.contains(&id));
        assert!(listed.text.contains("user_required"));
        assert!(listed.text.contains("Delta Werkzeuge [fs.read]"));
        let reviewed = reviewed.map_err(ctx("review"))?;
        assert!(reviewed.text.contains("Lies zuerst die Tests."));
        assert!(reviewed.text.contains("Evals: keine"));
        let accepted = accepted.map_err(ctx("accept"))?;
        assert!(accepted.text.contains("keinem Agenten zugewiesen"));
        assert!(skills_dir.join("review/skill.toml").is_file());
        assert!(matches!(again, Err(OpError::InvalidArguments(_))));
        assert!(matches!(missing_id, Err(OpError::InvalidArguments(_))));
        Ok(())
    }

    #[tokio::test]
    async fn skills_reject_records_reason_and_blocks_accept() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(SkillProposalStore::new(temp.path().join("skills")));
        let id = propose_review_skill(&store)?;
        let (ctx_, root) = proposal_context(Arc::clone(&store))?;

        let rejected = super::skills(
            &ctx_,
            SkillsArgs {
                action: Some("reject".to_owned()),
                target: Some(id.clone()),
                value: Some("doppelt".to_owned()),
            },
        )
        .await;
        let accepted = super::skills(&ctx_, args("accept", Some(id.as_str()))).await;
        let unknown = super::skills(&ctx_, args("review", Some("does-not-exist"))).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        let rejected = rejected.map_err(ctx("reject"))?;
        assert!(rejected.text.contains("abgelehnt: doppelt"));
        assert!(matches!(accepted, Err(OpError::InvalidArguments(_))));
        assert!(matches!(unknown, Err(OpError::InvalidArguments(_))));
        assert!(!temp.path().join("skills/review").exists());
        Ok(())
    }

    /// Ohne injizierte Ablage liest `/skills proposals` das Profil des
    /// gebundenen Root-Space — nie `HARW_HOME`.
    #[tokio::test]
    async fn skill_proposals_default_to_the_bound_profile() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = crate::config_util::test_home_context(temp.path())?;
        let store = SkillProposalStore::new(
            temp.path()
                .join("profiles")
                .join("default")
                .join("skills"),
        );
        let id = propose_review_skill(&store)?;
        let mut services = ServiceMap::new();
        services.insert(home);
        let (op_ctx, root) = context_with_services(services)?;

        let listed = super::skills(&op_ctx, args("proposals", None)).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        let listed = listed.map_err(ctx("proposals"))?;
        assert!(listed.text.contains(&id), "{}", listed.text);
        Ok(())
    }

    /// Ohne Ablage und ohne gebundenen Root-Space sind die Vorschlags-Aktionen
    /// nicht verfügbar, statt auf den Prozess-Root-Space auszuweichen.
    #[tokio::test]
    async fn skill_proposals_without_store_or_bound_home_are_not_available() -> TestResult {
        let (op_ctx, root) = context_with_services(ServiceMap::new())?;
        let listed = super::skills(&op_ctx, args("proposals", None)).await;
        let reviewed = super::skills(&op_ctx, args("review", Some("some-proposal"))).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        for result in [listed, reviewed] {
            match result {
                Err(OpError::NotAvailable(message)) => {
                    assert!(message.contains("Root-Space"), "{message}");
                }
                other => {
                    return Err(TestError::Unexpected(format!(
                        "expected NotAvailable, got: {other:?}"
                    )));
                }
            }
        }
        Ok(())
    }
}
