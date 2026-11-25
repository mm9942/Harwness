//! PL-100 WP 1.5: der Skill-Injektor, den eine [`ChildRegistryFactory`] über
//! [`ChildSessionObservers::skill_injector`] liefert, erreicht die
//! Kind-Sitzung, geschnitten mit der Kind-Aktivierung.
//!
//! Die Tests fahren den echten Admissionsweg (`admit` unter einer externen
//! Wurzel) mit einer Fabrik, deren Registry `fs.read`, `fs.write` und
//! `sys.ps` bedient. Ihr Injektor führt alle drei Werkzeuge sowie die
//! abstrakten Fähigkeits-Labels `filesystem.read` (gedeckt von `fs.read`),
//! `filesystem.write` (gedeckt von `fs.write`, all-of) und das ungedeckte
//! `notes.lookup` als aufrufbar; die Deckung liefert sie in
//! [`ChildSessionObservers::skill_label_backing`].
//!
//! Sperrt die Aktivierung der Wurzel ein bedientes Werkzeug, ist sein Skill
//! in der Kind-Sitzung nicht mehr wählbar. Ein Label überlebt nur, solange
//! eines seiner deckenden Werkzeuge überlebt (Review-Befund: ein Kind, dem
//! die Wurzel `fs.write` sperrt, bekommt keinen Skill für
//! `filesystem.write`). Ein ungedeckter Name, den die Kind-Registry nicht
//! bedient, bleibt, wie die Fabrik ihn gesetzt hat.

use harw_catalog::SkillIndex;
use harw_extension_api::{ToolExecutor, ToolProvider, ToolSpec};
use harw_tools::{FunctionToolSpec, JsonSchema};

use super::*;
use crate::skill_injection::{SkillInjectionSettings, SkillInjector, ToolAvailability};

/// Die Werkzeuge der Kind-Registry.
const SERVED_TOOLS: [&str; 3] = ["fs.read", "fs.write", "sys.ps"];

/// Abstraktes Lese-Label, gedeckt von `fs.read`.
const READ_LABEL: &str = "filesystem.read";

/// Abstraktes Schreib-Label, gedeckt von `fs.write`; all-of
/// ([`ToolAvailability::with_required`]).
const WRITE_LABEL: &str = "filesystem.write";

/// Abstraktes Label ohne Deckung: die Fabrik führt es als aufrufbar, nennt
/// aber kein Werkzeug dazu.
const UNBACKED_LABEL: &str = "notes.lookup";

/// Die Skills des Index als `(Name, deklarierte Werkzeugnamen)`.
const SKILLS: [(&str, &[&str]); 6] = [
    ("reader", &["fs.read"]),
    ("process-lister", &["sys.ps"]),
    ("labelled-reader", &[READ_LABEL]),
    ("labelled-writer", &[WRITE_LABEL]),
    ("labelled-editor", &[READ_LABEL, WRITE_LABEL]),
    ("noted", &[UNBACKED_LABEL]),
];

/// Ein Index über einem In-Memory-Bündel: je Skill ein Manifest, ein Text
/// und ein `triggers.toml`, das genau seine Werkzeugnamen deklariert.
fn skill_index() -> Arc<SkillIndex> {
    let files: Vec<(String, String)> = SKILLS
        .iter()
        .flat_map(|(name, tools)| {
            let declared = tools
                .iter()
                .map(|tool| format!("\"{tool}\""))
                .collect::<Vec<_>>()
                .join(", ");
            [
                (
                    format!("skills/{name}/skill.toml"),
                    format!("name = \"{name}\"\ndescription = \"Use when {name} matters.\"\n"),
                ),
                (
                    format!("skills/{name}/instructions.md"),
                    format!("# {name}\n\nFollow the {name} steps.\n"),
                ),
                (
                    format!("skills/{name}/triggers.toml"),
                    format!("[triggers]\ntools = [{declared}]\n"),
                ),
            ]
        })
        .collect();
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_str()))
        .collect();
    Arc::new(SkillIndex::build_with_bundle(&[], &refs))
}

/// Der Injektor, wie ihn die Fabrik baut: alle bedienten Werkzeuge und alle
/// drei Labels sind registriert (aufrufbar) und bekannt; [`WRITE_LABEL`]
/// gilt all-of.
fn factory_injector(index: &Arc<SkillIndex>) -> SkillInjector {
    let callable: Vec<&str> = SERVED_TOOLS
        .into_iter()
        .chain([READ_LABEL, WRITE_LABEL, UNBACKED_LABEL])
        .collect();
    SkillInjector::new(Arc::clone(index), SkillInjectionSettings::default()).with_tool_availability(
        ToolAvailability::new(callable.clone(), callable).with_required([WRITE_LABEL]),
    )
}

