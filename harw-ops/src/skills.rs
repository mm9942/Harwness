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
//! `preview <aufgabentext> [--tool NAME] [--error TEXT] [--path PFAD]
//! [--command BEFEHL] [--budget BYTES]` (PL-97 B3) zeigt **ohne
//! Modellaufruf**, welche Skill-Abschnitte die erste Runde einer frischen
//! Sitzung getriggert einspielen würde — samt verworfener Treffer mit
//! typisiertem Grund. Gewählt wird über denselben Weg wie im Turn-Loop
//! ([`harw_core::SkillInjector::preview`]): Einstellungen aus `[skills]`
//! ([`harw_core::SkillInjectionSettings::from_config`]) und ein
//! [`harw_catalog::SkillIndex`] samt `TriggerIndex`: der injizierte
//! `Arc<SkillIndex>` der Sitzung, sonst derselbe Bau wie in der
//! Laufzeit-Montage über die vertrauten Layer; welcher, nennt die Ausgabe
//! (`Index: …`, JSON `index.source`), ebenso die vom Index übersprungenen
//! Skills und verworfenen Trigger. `validate <verzeichnis>` prüft
//! `skill.toml` und `triggers.toml` eines Skill-Verzeichnisses so, wie der
//! Index sie liest ([`validate_skill_dir`], `triggers.toml` über
//! [`harw_catalog::read_skill_triggers`]), und meldet jede Warnung von
//! [`harw_catalog::parse_triggers_file`] wörtlich. Beide sind rein lesend und
//! laufen auch während eines Turns sofort; `harw skill preview|validate`
//! nutzt dieselben Wege.
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
//!   `deactivate` | `show` | `preview` | `validate` | …), optionalen
//!   Skill-Namen (`target`) und die ungekürzten Folge-Token (`rest`).
//! - [`PreviewArgs`] — die geparsten Optionen von `preview`.
//! - [`SkillDirReport`] — Ergebnis von [`validate_skill_dir`].
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

use harw_catalog::{DropReason, SkillIndex, SkillTriggerSpec, TRIGGERS_FILE};
use harw_config::{ResolvedConfig, SkillToml};
use harw_core::skill_injection::{
    RoundInjection, SkillInjectionSettings, SkillInjector, SkillPreview, SkillPreviewInput,
};
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_registry_defaults::skill_proposal_tools::{
    CommitAuthority, LoadedSkillProposal, SkillProposalError, SkillProposalListing,
    SkillProposalStore,
};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Dateiname des Skill-Manifests innerhalb eines Skill-Verzeichnisses.
const SKILL_MANIFEST: &str = "skill.toml";

/// Größte `skill.toml`, die `validate` liest (wie der Deckel der
/// `triggers.toml`); schützt vor Geräte- und Riesendateien.
const MAX_SKILL_MANIFEST_BYTES: usize = 64 * 1024;

// ── Args ──────────────────────────────────────────────────────────────────────

/// Argumente für die `/skills`-Operation.
///
/// # Beschreibung
/// Trägt das Sub-Kommando (`action`) und einen optionalen Skill-Namen (`target`).
/// Die Felder werden von [`harw_operations::FromRawArgs`] direkt aus den
/// tokenisierten TUI-Rohargumenten befüllt — `action`/`target`/`value`
/// erhalten genau das Token an der entsprechenden Position (0-basiert), wie
/// es das Derive mit `first`/`nth` täte; `rest` erhält alle Token hinter dem
/// Sub-Kommando unverbunden, weil `preview` Optionswerte mit Leerzeichen
/// trägt (`--command "cargo test"`).
///
/// - `None` oder `action = Some("list")` → Skills auflisten (Standard-Verhalten).
/// - `action = Some("show")` mit `target` → Details eines Skills.
/// - `action = Some("activate")`/`Some("deactivate")` mit `target` → `enabled`
///   im Skill-Manifest dauerhaft umschalten.
/// - `action = Some("proposals")` → offene und entschiedene Skill-Vorschläge.
/// - `action = Some("review")`/`Some("accept")` mit `target` (Vorschlags-ID) →
///   Vorschlag ansehen bzw. übernehmen; `Some("reject")` mit `target` und
///   optionalem Grund in `value` → verwerfen.
/// - `action = Some("preview")` mit `rest` → Trigger-Vorschau ([`PreviewArgs`]).
/// - `action = Some("validate")` mit `rest` (genau ein Verzeichnis) →
///   Skill-Verzeichnis prüfen ([`validate_skill_dir`]).
///
/// `preview` und `validate` lesen `rest`; ein JSON-Aufruf ohne `rest` gilt
/// als `rest = [target, value]` (soweit gesetzt).
///
/// # Felder
/// - `action` (`Option<String>`): Token 0 — Sub-Kommando: `"list"` (Standard),
///   `"activate"`, `"deactivate"`, `"show"`. `None` wird wie `"list"` behandelt.
/// - `target` (`Option<String>`): Token 1 — Skill-Name. Pflicht für `activate`,
///   `deactivate` und `show`; bei `list` ignoriert; bei `validate` das
///   Skill-Verzeichnis.
/// - `value` (`Option<String>`): Token 2 — dritter Parameter (reserviert für
///   zukünftige Erweiterungen; aktuell ungenutzt).
/// - `rest` (`Vec<String>`): Token 1 bis Ende, unverbunden — die Optionen und
///   der Aufgabentext von `preview`.
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
/// assert_eq!(args.rest, vec!["my-skill".to_owned()]);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct SkillsArgs {
    /// Sub-Kommando: `"list"` (Standard), `"activate"`, `"deactivate"`, `"show"`. Token 0.
    ///
    /// `None` wird wie `"list"` behandelt.
    #[serde(default)]
    pub action: Option<String>,
    /// Skill-Name (z. B. `"my-skill"`). Token 1. `None` wenn nicht angegeben.
    #[serde(default)]
    pub target: Option<String>,
    /// Dritter Parameter (reserviert). Token 2. `None` wenn nicht angegeben.
    #[serde(default)]
    pub value: Option<String>,
    /// Alle Token hinter dem Sub-Kommando, unverbunden (Token 1 bis Ende).
    #[serde(default)]
    pub rest: Vec<String>,
}

impl harw_operations::FromRawArgs for SkillsArgs {
    /// Verteilt die Token wie das Derive (`first`, `nth = 1`, `nth = 2`) und
    /// behält zusätzlich alle Token hinter dem Sub-Kommando in `rest`.
    ///
    /// # Fehler
    /// Nie; jede Token-Folge ist syntaktisch gültig, die Aktion prüft ihre
    /// Argumente selbst.
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {
            action: harw_operations::args::first_optional(tokens),
            target: harw_operations::args::nth_optional(tokens, 1),
            value: harw_operations::args::nth_optional(tokens, 2),
            rest: tokens.get(1..).map(<[String]>::to_vec).unwrap_or_default(),
        })
    }
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
    Ok(
        Arc::new(LayeredSkillStatePersistence::from_home_context(home)?)
            as Arc<dyn SkillStatePersistence>,
    )
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

// ── Vorschau und Prüfung (PL-97 B3) ───────────────────────────────────────────

/// Modellfenster der Vorschau in Token: `0` = unbekannt, dann gilt der
/// Pro-Runde-Deckel (`[skills] per_turn_bytes` bzw. `--budget`) unverändert.
/// Im Turn-Loop sind es ~3 % des Fensters (4 Bytes je Token), gedeckelt:
/// denselben Wert ergibt erst ein Fenster ab Deckel / 0,12 Token (rund
/// 34 000 Token beim Standarddeckel von 4096 Bytes, rund 546 000 bei 65 536);
/// kleinere Fenster wählen knapper. Die genaue Schwelle nennt die Ausgabe
/// ([`full_cap_window_tokens`]).
const PREVIEW_WINDOW_TOKENS: u64 = 0;

/// Turn-Kennung der `injection`-Ereignisse, in deren Form die Vorschau ihre
/// Lieferungen zeigt (sie wird nie gespeichert).
const PREVIEW_TURN_ID: &str = "preview";

/// Aufrufform von `/skills preview` für Fehlermeldungen.
const PREVIEW_USAGE: &str = "usage: /skills preview [--tool NAME] [--error TEXT] [--path PATH] \
     [--command COMMAND] [--budget BYTES] [--] <task text>";

/// Die geparsten Argumente von `/skills preview`.
///
/// # Felder
/// - `input`: die Trigger-Eingaben (Aufgabentext, `--tool`, `--error`,
///   `--path`, `--command`).
/// - `budget`: `--budget`, der Pro-Runde-Deckel in Bytes statt
///   `[skills] per_turn_bytes`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviewArgs {
    /// Die Trigger-Eingaben.
    pub input: SkillPreviewInput,
    /// `--budget`: Pro-Runde-Deckel in Bytes.
    pub budget: Option<usize>,
}

