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
//!   vertrauten Config-Layer (`harw_home::config_layers`).
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
//! geladene Konfiguration den Skill tatsächlich bezieht. Nicht vertraute
//! Repo-Layer liefert `harw_home::config_layers` gar nicht erst.
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
/// [`harw_operations::context::ServiceMap`] ab und fällt ohne registrierten
/// Dienst auf [`LayeredSkillStatePersistence::from_home`] zurück. Tests und
/// Oberflächen mit eigener Config-Wurzel injizieren so eine eigene
/// Implementierung, ohne `HARW_HOME` zu verändern.
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
/// let persistence = LayeredSkillStatePersistence::new(vec!["/home/mia/.harw".into()]);
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

    /// Erstellt die Persistenz über die vertrauten Layer des aktiven `HARW_HOME`.
    ///
    /// # Fehler
    /// - [`OpError::Execution`]: Home oder Layer nicht auflösbar.
    pub fn from_home() -> Result<Self, OpError> {
        let home = harw_home::home_dir()
            .map_err(|error| OpError::Execution(format!("HARW_HOME nicht auflösbar: {error}")))?;
        let layers = harw_home::config_layers(&home).map_err(|error| {
            OpError::Execution(format!("Config-Layer nicht auflösbar: {error}"))
        })?;
        Ok(Self::new(layers))
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

/// Liefert den injizierten Persistenzdienst oder die Layer-Standardimplementierung.
///
/// # Fehler
/// - [`OpError::Execution`]: kein Dienst registriert und `HARW_HOME`/Layer
///   nicht auflösbar.
fn skill_state_persistence(ctx: &OpContext) -> Result<Arc<dyn SkillStatePersistence>, OpError> {
    if let Some(persistence) = ctx.service::<Arc<dyn SkillStatePersistence>>() {
        return Ok(Arc::clone(persistence));
    }
    Ok(Arc::new(LayeredSkillStatePersistence::from_home()?) as Arc<dyn SkillStatePersistence>)
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
/// - `ctx` (`&OpContext`): Ausführungskontext; benötigt `Arc<ResolvedConfig>`,
///   optional `Arc<dyn SkillStatePersistence>`.
/// - `args` (`SkillsArgs`): Typisierte Sub-Kommando-Argumente.
///
/// # Rückgabe
/// - `Ok(OpOutput)`: menschenlesbare Ausgabe.
///
/// # Fehler
/// - [`OpError::NotAvailable`]: kein `Arc<ResolvedConfig>`-Service (statische
///   Meldung ohne Echo der Argumente).
/// - [`OpError::InvalidArguments`]: fehlender/unbekannter Skill-Name oder
///   unbekanntes Sub-Kommando.
/// - [`OpError::Execution`]: Persistenzfehler bei `activate`/`deactivate`.
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
    summary = "Skill-Katalog: list/show/activate/deactivate gegen ResolvedConfig.skills.",
    domain = "catalog_config",
    permission = "operator",
    command(path = "/skills", visibility = "channel_reduced")
)]
async fn skills(ctx: &OpContext, args: SkillsArgs) -> Result<OpOutput, OpError> {
    let Some(config) = ctx.service::<Arc<ResolvedConfig>>() else {
        return Err(OpError::NotAvailable(
            "skill catalog integration is not available".to_owned(),
        ));
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
}