/// Die Deckung der Labels, wie sie die Fabrik übergibt; [`UNBACKED_LABEL`]
/// fehlt bewusst.
fn label_backing() -> BTreeMap<String, BTreeSet<String>> {
    [(READ_LABEL, "fs.read"), (WRITE_LABEL, "fs.write")]
        .into_iter()
        .map(|(label, tool)| (label.to_owned(), BTreeSet::from([tool.to_owned()])))
        .collect()
}

/// Prüft, dass `skill` im Index des Injektors steht (sonst wäre
/// `tools_admit` für einen unbekannten Namen trivial `true`).
fn assert_indexed(injector: &SkillInjector, skill: &str) -> TestResult {
    injector
        .index()
        .get(skill)
        .map(|_| ())
        .ok_or_else(|| TestError::Unexpected(format!("skill {skill} is not indexed")))
}

/// Die Skills, die `injector` zulässt, in der Reihenfolge von [`SKILLS`];
/// prüft dabei, dass jeder Skill indiziert ist.
fn admitted_skills(injector: &SkillInjector) -> TestResult<Vec<&'static str>> {
    let mut admitted = Vec::new();
    for (skill, _) in SKILLS {
        assert_indexed(injector, skill)?;
        if injector.tools_admit(skill) {
            admitted.push(skill);
        }
    }
    Ok(admitted)
}

/// Bedient [`SERVED_TOOLS`] ohne Ausführer; es zählt nur die Fläche.
struct ServedTools;

impl ToolProvider for ServedTools {
    fn tools(&self) -> Vec<ToolSpec> {
        SERVED_TOOLS
            .iter()
            .map(|name| {
                ToolSpec::Function(FunctionToolSpec {
                    name: ToolName::new(*name),
                    description: "Testwerkzeug".to_owned(),
                    parameters: JsonSchema::default(),
                    strict: false,
                })
            })
            .collect()
    }

    fn executor(&self, _name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        None
    }
}

/// Fabrik mit der Registry [`ServedTools`]; je Admission liefert sie einen
/// frischen [`factory_injector`] samt [`label_backing`] als Beobachter der
/// Kind-Sitzung.
struct InjectingChildRegistry {
    index: Arc<SkillIndex>,
}

impl ChildRegistryFactory for InjectingChildRegistry {
    fn build_registry(
        &self,
        _role: &str,
        _input: &SpawnInput,
        _suggestions: Option<&AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        Ok(ExtensionRegistryBuilder::default()
            .tool_provider(Arc::new(ServedTools))
            .build())
    }

    fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        Err(AgentSpawnError {
            kind: Default::default(),
            message: "test registry does not run children".to_owned(),
        })
    }

    fn child_session_observers(&self, _role: &str, _child: &SessionId) -> ChildSessionObservers {
        ChildSessionObservers {
            skill_injector: Some(Arc::new(factory_injector(&self.index))),
            skill_label_backing: label_backing(),
            ..ChildSessionObservers::default()
        }
    }
}

/// Lässt ein `worker`-Kind unter einer externen Wurzel mit der Aktivierung
/// `root_activation` zu und liefert den Skill-Injektor seiner Sitzung.
fn admitted_injector(root_activation: SessionActivation) -> TestResult<Arc<SkillInjector>> {
    let (events, _receiver) = mpsc::unbounded_channel();
    let manager = Arc::new(Mutex::new(SessionManager::new(events)));
    let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
    let parent = SessionId::new();
    let spawner = ManagedAgentSpawner::new(Arc::clone(&manager), ChildLimits::conservative())
        .with_role(
            "worker",
            AgentRole::Agent {
                name: "worker".to_owned(),
            },
            harw_agent_dsl::roles::AgentRoleId::Worker,
            Arc::new(InjectingChildRegistry {
                index: skill_index(),
            }),
        )
        .with_external_root_parent(
            parent.clone(),
            external_root_context(
                sandbox.clone(),
                harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
            ),
            None,
            root_activation,
        )
        .map_err(ctx("trusted external root registers during construction"))?;
    let child = spawner
        .admit("worker", spawn_input(parent), sandbox, None)
        .map_err(ctx("child is admitted"))?;
    let guard = manager
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let session = guard.get(&child).map_err(ctx("child is manager-owned"))?;
    let injector = session.skill_injector().cloned().ok_or(TestError::Missing(
        "the child session carries the factory injector",
    ))?;
    Ok(injector)
}