impl PreviewArgs {
    /// Parst die Token hinter `preview`.
    ///
    /// # Beschreibung
    /// `--tool`, `--error`, `--path` und `--command` sind wiederholbar und
    /// nehmen genau das folgende Token (oder `--option=wert`); `--budget`
    /// nimmt eine Byte-Zahl im erlaubten Bereich von `[skills]
    /// per_turn_bytes`. Alle übrigen Token — und alles nach `--` — bilden,
    /// mit Leerzeichen verbunden, den Aufgabentext.
    ///
    /// # Fehler
    /// [`OpError::InvalidArguments`] bei einer unbekannten Option, einer
    /// Option ohne Wert, einem ungültigen Budget oder ganz ohne Eingabe.
    pub fn parse(tokens: &[String]) -> Result<Self, OpError> {
        let mut args = Self::default();
        let mut task: Vec<&str> = Vec::new();
        let mut options_done = false;
        let mut tokens = tokens.iter();
        while let Some(token) = tokens.next() {
            if options_done || !token.starts_with("--") {
                task.push(token.as_str());
                continue;
            }
            if token == "--" {
                options_done = true;
                continue;
            }
            let (option, inline) = match token.split_once('=') {
                Some((option, value)) => (option, Some(value.to_owned())),
                None => (token.as_str(), None),
            };
            if !matches!(
                option,
                "--tool" | "--error" | "--path" | "--command" | "--budget"
            ) {
                return Err(OpError::InvalidArguments(format!(
                    "unknown option '{option}' for /skills preview; {PREVIEW_USAGE}"
                )));
            }
            let value = match inline {
                Some(value) => value,
                None => tokens.next().cloned().ok_or_else(|| {
                    OpError::InvalidArguments(format!(
                        "option '{option}' requires a value; {PREVIEW_USAGE}"
                    ))
                })?,
            };
            match option {
                "--tool" => args.input.tools.push(value),
                "--error" => args.input.tool_output.push(value),
                "--path" => args.input.paths.push(value),
                "--command" => args.input.commands.push(value),
                _ => args.budget = Some(parse_preview_budget(&value)?),
            }
        }
        args.input.task = task.join(" ");
        if args.input.is_empty() {
            return Err(OpError::InvalidArguments(format!(
                "/skills preview needs task text or at least one of --tool, --error, --path, \
                 --command; {PREVIEW_USAGE}"
            )));
        }
        Ok(args)
    }
}

/// Das kleinste Modellfenster in Token, für das eine Sitzung denselben
/// Pro-Runde-Deckel wählt wie die Vorschau (die ein unbekanntes Fenster
/// annimmt, [`PREVIEW_WINDOW_TOKENS`]).
///
/// # Beschreibung
/// Sucht binär über [`SkillInjectionSettings::per_turn_bytes`] — dieselbe
/// Ableitung wie im Turn-Loop, ohne ihren Anteil hier zu wiederholen; sie
/// wächst monoton mit dem Fenster.
fn full_cap_window_tokens(settings: &SkillInjectionSettings) -> u64 {
    let cap = settings.per_turn_cap_bytes;
    let (mut low, mut high) = (1_u64, u64::MAX / 8);
    if settings.per_turn_bytes(high) < cap {
        return high;
    }
    while low < high {
        let middle = low + (high - low) / 2;
        if settings.per_turn_bytes(middle) >= cap {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    low
}

/// Die Folge-Token einer Aktion: `rest` aus den Rohargumenten, für einen
/// JSON-Aufruf ohne `rest` ersatzweise `target` und `value`.
fn action_tokens(args: &SkillsArgs) -> Vec<String> {
    if !args.rest.is_empty() {
        return args.rest.clone();
    }
    args.target.iter().chain(&args.value).cloned().collect()
}

/// Prüft `--budget` gegen den erlaubten Bereich von `[skills] per_turn_bytes`.
fn parse_preview_budget(value: &str) -> Result<usize, OpError> {
    let (min, max) = harw_config::skills_toml::SKILLS_PER_TURN_BYTES_RANGE;
    value
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|bytes| (min..=max).contains(bytes))
        .ok_or_else(|| {
            OpError::InvalidArguments(format!(
                "--budget must be a byte count between {min} and {max}, got '{value}'"
            ))
        })
}

/// Woher der Index einer Vorschau stammt (Text `Index: …`, JSON `index`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum PreviewIndexSource {
    /// Der injizierte `Arc<SkillIndex>` der Sitzung.
    Session,
    /// Neu gebaut über diese Layer (aufsteigende Präzedenz) samt
    /// eingebettetem Bündel.
    Layers(Vec<PathBuf>),
}

impl PreviewIndexSource {
    /// Eine Zeile für die Textausgabe.
    fn render(&self) -> String {
        match self {
            Self::Session => "Index: Skill-Index der Sitzung".to_owned(),
            Self::Layers(layers) => format!(
                "Index: neu gebaut wie in der Laufzeit-Montage aus {} vertrauten Layer(n) des \
                 gebundenen Root-Space samt eingebettetem Bündel (ein eingebetteter Lauf liest \
                 nur das Bündel seines Artefakts)",
                layers.len()
            ),
        }
    }

    /// JSON-Form (`index` der Vorschau).
    fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Session => serde_json::json!({ "source": "session" }),
            Self::Layers(layers) => serde_json::json!({
                "source": "layers",
                "layers": layers
                    .iter()
                    .map(|layer| layer.display().to_string())
                    .collect::<Vec<_>>(),
            }),
        }
    }
}

/// Die vertrauten Layer des gebundenen Root-Space in derselben Folge wie
/// `ConfigTrustReport::layers` der Laufzeit (`harw_runtime::config`):
/// Root, Profil, vertrauter Repo-Layer und danach das
/// Projekt-Einstellungsverzeichnis, sobald es `settings.toml` oder
/// `config.toml` enthält.
///
/// # Fehler
/// Wie [`crate::config_util::bound_config_layers`].
fn preview_index_layers(
    home: &harw_home::ResolvedHomeContext,
) -> Result<Vec<PathBuf>, harw_home::HomeError> {
    let mut layers = crate::config_util::bound_config_layers(home)?;
    if let Some(settings_dir) = home.project_settings_path.parent()
        && (home.project_settings_path.is_file() || settings_dir.join("config.toml").is_file())
    {
        layers.push(settings_dir.to_path_buf());
    }
    Ok(layers)
}

/// Der Skill-Index der Vorschau: ein injizierter `Arc<SkillIndex>` (der Index
/// der laufenden Montage), sonst derselbe Bau wie in der Laufzeit-Montage —
/// [`SkillIndex::build`] über die vertrauten Layer des gebundenen Root-Space
/// ([`preview_index_layers`]), eingebettete Skills als Rückfall, der
/// `TriggerIndex` einmal mit ihm.
///
/// # Fehler
/// - [`OpError::NotAvailable`]: kein Index injiziert und kein Root-Space
///   gebunden (kein Rückfall auf `HARW_HOME`).
/// - [`OpError::Execution`]: Layer des gebundenen Root-Space nicht auflösbar.
fn preview_skill_index(ctx: &OpContext) -> Result<(Arc<SkillIndex>, PreviewIndexSource), OpError> {
    if let Some(index) = ctx.service::<Arc<SkillIndex>>() {
        return Ok((Arc::clone(index), PreviewIndexSource::Session));
    }
    let home = crate::config_util::bound_home(ctx)?;
    let layers = preview_index_layers(home)
        .map_err(|error| OpError::Execution(format!("Config-Layer nicht auflösbar: {error}")))?;
    let index = Arc::new(SkillIndex::build(&layers));
    Ok((index, PreviewIndexSource::Layers(layers)))
}

/// Führt `/skills preview …` aus: dieselben Einstellungen, derselbe Index und
/// derselbe Auswahlweg wie die erste Runde einer Sitzung, ohne Modellaufruf.
///
/// # Fehler
/// Wie [`PreviewArgs::parse`] und [`preview_skill_index`].
fn run_preview(
    ctx: &OpContext,
    config: &ResolvedConfig,
    args: &SkillsArgs,
) -> Result<OpOutput, OpError> {
    let preview_args = PreviewArgs::parse(&action_tokens(args))?;
    let mut settings = SkillInjectionSettings::from_config(&config.harness.skills);
    if let Some(budget) = preview_args.budget {
        settings.per_turn_cap_bytes = budget;
    }
    let (index, source) = preview_skill_index(ctx)?;
    let preview = SkillInjector::new(Arc::clone(&index), settings)
        .preview(&preview_args.input, PREVIEW_WINDOW_TOKENS);
    let frame = PreviewFrame {
        input: &preview_args.input,
        source: &source,
        index: &index,
        full_cap_window_tokens: full_cap_window_tokens(&settings),
    };
    Ok(OpOutput {
        text: render_preview(&frame, &preview),
        data: Some(preview_json(&frame, &preview)),
    })
}

/// Was eine Vorschau neben der Auswahl selbst zeigt.
struct PreviewFrame<'a> {
    /// Die Trigger-Eingaben.
    input: &'a SkillPreviewInput,
    /// Woher der Index stammt.
    source: &'a PreviewIndexSource,
    /// Der Index (für übersprungene Skills und Trigger-Warnungen).
    index: &'a SkillIndex,
    /// Ab welchem Modellfenster eine Sitzung denselben Deckel wählt.
    full_cap_window_tokens: u64,
}

/// Stabiler Schlüssel eines Verwerfungsgrunds (JSON, Klammer im Text).
fn drop_reason_key(reason: DropReason) -> &'static str {
    match reason {
        DropReason::AlreadyDelivered => "already_delivered",
        DropReason::TurnBudget => "turn_budget",
        DropReason::SessionBudget => "session_budget",
    }
}

/// `skill` bzw. `skill § abschnitt`.
fn excerpt_label(skill: &str, section: Option<&str>) -> String {
    match section {
        Some(section) => format!("{skill} § {section}"),
        None => skill.to_owned(),
    }
}

/// Die nicht leeren Eingaben als eine Zeile (zur Kontrolle der Quotierung).
fn render_preview_input(input: &SkillPreviewInput) -> String {
    let mut parts = Vec::new();
    if !input.task.trim().is_empty() {
        parts.push(format!("Text „{}“", first_line(input.task.trim(), 80)));
    }
    for (label, values) in [
        ("--tool", &input.tools),
        ("--error", &input.tool_output),
        ("--path", &input.paths),
        ("--command", &input.commands),
    ] {
        if !values.is_empty() {
            let shown: Vec<String> = values.iter().map(|value| first_line(value, 80)).collect();
            parts.push(format!("{label} {}", shown.join(", ")));
        }
    }
    format!("Eingaben: {}", parts.join(" · "))
}

/// Rendert eine Vorschau für `/skills preview`.
fn render_preview(frame: &PreviewFrame<'_>, preview: &SkillPreview) -> String {
    let round = &preview.round;
    let mut lines = vec![
        "Skill-Vorschau (ohne Modellaufruf; erste Runde einer frischen Sitzung)".to_owned(),
        render_preview_input(frame.input),
        frame.source.render(),
        format!(
            "Budget: {} Bytes je Runde, {} Bytes je Sitzung (Modellfenster unbekannt: es gilt der \
             Pro-Runde-Deckel; eine Sitzung wählt so erst ab {} Token Fenster, darunter knapper)",
            preview.budget.per_turn_bytes,
            preview.budget.per_session_bytes,
            frame.full_cap_window_tokens
        ),
    ];
    if !preview.enabled {
        lines.push(
            "Hinweis: [skills] triggered_injection = false — eine Sitzung spielt keinen dieser \
             Auszüge ein."
                .to_owned(),
        );
    }
    if round.delivered.is_empty() && round.dropped.is_empty() && round.suppressed.is_empty() {
        lines.push("Kein Skill getroffen.".to_owned());
    } else {
        render_preview_round(round, &mut lines);
    }
    render_index_notes(frame.index, &mut lines);
    lines.join("\n")
}

/// Die Lieferungen, verworfenen Treffer und zurückgehaltenen Skills einer
/// Vorschau-Runde.
fn render_preview_round(round: &RoundInjection, lines: &mut Vec<String>) {
    let bytes: usize = round
        .delivered
        .iter()
        .map(|delivered| delivered.bytes)
        .sum();
    lines.push(format!(
        "Eingespielt ({} Auszug/Auszüge, {bytes} Bytes):",
        round.delivered.len()
    ));
    if round.delivered.is_empty() {
        lines.push("- keiner".to_owned());
    }
    for delivered in &round.delivered {
        // Dieselbe Form wie das `injection`-Ereignis der Sitzung.
        let record = delivered.to_record(PREVIEW_TURN_ID);
        lines.push(format!(
            "- {} — {} {} · {} Bytes · {}",
            excerpt_label(&record.skill, record.section.as_deref()),
            record.trigger_kind,
            record.trigger_key,
            record.bytes,
            record.trust
        ));
    }
    if !round.dropped.is_empty() {
        lines.push(format!("Verworfen ({}):", round.dropped.len()));
        for dropped in &round.dropped {
            lines.push(format!(
                "- {} — {} ({})",
                excerpt_label(&dropped.skill, dropped.section.as_deref()),
                dropped.reason,
                drop_reason_key(dropped.reason)
            ));
        }
    }
    if !round.suppressed.is_empty() {
        lines.push(format!(
            "Zurückgehalten (importiert, [skills] inject_imported = false): {}",
            round.suppressed.join(", ")
        ));
    }
}

/// Was der Index beim Bau übersprungen bzw. an Triggern verworfen hat — der
/// Grund, warum ein erwarteter Skill nicht getroffen wird. Nichts, wenn der
/// Index sauber ist.
fn render_index_notes(index: &SkillIndex, lines: &mut Vec<String>) {
    let (skipped, warnings) = (index.skipped(), index.warnings());
    if skipped.is_empty() && warnings.is_empty() {
        return;
    }
    lines.push(format!(
        "Index-Hinweise ({} Skill(s) übersprungen, {} Trigger-Warnung(en)):",
        skipped.len(),
        warnings.len()
    ));
    for skipped in skipped {
        lines.push(format!(
            "- übersprungen: {} — {}",
            skipped.location, skipped.reason
        ));
    }
    for warning in warnings {
        lines.push(format!("- Warnung: {} — {}", warning.skill, warning.reason));
    }
}

/// JSON-Form einer Vorschau (`harw skill preview --json`).
fn preview_json(frame: &PreviewFrame<'_>, preview: &SkillPreview) -> serde_json::Value {
    let round = &preview.round;
    let input = frame.input;
    let injections: Vec<serde_json::Value> = round
        .delivered
        .iter()
        .map(|delivered| {
            let record = delivered.to_record(PREVIEW_TURN_ID);
            serde_json::json!({
                "skill": record.skill,
                "section": record.section,
                "label": delivered.label,
                "trigger_kind": record.trigger_kind,
                "trigger_key": record.trigger_key,
                "bytes": record.bytes,
                "trust": record.trust,
                "sha256": record.sha256,
            })
        })
        .collect();
    let dropped: Vec<serde_json::Value> = round
        .dropped
        .iter()
        .map(|dropped| {
            serde_json::json!({
                "skill": dropped.skill,
                "section": dropped.section,
                "reason": drop_reason_key(dropped.reason),
                "reason_text": dropped.reason.to_string(),
            })
        })
        .collect();
    let bytes: usize = round
        .delivered
        .iter()
        .map(|delivered| delivered.bytes)
        .sum();
    let mut index = frame.source.to_json();
    if let serde_json::Value::Object(fields) = &mut index {
        fields.insert(
            "skipped".to_owned(),
            frame
                .index
                .skipped()
                .iter()
                .map(|skipped| {
                    serde_json::json!({ "location": skipped.location, "reason": skipped.reason })
                })
                .collect(),
        );
        fields.insert(
            "warnings".to_owned(),
            frame
                .index
                .warnings()
                .iter()
                .map(|warning| serde_json::json!({ "skill": warning.skill, "reason": warning.reason }))
                .collect(),
        );
    }
    serde_json::json!({
        "action": "preview",
        "index": index,
        "injection_enabled": preview.enabled,
        "budget": {
            "per_turn_bytes": preview.budget.per_turn_bytes,
            "per_session_bytes": preview.budget.per_session_bytes,
            "window_tokens": PREVIEW_WINDOW_TOKENS,
            "full_cap_window_tokens": frame.full_cap_window_tokens,
        },
        "input": {
            "task": input.task,
            "tools": input.tools,
            "errors": input.tool_output,
            "paths": input.paths,
            "commands": input.commands,
        },
        "injections": injections,
        "dropped": dropped,
        "suppressed": round.suppressed,
        "bytes": bytes,
    })
}