/// Eine Wurzel mit Profil `Full`, die genau `disabled` sperrt.
fn full_root_without(disabled: &[&str]) -> SessionActivation {
    let mut root = SessionActivation::default();
    for tool in disabled {
        root.disable_tool(ToolName::new(*tool));
    }
    root
}

/// Die Wurzel sperrt `sys.ps`: der Injektor erreicht die Kind-Sitzung,
/// aber ein Skill, der nur `sys.ps` deklariert, ist dort nicht mehr
/// wählbar; alle anderen bleiben. Gegenprobe: der ungeschnittene Injektor
/// der Fabrik lässt alle zu.
#[test]
fn a_tool_disabled_by_the_parent_activation_makes_its_skill_unavailable() -> TestResult {
    let every_skill: Vec<&str> = SKILLS.iter().map(|(skill, _)| *skill).collect();
    assert_eq!(
        admitted_skills(&factory_injector(&skill_index()))?,
        every_skill
    );

    let injector = admitted_injector(full_root_without(&["sys.ps"]))?;
    assert_eq!(
        admitted_skills(&injector)?,
        [
            "reader",
            "labelled-reader",
            "labelled-writer",
            "labelled-editor",
            "noted",
        ],
        "sys.ps is disabled by the parent activation"
    );
    Ok(())
}

/// Ohne Schnitt der Wurzel (`SessionActivation::default()`, Profil `Full`)
/// bleibt jeder Skill des Fabrik-Injektors wählbar: der Controller nimmt
/// nur weg, was die Kind-Aktivierung sperrt.
#[test]
fn an_uncut_child_keeps_every_skill_of_the_factory_injector() -> TestResult {
    let injector = admitted_injector(SessionActivation::default())?;
    let every_skill: Vec<&str> = SKILLS.iter().map(|(skill, _)| *skill).collect();
    assert_eq!(admitted_skills(&injector)?, every_skill);
    Ok(())
}

/// Review-Befund: die Wurzel sperrt das einzige Schreibwerkzeug `fs.write`
/// (wie `/tools off fs.write`), die Sandbox trägt das Schreibrecht aber
/// weiter. Das Label `filesystem.write` verliert damit seine Deckung und
/// fällt weg: weder `labelled-writer` noch `labelled-editor` (all-of, auch
/// nicht über `filesystem.read`) sind wählbar; das Lese-Label bleibt.
#[test]
fn disabling_the_only_write_tool_makes_write_label_skills_unavailable() -> TestResult {
    let injector = admitted_injector(full_root_without(&["fs.write"]))?;
    assert_eq!(
        admitted_skills(&injector)?,
        ["reader", "process-lister", "labelled-reader", "noted"]
    );
    Ok(())
}

/// Ein Label überlebt nur, solange eines seiner deckenden Werkzeuge
/// überlebt: eine Wurzel mit Profil `Minimal`, die nur `fs.read`
/// freischaltet, lässt `filesystem.read` stehen und nimmt
/// `filesystem.write` (gedeckt vom nicht freigeschalteten `fs.write`)
/// sowie das bediente `sys.ps` weg. Sperrt die Wurzel stattdessen
/// `fs.read`, fällt `filesystem.read` mit ihm; `labelled-editor` bleibt
/// dann über das weiter gedeckte `filesystem.write` (das Lese-Label gilt
/// any-of). Das ungedeckte `notes.lookup` bedient keine Registry; die
/// Aktivierung entscheidet darüber nicht, obwohl sie es nicht freischaltet,
/// und es bleibt.
#[test]
fn a_label_survives_only_while_one_of_its_backing_tools_does() -> TestResult {
    let mut minimal = SessionActivation::new(ToolProfile::Minimal);
    minimal.enable_tool(ToolName::new("fs.read"));
    assert!(
        !minimal.is_tool_enabled(&ToolName::new(UNBACKED_LABEL)),
        "precondition: the activation does not enable the unbacked label"
    );
    let injector = admitted_injector(minimal)?;
    assert_eq!(
        admitted_skills(&injector)?,
        ["reader", "labelled-reader", "noted"]
    );

    let injector = admitted_injector(full_root_without(&["fs.read"]))?;
    assert_eq!(
        admitted_skills(&injector)?,
        [
            "process-lister",
            "labelled-writer",
            "labelled-editor",
            "noted"
        ]
    );
    Ok(())
}