/// Ergebnis von [`validate_skill_dir`]: was der Skill-Index beim Lesen dieses
/// Verzeichnisses verwerfen oder bemängeln würde.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDirReport {
    /// Das geprüfte Verzeichnis.
    pub dir: PathBuf,
    /// Skill-Name aus `skill.toml`, falls lesbar.
    pub name: Option<String>,
    /// `enabled` aus `skill.toml`, falls lesbar (`false`: der Index führt den
    /// Skill nicht).
    pub enabled: Option<bool>,
    /// Fehler, mit denen der Index den Skill überspringen würde
    /// (`skill.toml`, Anweisungstext, ein Symlink- oder versteckter
    /// Verzeichnisname).
    pub errors: Vec<String>,
    /// Verworfene Teile der `triggers.toml`, wörtlich wie
    /// [`harw_catalog::parse_triggers_file`] (bzw. das Lesen der Datei im
    /// Index) sie meldet; der Skill bliebe geladen.
    pub warnings: Vec<String>,
    /// Die gültigen Teile der `triggers.toml`; `None` ohne lesbare Datei.
    pub triggers: Option<SkillTriggerSpec>,
}

impl SkillDirReport {
    /// Ob nichts zu beanstanden ist (keine Fehler, keine Warnungen).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty() && self.warnings.is_empty()
    }

    /// Menschenlesbarer Bericht (`/skills validate`, `harw skill validate`).
    #[must_use]
    pub fn render(&self) -> String {
        let mut lines = vec![format!("Skill-Verzeichnis: {}", self.dir.display())];
        match (&self.name, self.enabled) {
            (Some(name), Some(false)) => lines.push(format!(
                "Skill: {name} (enabled = false — der Index führt ihn nicht)"
            )),
            (Some(name), _) => lines.push(format!("Skill: {name}")),
            (None, _) => {}
        }
        lines.extend(self.errors.iter().map(|error| format!("Fehler: {error}")));
        lines.extend(
            self.warnings
                .iter()
                .map(|warning| format!("Warnung: {warning}")),
        );
        match &self.triggers {
            None if self.warnings.is_empty() => lines.push(format!(
                "{TRIGGERS_FILE}: keine (der Skill wird nur über skills.search/skills.load \
                 gefunden)"
            )),
            None => lines.push(format!("{TRIGGERS_FILE}: verworfen")),
            Some(spec) if spec.triggers.is_empty() => {
                lines.push(format!("{TRIGGERS_FILE}: keine gültigen Trigger"));
            }
            Some(spec) => {
                let triggers = &spec.triggers;
                let mut line = format!(
                    "{TRIGGERS_FILE}: keywords {}, error_codes {}, paths {}, tools {}, \
                     commands {}, priority {}",
                    triggers.keywords.len(),
                    triggers.error_codes.len(),
                    triggers.paths.len(),
                    triggers.tools.len(),
                    triggers.commands.len(),
                    triggers.priority
                );
                if let Some(when) = &spec.when {
                    line.push_str(&format!("; when „{when}“"));
                }
                lines.push(line);
            }
        }
        lines.push(if self.is_valid() {
            "Ergebnis: gültig".to_owned()
        } else {
            format!(
                "Ergebnis: zu beanstanden ({} Fehler, {} Warnung(en))",
                self.errors.len(),
                self.warnings.len()
            )
        });
        lines.join("\n")
    }

    /// JSON-Form (`harw skill validate --json`).
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let triggers = self.triggers.as_ref().map(|spec| {
            serde_json::json!({
                "when": spec.when,
                "keywords": spec.triggers.keywords,
                "error_codes": spec.triggers.error_codes,
                "paths": spec.triggers.paths,
                "tools": spec.triggers.tools,
                "commands": spec.triggers.commands,
                "priority": spec.triggers.priority,
            })
        });
        serde_json::json!({
            "action": "validate",
            "dir": self.dir.display().to_string(),
            "skill": self.name,
            "enabled": self.enabled,
            "valid": self.is_valid(),
            "errors": self.errors,
            "warnings": self.warnings,
            "triggers": triggers,
        })
    }
}

/// Prüft ein Skill-Verzeichnis so, wie der Skill-Index es liest.
///
/// # Beschreibung
/// `skill.toml` muss eine reguläre Datei (wie im Index, der andere
/// Verzeichnisse überspringt) von höchstens 64 KiB und ein gültiges
/// [`SkillToml`] sein, sein Anweisungstext über
/// [`harw_catalog::load_skill_runtime_snapshot`] ladbar (Symlink- und
/// Traversal-Schutz, 512-KiB-Deckel) — sonst überspränge der Index den Skill
/// (Fehler). Ebenso ein Fehler: das Verzeichnis selbst ist ein Symlink oder
/// sein Name beginnt mit `.` (beides liest der Index in keinem Layer). Eine vorhandene `triggers.toml` liest dieselbe Funktion wie im
/// Index ([`harw_catalog::read_skill_triggers`]: Datei, Größe, sicheres
/// Lesen) und prüft [`harw_catalog::parse_triggers_file`]; jede dabei
/// verworfene Angabe erscheint wörtlich als Warnung. Anders als der Index
/// prüft die Funktion auch einen Skill mit `enabled = false` vollständig.
/// Rein lesend; liest nie aus FIFOs oder Gerätedateien.
///
/// # Rückgabe
/// Der [`SkillDirReport`]; [`SkillDirReport::is_valid`] ist `false`, sobald
/// es einen Fehler oder eine Warnung gibt.
#[must_use]
pub fn validate_skill_dir(dir: &Path) -> SkillDirReport {
    let mut report = SkillDirReport {
        dir: dir.to_path_buf(),
        name: None,
        enabled: None,
        errors: Vec::new(),
        warnings: Vec::new(),
        triggers: None,
    };
    if !dir.is_dir() {
        report
            .errors
            .push(format!("'{}' ist kein Verzeichnis", dir.display()));
        return report;
    }
    // Wie `SkillIndex::build` beim Lesen eines Layers: Skill-Verzeichnisse,
    // die Symlinks sind oder mit '.' beginnen, überspringt der Index.
    if std::fs::symlink_metadata(dir).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        report.errors.push(
            "das Verzeichnis ist ein Symlink (der Index überspringt Skill-Verzeichnisse, die \
             Symlinks sind)"
                .to_owned(),
        );
    }
    if dir
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with('.'))
    {
        report.errors.push(
            "der Verzeichnisname beginnt mit '.' (der Index überspringt versteckte \
             Skill-Verzeichnisse)"
                .to_owned(),
        );
    }
    match read_skill_manifest(dir) {
        Err(reason) => report.errors.push(reason),
        Ok(source) => match toml::from_str::<SkillToml>(&source) {
            Err(error) => report
                .errors
                .push(format!("{SKILL_MANIFEST} ungültig: {error}")),
            Ok(manifest) => {
                report.name = Some(manifest.name.clone());
                report.enabled = Some(manifest.enabled);
                if let Err(error) = harw_catalog::load_skill_runtime_snapshot(dir, &manifest) {
                    report.errors.push(error.to_string());
                }
            }
        },
    }
    match harw_catalog::read_skill_triggers(dir) {
        None => {}
        Some(Err(reason)) => report.warnings.push(reason),
        Some(Ok(source)) => {
            let (spec, warnings) = harw_catalog::parse_triggers_file(&source);
            report.warnings.extend(warnings);
            report.triggers = Some(spec);
        }
    }
    report
}

/// Liest `skill.toml` eines Skill-Verzeichnisses für `validate`: nur eine
/// reguläre Datei (der Index überspringt das Verzeichnis sonst; ein FIFO oder
/// eine Gerätedatei würde das Lesen blockieren bzw. nie enden), höchstens
/// [`MAX_SKILL_MANIFEST_BYTES`] Bytes, auch wenn die Datei während des Lesens
/// wächst.
///
/// # Fehler
/// Die Meldung für [`SkillDirReport::errors`].
fn read_skill_manifest(dir: &Path) -> Result<String, String> {
    let path = dir.join(SKILL_MANIFEST);
    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "{SKILL_MANIFEST} fehlt (der Index überspringt das Verzeichnis)"
            ));
        }
        Err(error) => return Err(format!("{SKILL_MANIFEST} nicht lesbar: {error}")),
    };
    if !metadata.is_file() {
        return Err(format!(
            "{SKILL_MANIFEST} ist keine Datei (der Index überspringt das Verzeichnis)"
        ));
    }
    let too_large = || format!("{SKILL_MANIFEST} größer als {MAX_SKILL_MANIFEST_BYTES} Bytes");
    let limit = u64::try_from(MAX_SKILL_MANIFEST_BYTES).unwrap_or(u64::MAX);
    if metadata.len() > limit {
        return Err(too_large());
    }
    let file = std::fs::File::open(&path)
        .map_err(|error| format!("{SKILL_MANIFEST} nicht lesbar: {error}"))?;
    let mut source = String::new();
    file.take(limit.saturating_add(1))
        .read_to_string(&mut source)
        .map_err(|error| format!("{SKILL_MANIFEST} nicht lesbar: {error}"))?;
    if source.len() > MAX_SKILL_MANIFEST_BYTES {
        return Err(too_large());
    }
    Ok(source)
}

/// Führt `/skills validate <verzeichnis>` aus; ein relatives Verzeichnis gilt
/// relativ zur Workspace-Wurzel der Sitzung.
///
/// # Fehler
/// [`OpError::InvalidArguments`] ohne Verzeichnis oder mit überzähligen
/// Token. Ein beanstandetes Verzeichnis ist kein Fehler der Operation: der
/// Bericht nennt es (`valid = false`).
fn run_validate(ctx: &OpContext, args: &SkillsArgs) -> Result<OpOutput, OpError> {
    let tokens = action_tokens(args);
    let target = match tokens.as_slice() {
        [] => None,
        [target] => Some(target).filter(|target| !target.trim().is_empty()),
        _ => {
            return Err(OpError::InvalidArguments(
                "action 'validate' takes exactly one skill directory (quote paths with spaces)"
                    .to_owned(),
            ));
        }
    }
    .ok_or_else(|| {
        OpError::InvalidArguments("action 'validate' requires a skill directory".to_owned())
    })?;
    let path = Path::new(target);
    let dir = if path.is_absolute() {
        path.to_path_buf()
    } else {
        ctx.sandbox().workspace().canonical_root().join(path)
    };
    let report = validate_skill_dir(&dir);
    Ok(OpOutput {
        text: report.render(),
        data: Some(report.to_json()),
    })
}

// ── Operation ─────────────────────────────────────────────────────────────────

/// Liest den Skill-Katalog und schaltet Skills dauerhaft an oder ab.
///
/// # Beschreibung
/// - `list` (Standard): alle konfigurierten Skills mit Status.
/// - `show <name>`: Details eines Skills.
/// - `preview <aufgabentext> [--tool …] [--error …] [--path …] [--command …]
///   [--budget BYTES]`: Trigger-Vorschau ohne Modellaufruf ([`PreviewArgs`]).
/// - `validate <verzeichnis>`: prüft `skill.toml` und `triggers.toml`
///   ([`validate_skill_dir`]).
/// - `activate <name>` / `deactivate <name>`: setzt `enabled` im wirksamen
///   Skill-Manifest über [`SkillStatePersistence`]; wirksam ab dem nächsten
///   Config-Laden.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext; `list`/`show`/`preview`/
///   `activate`/`deactivate` benötigen `Arc<ResolvedConfig>` (oder die
///   Live-Config). `preview` nutzt einen injizierten `Arc<SkillIndex>`, sonst
///   die Layer des gebundenen Root-Space; `validate` braucht nichts davon.
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
    summary = "Skill-Katalog (list/show/preview/validate/activate/deactivate) und Skill-Vorschläge (proposals/review/accept/reject).",
    domain = "catalog_config",
    permission = "operator",
    command(
        path = "/skills",
        visibility = "channel_reduced",
        channel_subcommands = "-,list,show",
        busy_subcommands = "-=immediate, list=immediate, show=immediate, preview=immediate, validate=immediate"
    )
)]
async fn skills(ctx: &OpContext, args: SkillsArgs) -> Result<OpOutput, OpError> {
    if let Some(action @ ("proposals" | "review" | "accept" | "reject")) = args.action.as_deref() {
        return run_proposal_action(ctx, action, &args);
    }
    // PL-97 B3: `validate` prüft nur Dateien und braucht keinen Config-Dienst.
    if args.action.as_deref() == Some("validate") {
        return run_validate(ctx, &args);
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
        "preview" => run_preview(ctx, &config, &args),
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
            super::skills(&ctx, SkillsArgs { action: Some("list".to_owned()), ..SkillsArgs::default() }).await,
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
                rest: vec![target.to_owned(), value.to_owned()],
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
            rest: target.map(str::to_owned).into_iter().collect(),
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
        let manifest =
            write_manifest(temp.path(), "review", "name = \"review\"\nenabled = true\n")?;
        let mut services = ServiceMap::new();
        services.insert(Arc::new(review_config()));
        services.insert(crate::config_util::test_home_context(temp.path())?);
        let (op_ctx, root) = context_with_services(services)?;

        let result = super::skills(&op_ctx, args("deactivate", Some("review"))).await;
        std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;

        let output = result.map_err(ctx("deactivate"))?;
        assert!(
            output.text.contains("'review' deaktiviert"),
            "{}",
            output.text
        );
        assert!(!read_enabled(&manifest)?);
        Ok(())
    }

    /// Ohne Persistenzdienst und ohne gebundenen Root-Space ist `deactivate`
    /// nicht verfügbar, statt auf den Prozess-Root-Space auszuweichen.
    #[tokio::test]
    async fn skills_deactivate_without_persistence_or_bound_home_is_not_available() -> TestResult {
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
                rest: vec![id.clone(), "doppelt".to_owned()],
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
        let store =
            SkillProposalStore::new(temp.path().join("profiles").join("default").join("skills"));
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

    // ── preview / validate (PL-97 B3) ────────────────────────────────────────

    mod preview_validate {
        use std::path::{Path, PathBuf};
        use std::sync::Arc;

        use harw_catalog::SkillIndex;
        use harw_core::skill_injection::{
            SkillInjectionSettings, SkillInjector, SkillPreviewInput,
        };
        use harw_operations::context::ServiceMap;
        use harw_operations::operation::BusyAvailability;
        use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput, Operation};

        use super::super::{PreviewArgs, SkillsArgs, SkillsOperation, validate_skill_dir};
        use super::context_with_services;
        use crate::test_support::{TestError, TestResult, ctx};
        use crate::testutil::toks;

        const BORROW_BODY: &str = "# Borrow\n\nIntro.\n\n## When to use\n\nWhenever the borrow checker complains.\n\n## Cause E0505\n\nThe value is still borrowed.\n\n## Fix\n\nOwn the value first.\n";
        const BORROW_TRIGGERS: &str = "[triggers]\nerror_codes = [\"E0505\"]\npaths = [\"**/*.rs\"]\ncommands = [\"cargo test\"]\npriority = 70\n";
        const LINT_BODY: &str = "# Lint\n\n## Clippy\n\nRun clippy with -D warnings and fix every lint before the commit.\n";
        const LINT_TRIGGERS: &str =
            "[triggers]\nkeywords = [\"clippy\"]\ntools = [\"shell.exec\"]\npriority = 40\n";

        /// Ein Index über ein Bündel im Speicher: `(name, triggers.toml, text)`.
        fn index() -> Arc<SkillIndex> {
            let mut files: Vec<(String, String)> = Vec::new();
            for (name, triggers, body) in [
                ("borrow", BORROW_TRIGGERS, BORROW_BODY),
                ("lint", LINT_TRIGGERS, LINT_BODY),
            ] {
                files.push((
                    format!("skills/{name}/skill.toml"),
                    format!("name = \"{name}\"\ndescription = \"Use when {name} matters.\"\n"),
                ));
                files.push((format!("skills/{name}/instructions.md"), body.to_owned()));
                files.push((format!("skills/{name}/triggers.toml"), triggers.to_owned()));
            }
            let refs: Vec<(&str, &str)> = files
                .iter()
                .map(|(path, content)| (path.as_str(), content.as_str()))
                .collect();
            Arc::new(SkillIndex::build_with_bundle(&[], &refs))
        }

        fn preview_context(index: Arc<SkillIndex>) -> TestResult<(OpContext, PathBuf)> {
            let mut services = ServiceMap::new();
            services.insert(Arc::new(harw_config::ResolvedConfig::default()));
            services.insert(index);
            context_with_services(services)
        }

        async fn run(op_ctx: &OpContext, tokens: &[&str]) -> Result<OpOutput, OpError> {
            let args = SkillsArgs::from_raw_args(&toks(tokens))?;
            super::super::skills(op_ctx, args).await
        }

        fn input() -> SkillPreviewInput {
            SkillPreviewInput {
                task: "cargo test fails, clippy too".to_owned(),
                tools: vec!["shell.exec".to_owned()],
                tool_output: vec!["error[E0505]: cannot move out of `v`".to_owned()],
                paths: vec!["src/lib.rs".to_owned()],
                commands: vec!["cargo test".to_owned()],
            }
        }

        const PREVIEW_TOKENS: &[&str] = &[
            "preview",
            "--tool",
            "shell.exec",
            "--error",
            "error[E0505]: cannot move out of `v`",
            "--path",
            "src/lib.rs",
            "--command",
            "cargo test",
            "--",
            "cargo test fails, clippy too",
        ];

        #[test]
        fn skills_args_keep_the_unjoined_tokens_after_the_action() -> TestResult {
            let args = SkillsArgs::from_raw_args(&toks(&[
                "preview",
                "cargo test fails",
                "--command",
                "cargo test",
            ]))
            .map_err(ctx("SkillsArgs::from_raw_args"))?;
            assert_eq!(args.action.as_deref(), Some("preview"));
            assert_eq!(args.target.as_deref(), Some("cargo test fails"));
            assert_eq!(args.value.as_deref(), Some("--command"));
            assert_eq!(
                args.rest,
                toks(&["cargo test fails", "--command", "cargo test"])
            );
            Ok(())
        }

        #[test]
        fn preview_args_parse_repeatable_options_budget_and_task_text() -> TestResult {
            let args = PreviewArgs::parse(&toks(&[
                "--tool",
                "sys.ps",
                "cargo",
                "test",
                "--command",
                "cargo test",
                "--error=E0505",
                "--path",
                "src/lib.rs",
                "--tool=fs.read",
                "--budget",
                "512",
                "--",
                "--literal",
            ]))
            .map_err(ctx("PreviewArgs::parse"))?;
            assert_eq!(args.input.task, "cargo test --literal");
            assert_eq!(args.input.tools, toks(&["sys.ps", "fs.read"]));
            assert_eq!(args.input.tool_output, toks(&["E0505"]));
            assert_eq!(args.input.paths, toks(&["src/lib.rs"]));
            assert_eq!(args.input.commands, toks(&["cargo test"]));
            assert_eq!(args.budget, Some(512));
            Ok(())
        }

        #[test]
        fn preview_args_reject_bad_options_budgets_and_empty_input() -> TestResult {
            for (tokens, needle) in [
                (vec!["--frobnicate", "x"], "unknown option '--frobnicate'"),
                (vec!["task", "--tool"], "option '--tool' requires a value"),
                (
                    vec!["task", "--budget", "12"],
                    "--budget must be a byte count",
                ),
                (
                    vec!["task", "--budget", "many"],
                    "--budget must be a byte count",
                ),
                (vec![], "needs task text"),
                (vec!["--", "  "], "needs task text"),
            ] {
                match PreviewArgs::parse(&toks(&tokens)) {
                    Err(OpError::InvalidArguments(message)) => {
                        assert!(message.contains(needle), "{tokens:?}: {message}");
                    }
                    other => {
                        return Err(TestError::Unexpected(format!(
                            "{tokens:?}: expected InvalidArguments, got {other:?}"
                        )));
                    }
                }
            }
            Ok(())
        }

        /// `/skills preview` liefert genau die Auswahl des gemeinsamen
        /// Injektors (`SkillInjector::preview`) mit denselben Einstellungen —
        /// derselbe Weg wie die erste Runde des Turn-Loops.
        #[tokio::test]
        async fn skills_preview_prints_the_selection_of_the_shared_injector() -> TestResult {
            let index = index();
            let (op_ctx, root) = preview_context(Arc::clone(&index))?;
            let output = run(&op_ctx, PREVIEW_TOKENS).await;
            std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;
            let output = output.map_err(ctx("/skills preview"))?;

            let settings = SkillInjectionSettings::from_config(
                &harw_config::ResolvedConfig::default().harness.skills,
            );
            let expected = SkillInjector::new(index, settings).preview(&input(), 0);
            assert_eq!(expected.round.delivered.len(), 2);

            let data = output.data.ok_or(TestError::Missing("preview data"))?;
            let injections = data["injections"]
                .as_array()
                .ok_or(TestError::Missing("injections"))?;
            assert_eq!(injections.len(), expected.round.delivered.len());
            for (json, delivered) in injections.iter().zip(&expected.round.delivered) {
                assert_eq!(json["skill"], delivered.skill.as_str());
                assert_eq!(json["section"].as_str(), delivered.section.as_deref());
                assert_eq!(json["label"], delivered.label.as_str());
                assert_eq!(json["trigger_kind"], delivered.kind.as_str());
                assert_eq!(json["trigger_key"], delivered.key.as_str());
                assert_eq!(json["bytes"].as_u64(), u64::try_from(delivered.bytes).ok());
                assert_eq!(json["trust"], "instruction");
                assert_eq!(json["sha256"], delivered.sha256.as_str());
            }
            assert_eq!(
                data["budget"]["per_turn_bytes"].as_u64(),
                u64::try_from(expected.budget.per_turn_bytes).ok()
            );
            assert_eq!(data["injection_enabled"], true);
            assert_eq!(data["index"]["source"], "session");
            assert!(
                output.text.contains("Index: Skill-Index der Sitzung"),
                "{}",
                output.text
            );
            assert!(
                data["dropped"].as_array().is_some_and(Vec::is_empty),
                "{data}"
            );
            assert!(
                output
                    .text
                    .contains("- borrow § Cause E0505 — error_code E0505 · "),
                "{}",
                output.text
            );
            assert!(
                output.text.contains("- lint § Clippy — tool shell.exec · "),
                "{}",
                output.text
            );
            assert!(
                output.text.contains("--command cargo test"),
                "{}",
                output.text
            );
            Ok(())
        }

        /// `--budget` ersetzt den Pro-Runde-Deckel; was nicht mehr passt,
        /// steht mit typisiertem Grund unter „Verworfen“ — wie im Injektor.
        #[tokio::test]
        async fn skills_preview_budget_drops_hits_with_typed_reasons() -> TestResult {
            let index = index();
            let (op_ctx, root) = preview_context(Arc::clone(&index))?;
            let mut tokens = PREVIEW_TOKENS.to_vec();
            tokens.insert(1, "260");
            tokens.insert(1, "--budget");
            let output = run(&op_ctx, &tokens).await;
            std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;
            let output = output.map_err(ctx("/skills preview --budget"))?;

            let settings = SkillInjectionSettings {
                per_turn_cap_bytes: 260,
                ..SkillInjectionSettings::from_config(
                    &harw_config::ResolvedConfig::default().harness.skills,
                )
            };
            let expected = SkillInjector::new(index, settings).preview(&input(), 0);
            let data = output.data.ok_or(TestError::Missing("preview data"))?;
            let dropped = data["dropped"]
                .as_array()
                .ok_or(TestError::Missing("dropped"))?;
            assert_eq!(dropped.len(), expected.round.dropped.len());
            assert_eq!(dropped.len(), 1);
            assert_eq!(dropped[0]["skill"], "lint");
            assert_eq!(dropped[0]["reason"], "turn_budget");
            assert_eq!(dropped[0]["reason_text"], "Pro-Turn-Budget überschritten");
            assert_eq!(data["budget"]["per_turn_bytes"], 260);
            assert!(
                output
                    .text
                    .contains("- lint § Clippy — Pro-Turn-Budget überschritten (turn_budget)"),
                "{}",
                output.text
            );
            Ok(())
        }

        #[tokio::test]
        async fn skills_preview_without_a_hit_says_so() -> TestResult {
            let (op_ctx, root) = preview_context(index())?;
            let output = run(&op_ctx, &["preview", "write a poem"]).await;
            std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;
            let output = output.map_err(ctx("/skills preview"))?;
            assert!(
                output.text.contains("Kein Skill getroffen."),
                "{}",
                output.text
            );
            Ok(())
        }

        /// Die Schwelle ist das kleinste Fenster, für das der Turn-Loop
        /// denselben Pro-Runde-Deckel wählt wie die Vorschau (rund 34 000
        /// Token beim Standarddeckel, rund 546 000 bei 65 536 Bytes).
        #[test]
        fn full_cap_window_is_the_smallest_window_that_reaches_the_cap() {
            let defaults = SkillInjectionSettings::from_config(
                &harw_config::ResolvedConfig::default().harness.skills,
            );
            for cap in [256, 1024, 4096, 65_536] {
                let settings = SkillInjectionSettings {
                    per_turn_cap_bytes: cap,
                    ..defaults
                };
                let window = super::super::full_cap_window_tokens(&settings);
                assert_eq!(settings.per_turn_bytes(window), cap, "cap {cap}");
                assert_eq!(settings.per_turn_bytes(0), cap, "cap {cap}");
                if window > 1 {
                    assert!(settings.per_turn_bytes(window - 1) < cap, "cap {cap}");
                }
            }
            let settings = SkillInjectionSettings {
                per_turn_cap_bytes: 4096,
                ..defaults
            };
            assert!((34_000..=34_200).contains(&super::super::full_cap_window_tokens(&settings)));
        }

        /// Die Vorschau nennt die Fenster-Schwelle und die Hinweise des
        /// Index (übersprungene Skills, verworfene Trigger) — der Grund,
        /// warum ein erwarteter Skill nicht trifft.
        #[tokio::test]
        async fn skills_preview_names_the_window_threshold_and_the_index_warnings() -> TestResult {
            let keywords: Vec<String> = (1..=17).map(|n| format!("\"keyword{n:02}\"")).collect();
            let triggers = format!("[triggers]\nkeywords = [{}]\n", keywords.join(", "));
            let files = [
                (
                    "skills/noisy/skill.toml",
                    "name = \"noisy\"\ndescription = \"Use when noisy.\"\n",
                ),
                (
                    "skills/noisy/instructions.md",
                    "# Noisy\n\n## Use\n\nNoise.\n",
                ),
                ("skills/noisy/triggers.toml", triggers.as_str()),
            ];
            let index = Arc::new(SkillIndex::build_with_bundle(&[], &files));
            let warning = "triggers.keywords: 17 Einträge, nur die ersten 16 behalten";
            let (op_ctx, root) = preview_context(index)?;
            let output = run(&op_ctx, &["preview", "write a poem"]).await;
            std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;
            let output = output.map_err(ctx("/skills preview"))?;

            let threshold =
                super::super::full_cap_window_tokens(&SkillInjectionSettings::from_config(
                    &harw_config::ResolvedConfig::default().harness.skills,
                ));
            let text = &output.text;
            assert!(text.contains("Kein Skill getroffen."), "{text}");
            assert!(
                text.contains(&format!("erst ab {threshold} Token Fenster")),
                "{text}"
            );
            assert!(
                text.contains("Index-Hinweise (0 Skill(s) übersprungen, 1 Trigger-Warnung(en)):"),
                "{text}"
            );
            assert!(
                text.contains(&format!("- Warnung: noisy — {warning}")),
                "{text}"
            );
            let data = output.data.ok_or(TestError::Missing("preview data"))?;
            assert_eq!(
                data["budget"]["full_cap_window_tokens"].as_u64(),
                Some(threshold)
            );
            assert_eq!(data["budget"]["window_tokens"], 0);
            assert_eq!(data["index"]["warnings"][0]["skill"], "noisy");
            assert_eq!(data["index"]["warnings"][0]["reason"], warning);
            assert!(
                data["index"]["skipped"]
                    .as_array()
                    .is_some_and(Vec::is_empty),
                "{data}"
            );
            Ok(())
        }

        /// Ein JSON-Aufruf ohne `rest` nennt Verzeichnis bzw. Aufgabentext
        /// in `target`; `validate` und `preview` lesen es dann von dort.
        #[tokio::test]
        async fn skills_validate_and_preview_fall_back_to_the_target_without_rest() -> TestResult {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let dir = temp.path().join("probe");
            write_skill(&dir, None)?;
            let (op_ctx, root) = preview_context(index())?;
            let validate = super::super::skills(
                &op_ctx,
                SkillsArgs {
                    action: Some("validate".to_owned()),
                    target: Some(dir.display().to_string()),
                    ..SkillsArgs::default()
                },
            )
            .await;
            let preview = super::super::skills(
                &op_ctx,
                SkillsArgs {
                    action: Some("preview".to_owned()),
                    target: Some("clippy complains".to_owned()),
                    ..SkillsArgs::default()
                },
            )
            .await;
            std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;
            let validate = validate.map_err(ctx("/skills validate via target"))?;
            assert!(
                validate.text.contains("Ergebnis: gültig"),
                "{}",
                validate.text
            );
            let preview = preview.map_err(ctx("/skills preview via target"))?;
            let data = preview.data.ok_or(TestError::Missing("preview data"))?;
            assert_eq!(data["input"]["task"], "clippy complains");
            assert_eq!(data["injections"][0]["skill"], "lint");
            Ok(())
        }

        /// Ohne injizierten Index und ohne gebundenen Root-Space ist die
        /// Vorschau nicht verfügbar, statt auf `HARW_HOME` auszuweichen.
        #[tokio::test]
        async fn skills_preview_without_index_or_bound_home_is_not_available() -> TestResult {
            let mut services = ServiceMap::new();
            services.insert(Arc::new(harw_config::ResolvedConfig::default()));
            let (op_ctx, root) = context_with_services(services)?;
            let result = run(&op_ctx, &["preview", "cargo test"]).await;
            std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;
            assert!(
                matches!(result, Err(OpError::NotAvailable(_))),
                "{result:?}"
            );
            Ok(())
        }

        /// Ohne injizierten Index baut die Vorschau ihn wie die Laufzeit-Montage
        /// über die vertrauten Layer — samt Projekt-Einstellungsverzeichnis,
        /// sobald es `settings.toml` hat — und nennt diese Quelle.
        #[tokio::test]
        async fn skills_preview_without_an_index_rebuilds_it_from_the_trusted_layers() -> TestResult
        {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let home = crate::config_util::test_home_context(temp.path())?;
            let settings_dir = home
                .project_settings_path
                .parent()
                .ok_or(TestError::Missing("project settings dir"))?
                .to_path_buf();
            let skill_dir = settings_dir.join("skills").join("probe");
            write_skill(
                &skill_dir,
                Some("[triggers]\nkeywords = [\"probe layer\"]\n"),
            )?;
            let mut services = ServiceMap::new();
            services.insert(Arc::new(harw_config::ResolvedConfig::default()));
            services.insert(Arc::clone(&home));
            let (op_ctx, root) = context_with_services(services)?;
            let tokens = ["preview", "please probe layer settings"];

            // Ohne `settings.toml`/`config.toml` ist das Verzeichnis (wie in
            // der Laufzeit) kein Layer.
            let without = run(&op_ctx, &tokens).await;
            let written = std::fs::write(settings_dir.join("settings.toml"), "");
            let with = run(&op_ctx, &tokens).await;
            std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;
            written.map_err(ctx("write settings.toml"))?;

            let settings_layer = settings_dir.display().to_string();
            let injected = |data: &serde_json::Value| {
                data["injections"]
                    .as_array()
                    .is_some_and(|items| items.iter().any(|item| item["skill"] == "probe"))
            };
            let layers = |data: &serde_json::Value| -> Vec<String> {
                data["index"]["layers"]
                    .as_array()
                    .map(|layers| {
                        layers
                            .iter()
                            .filter_map(|layer| layer.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default()
            };

            let without = without.map_err(ctx("/skills preview without settings"))?;
            let data = without.data.ok_or(TestError::Missing("preview data"))?;
            assert_eq!(data["index"]["source"], "layers");
            assert!(!layers(&data).contains(&settings_layer), "{data}");
            assert!(!injected(&data), "{data}");

            let with = with.map_err(ctx("/skills preview with settings"))?;
            assert!(
                with.text
                    .contains("Index: neu gebaut wie in der Laufzeit-Montage"),
                "{}",
                with.text
            );
            let data = with.data.ok_or(TestError::Missing("preview data"))?;
            assert_eq!(data["index"]["source"], "layers");
            assert_eq!(layers(&data).last(), Some(&settings_layer), "{data}");
            assert!(injected(&data), "{data}");
            Ok(())
        }

        #[test]
        fn skills_preview_and_validate_are_immediate_but_not_channel_reachable() {
            let busy = SkillsOperation.busy_subcommands();
            for sub in ["preview", "validate"] {
                assert!(
                    busy.iter().any(|entry| entry.subcommand == Some(sub)
                        && entry.busy == BusyAvailability::Immediate),
                    "{sub} must be immediate"
                );
                assert!(!SkillsOperation.channel_subcommands().contains(&sub));
            }
        }

        fn write_skill(dir: &Path, triggers: Option<&str>) -> TestResult {
            std::fs::create_dir_all(dir).map_err(ctx("create skill dir"))?;
            std::fs::write(
                dir.join("skill.toml"),
                "name = \"probe\"\ndescription = \"Use when probing.\"\n",
            )
            .map_err(ctx("write skill.toml"))?;
            std::fs::write(dir.join("instructions.md"), "# Probe\n\n## Use\n\nProbe.\n")
                .map_err(ctx("write instructions.md"))?;
            if let Some(triggers) = triggers {
                std::fs::write(dir.join("triggers.toml"), triggers)
                    .map_err(ctx("write triggers.toml"))?;
            }
            Ok(())
        }

        fn seventeen_keywords() -> String {
            let keywords: Vec<String> = (1..=17).map(|n| format!("\"keyword{n:02}\"")).collect();
            format!(
                "when = \"Probing\"\n[triggers]\nkeywords = [{}]\n",
                keywords.join(", ")
            )
        }

        /// Eine `triggers.toml` mit 17 Schlüsselwörtern wird beanstandet, mit
        /// genau der Warnung aus `harw_catalog::parse_triggers_file`.
        #[tokio::test]
        async fn skills_validate_rejects_seventeen_keywords_with_the_catalog_warning() -> TestResult
        {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let dir = temp.path().join("probe");
            let source = seventeen_keywords();
            write_skill(&dir, Some(&source))?;

            let (_, catalog_warnings) = harw_catalog::parse_triggers_file(&source);
            assert_eq!(
                catalog_warnings,
                vec!["triggers.keywords: 17 Einträge, nur die ersten 16 behalten".to_owned()]
            );

            let report = validate_skill_dir(&dir);
            assert!(!report.is_valid());
            assert!(report.errors.is_empty(), "{:?}", report.errors);
            assert_eq!(report.warnings, catalog_warnings);
            assert_eq!(
                report
                    .triggers
                    .as_ref()
                    .map(|spec| spec.triggers.keywords.len()),
                Some(16)
            );

            let (op_ctx, root) = context_with_services(ServiceMap::new())?;
            let dir_token = dir.display().to_string();
            let output = run(&op_ctx, &["validate", dir_token.as_str()]).await;
            std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;
            let output = output.map_err(ctx("/skills validate"))?;
            assert!(
                output.text.contains(
                    "Warnung: triggers.keywords: 17 Einträge, nur die ersten 16 behalten"
                ),
                "{}",
                output.text
            );
            assert!(
                output.text.contains("Ergebnis: zu beanstanden"),
                "{}",
                output.text
            );
            let data = output.data.ok_or(TestError::Missing("validate data"))?;
            assert_eq!(data["valid"], false);
            assert_eq!(data["skill"], "probe");
            assert_eq!(
                data["warnings"][0],
                "triggers.keywords: 17 Einträge, nur die ersten 16 behalten"
            );
            Ok(())
        }

        #[test]
        fn skills_validate_accepts_a_clean_dir_and_reports_broken_manifests() -> TestResult {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let clean = temp.path().join("clean");
            write_skill(
                &clean,
                Some("when = \"Probing\"\n[triggers]\nkeywords = [\"probe it\"]\n"),
            )?;
            let report = validate_skill_dir(&clean);
            assert!(report.is_valid(), "{report:?}");
            assert!(report.render().contains("Ergebnis: gültig"));
            assert!(report.render().contains("keywords 1"));

            let untriggered = temp.path().join("untriggered");
            write_skill(&untriggered, None)?;
            let report = validate_skill_dir(&untriggered);
            assert!(report.is_valid(), "{report:?}");
            assert!(report.triggers.is_none());

            let broken = temp.path().join("broken");
            std::fs::create_dir_all(&broken).map_err(ctx("create broken dir"))?;
            std::fs::write(broken.join("skill.toml"), "name = \"x\"\nunknown = 1\n")
                .map_err(ctx("write broken skill.toml"))?;
            let report = validate_skill_dir(&broken);
            assert!(!report.is_valid());
            assert!(
                report
                    .errors
                    .first()
                    .is_some_and(|error| error.starts_with("skill.toml ungültig: ")),
                "{report:?}"
            );

            let missing = validate_skill_dir(&temp.path().join("missing"));
            assert!(!missing.is_valid());
            assert!(
                missing
                    .errors
                    .iter()
                    .any(|error| error.contains("kein Verzeichnis"))
            );
            Ok(())
        }

        /// `skill.toml` muss eine reguläre Datei sein: ein Verzeichnis, eine
        /// Gerätedatei hinter einem Symlink oder eine Riesendatei wird nicht
        /// gelesen, sondern als Fehler gemeldet — `validate` kehrt sofort
        /// zurück.
        #[test]
        fn skills_validate_reads_only_a_bounded_regular_skill_manifest() -> TestResult {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let assert_error = |dir: &Path, needle: &str| {
                let report = validate_skill_dir(dir);
                assert!(!report.is_valid(), "{report:?}");
                assert!(
                    report.errors.iter().any(|error| error.contains(needle)),
                    "{needle}: {report:?}"
                );
                assert!(report.name.is_none(), "{report:?}");
            };

            let missing = temp.path().join("missing-manifest");
            std::fs::create_dir_all(&missing).map_err(ctx("create dir"))?;
            assert_error(&missing, "skill.toml fehlt");

            let nested = temp.path().join("dir-manifest");
            std::fs::create_dir_all(nested.join("skill.toml")).map_err(ctx("create dir"))?;
            assert_error(&nested, "skill.toml ist keine Datei");

            let huge = temp.path().join("huge-manifest");
            std::fs::create_dir_all(&huge).map_err(ctx("create dir"))?;
            let mut body = "name = \"huge\"\n".to_owned();
            body.push_str(&format!(
                "# {}\n",
                "x".repeat(super::super::MAX_SKILL_MANIFEST_BYTES)
            ));
            std::fs::write(huge.join("skill.toml"), body).map_err(ctx("write skill.toml"))?;
            assert_error(&huge, "skill.toml größer als 65536 Bytes");

            #[cfg(unix)]
            {
                let device = temp.path().join("device-manifest");
                std::fs::create_dir_all(&device).map_err(ctx("create dir"))?;
                std::os::unix::fs::symlink("/dev/zero", device.join("skill.toml"))
                    .map_err(ctx("symlink /dev/zero"))?;
                assert_error(&device, "skill.toml ist keine Datei");
            }
            Ok(())
        }

        /// Ein Skill-Verzeichnis, das der Index in keinem Layer liest — ein
        /// Symlink oder ein Name mit führendem '.' —, ist ein Fehler, auch
        /// wenn sein Inhalt gültig ist; dasselbe Ziel über seinen echten
        /// Pfad ist gültig.
        #[test]
        fn skills_validate_rejects_symlinked_and_hidden_skill_dirs() -> TestResult {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let real = temp.path().join("probe");
            write_skill(&real, None)?;
            assert!(validate_skill_dir(&real).is_valid());

            let hidden = temp.path().join(".probe");
            write_skill(&hidden, None)?;
            let report = validate_skill_dir(&hidden);
            assert!(!report.is_valid());
            assert!(
                report
                    .errors
                    .iter()
                    .any(|error| error.contains("beginnt mit '.'")),
                "{report:?}"
            );
            assert_eq!(report.name.as_deref(), Some("probe"));

            #[cfg(unix)]
            {
                let link = temp.path().join("linked");
                std::os::unix::fs::symlink(&real, &link).map_err(ctx("symlink skill dir"))?;
                let report = validate_skill_dir(&link);
                assert!(!report.is_valid());
                assert!(
                    report.errors.iter().any(|error| error.contains("Symlink")),
                    "{report:?}"
                );
            }
            Ok(())
        }

        /// `validate` und der Index lesen `triggers.toml` über dieselbe
        /// Funktion: eine nicht reguläre Datei ergibt dieselbe Warnung.
        #[test]
        fn skills_validate_reports_the_index_warning_for_a_non_file_triggers_toml() -> TestResult {
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let dir = temp.path().join("probe");
            write_skill(&dir, None)?;
            std::fs::create_dir_all(dir.join("triggers.toml")).map_err(ctx("create dir"))?;
            let expected = match harw_catalog::read_skill_triggers(&dir) {
                Some(Err(reason)) => reason,
                other => {
                    return Err(TestError::Unexpected(format!(
                        "expected a discarded triggers.toml, got {other:?}"
                    )));
                }
            };
            let report = validate_skill_dir(&dir);
            assert!(report.errors.is_empty(), "{report:?}");
            assert_eq!(report.warnings, vec![expected]);
            assert!(report.triggers.is_none());
            Ok(())
        }

        #[tokio::test]
        async fn skills_validate_needs_exactly_one_directory() -> TestResult {
            let (op_ctx, root) = context_with_services(ServiceMap::new())?;
            let missing = run(&op_ctx, &["validate"]).await;
            let extra = run(&op_ctx, &["validate", "a", "b"]).await;
            std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;
            assert!(
                matches!(&missing, Err(OpError::InvalidArguments(message)) if message.contains("requires a skill directory")),
                "{missing:?}"
            );
            assert!(
                matches!(&extra, Err(OpError::InvalidArguments(message)) if message.contains("exactly one")),
                "{extra:?}"
            );
            Ok(())
        }

        /// Ein relatives Verzeichnis gilt relativ zur Workspace-Wurzel.
        #[tokio::test]
        async fn skills_validate_resolves_relative_dirs_against_the_workspace() -> TestResult {
            let (op_ctx, root) = context_with_services(ServiceMap::new())?;
            let workspace = op_ctx.sandbox().workspace().canonical_root().to_path_buf();
            let written = write_skill(&workspace.join("skills").join("probe"), None);
            let output = run(&op_ctx, &["validate", "skills/probe"]).await;
            std::fs::remove_dir_all(root).map_err(ctx("remove test workspace"))?;
            written?;
            let output = output.map_err(ctx("/skills validate"))?;
            assert!(output.text.contains("Ergebnis: gültig"), "{}", output.text);
            Ok(())
        }
    }
}
