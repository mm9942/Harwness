//! Generated delegation tools and background handoff.

use super::*;

/// Liest den angezeigten Auftrag eines Handoff-Calls aus dessen Argumenten.
///
/// # Beschreibung
/// Zuerst zählt ein nicht leerer JSON-String unter `"task"` (die heutige
/// Schreibweise der Handoff-Werkzeuge), danach einer unter `"question"` (die
/// ältere). Jede andere Form (Objekt, Zahl, fehlender Schlüssel, leerer
/// Text) ergibt `None`: die Argumente stammen vom Modell, und ein
/// Ereignisfeld darf keinen Wert behaupten, den das Modell so nicht geliefert
/// hat. Ohne diese Reihenfolge zeigte die Oberfläche für jeden Handoff mit
/// `task` „(kein Auftrag angegeben)".
///
/// # Arguments
/// - `arguments` (`&serde_json::Value`): die Argumente des Handoff-Calls.
///
/// # Returns
/// `Some(auftrag)` als eigener `String`, sonst `None`.
pub(super) fn child_question(arguments: &serde_json::Value) -> Option<String> {
    ["task", "question"].iter().find_map(|field| {
        arguments
            .get(*field)
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(ToOwned::to_owned)
    })
}

/// Error text for the retired `background:false` / `wait:true` delegation
/// parameters. Old transcripts and models may still send them; they are
/// refused with an explanation instead of being silently reinterpreted.
pub(super) const LEGACY_INLINE_DELEGATION_ERROR: &str = "Delegation is background-only: the parameters \
`background:false` and `wait:true` no longer exist. Starting an agent returns immediately with \
its ids; the result and intermediate states are delivered to you automatically (notifications / \
auto-turn). Do not wait or poll. Repeat the call without these parameters and continue with \
other work.";

/// Error text when the session has no background executor for delegation.
pub(super) const NO_BACKGROUND_EXECUTOR_ERROR: &str = "Delegation is background-only and no background \
executor is configured in this session (no agent job submitter is registered and the host \
declared no in-process background launcher), so the agent was not started. There is no \
foreground fallback; delegation is unavailable here. Continue without delegating.";

/// Delegation is always background. Returns the tool error for a start that is
/// not admissible: a legacy inline request (`background:false` / `wait:true`)
/// or a session without an agent job submitter. Checked before the child is
/// spawned so a refused start never leaks a child session.
pub(super) fn background_only_refusal(
    session: &AgentSession,
    arguments: &serde_json::Value,
) -> Option<ToolCallResult> {
    let flag = |field: &str| arguments.get(field).and_then(serde_json::Value::as_bool);
    if flag("background") == Some(false) || flag("wait") == Some(true) {
        return Some(ToolCallResult::error(LEGACY_INLINE_DELEGATION_ERROR));
    }
    if session.registry().agent_job_submitter().is_none() && !session.host_background_launcher() {
        return Some(ToolCallResult::error(NO_BACKGROUND_EXECUTOR_ERROR));
    }
    None
}

/// Hands a freshly admitted child to the background executor.
///
/// # Returns
/// `Some(result)`: the tool result to record (the one-time start confirmation,
/// or a detailed error after the child was released because the submit
/// failed; never an `Err` that would abort the turn and leak the child).
/// `None`: no durable submitter, but the host declared an in-process launcher;
/// the caller yields `AwaitingChild` for the host to detach (never inline).
pub(super) async fn detach_admitted_child(
    session: &AgentSession,
    spawner: &Arc<dyn harw_extension_api::AgentSpawner>,
    child: &SessionId,
    role: &str,
    question: Option<&str>,
) -> Option<ToolCallResult> {
    let Some(submitter) = session.registry().agent_job_submitter().cloned() else {
        if session.host_background_launcher() {
            return None;
        }
        spawner.child_finished(child);
        return Some(ToolCallResult::error(NO_BACKGROUND_EXECUTOR_ERROR));
    };
    match submitter.submit_child(child, question).await {
        Ok(job) => Some(agent_job_started_result(&job, role)),
        Err(error) => {
            tracing::warn!(child = %child, role, error = %error, "turn.delegation.submit_failed");
            spawner.child_finished(child);
            Some(ToolCallResult::error(format!(
                "The '{role}' agent was admitted but could not be started in the background: \
                 {error}. The child was released and nothing is running for it. Retry the \
                 delegation, or continue without it."
            )))
        }
    }
}

/// One-time confirmation returned to the model when a background agent started.
pub(super) fn agent_job_started_result(
    handle: &harw_extension_api::AgentJobHandle,
    role: &str,
) -> ToolCallResult {
    ToolCallResult::success(serde_json::json!({
        "work_id": handle.work_id.as_str(),
        "child_id": handle.child.as_str(),
        "role": role,
        "status": "started",
        "async": true,
        "hint": "Started in the background. The result and intermediate states are delivered to you automatically; do not wait or poll. Continue with other work."
    }))
}

/// Argumentfelder eines Handoff-Calls, die — in dieser Reihenfolge — als
/// Arbeitsauftrag des Kindes gelesen werden. `"question"` ist die ältere
/// Schreibweise (siehe [`child_question`]).
pub(super) const HANDOFF_TASK_FIELDS: [&str; 4] = ["task", "instructions", "objective", "question"];

/// Leitet den Arbeitsauftrag (`SpawnInput::instructions`) eines Handoff-Calls
/// aus dessen Argumenten ab.
///
/// # Beschreibung
/// Das erste nicht-leere String-Feld aus [`HANDOFF_TASK_FIELDS`] ist der
/// Auftrag; ein zusätzliches nicht-leeres String-Feld `"context"` wird als
/// eigener Absatz angehängt, damit das Kind den Kontext in seinem ersten
/// Nutzer-Turn tatsächlich sieht. Fehlt ein solches Feld, wird das gesamte
/// Argument-JSON als Text übergeben (ein nackter JSON-String als sein Inhalt).
/// `null`, ein leeres Objekt und ein leerer String tragen keinen Auftrag und
/// ergeben `None` — der Spawner fällt dann auf `SpawnInput::context` zurück.
///
/// # Arguments
/// - `arguments` (`&serde_json::Value`): die vom Modell gelieferten Argumente.
///
/// # Returns
/// `Some(auftrag)` oder `None`, wenn die Argumente nichts Verwertbares tragen.
pub(super) fn handoff_instructions(arguments: &serde_json::Value) -> Option<String> {
    // Runde 9, E4: vorab erteilte Freigaben der Nutzerin führen den Auftrag an.
    crate::user_approval::with_user_approval(handoff_task_text(arguments), arguments)
}

/// Der Auftragstext eines Handoff-Calls ohne Freigabe-Zeile (siehe
/// [`handoff_instructions`]).
pub(super) fn handoff_task_text(arguments: &serde_json::Value) -> Option<String> {
    let non_empty_field = |field: &str| {
        arguments
            .get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    if let Some(task) = HANDOFF_TASK_FIELDS
        .iter()
        .find_map(|field| non_empty_field(field))
    {
        return Some(match non_empty_field("context") {
            Some(context) => format!("{task}\n\nKontext:\n{context}"),
            None => task.to_owned(),
        });
    }
    match arguments {
        serde_json::Value::Null => None,
        serde_json::Value::Object(map) if map.is_empty() => None,
        serde_json::Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_owned())
        }
        other => Some(other.to_string()),
    }
}

/// Prüft, ob ein Rollenname als Suffix eines Handoff-Werkzeugnamens taugt.
///
/// Provider verlangen Werkzeugnamen aus `[A-Za-z0-9_-]` mit höchstens 64
/// Zeichen; ein Name außerhalb davon würde den ganzen Request ungültig
/// machen und wird deshalb nicht als Werkzeug angeboten (der textuelle
/// Delegationsblock nennt ihn weiterhin).
pub(super) fn is_valid_handoff_role(role: &str) -> bool {
    !role.is_empty()
        && HANDOFF_PREFIX.len() + role.len() <= 64
        && role
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Rollenname des Host-Shell-Arms der UIA, der Root-Befehle über
/// `host.sudo_exec` anfragen darf (`harw_registry_defaults::profile::
/// role_names::UIA_SHELL_WORKER`; `harw-core` hängt nicht von
/// `harw-registry-defaults` ab, deshalb als Literal).
pub(super) const SUDO_HANDOFF_ROLE: &str = "uia-shell-worker";

/// Zusatz zur Beschreibung von `transfer_to_<role>` für Rollen, deren Zweck
/// das Modell sonst nicht aus dem Namen errät.
///
/// # Beschreibung
/// Heute nur `uia-shell-worker`: ohne diesen Satz sagte die UIA „sudo geht
/// nicht“, statt Root-Befehle dorthin zu delegieren. Jede andere Rolle
/// bekommt einen leeren Zusatz (Beschreibung unverändert).
pub(super) fn handoff_role_hint(role: &str) -> &'static str {
    if role == SUDO_HANDOFF_ROLE {
        " Host- und Root-Befehle (sudo): sudo funktioniert über diesen Agenten — er fragt \
         `host.sudo_exec` an, der Nutzer bestätigt den exakten Befehl im Freigabefenster der \
         TUI und gibt dort sein Passwort ein, falls sudo eines verlangt. Im Auftrag exakten \
         Befehl und Grund nennen, nie ein Passwort."
    } else {
        ""
    }
}

/// Plan R9, Teil C: das eine Delegationswerkzeug für große Ziellisten
/// (mehr als [`crate::delegation_visibility::DELEGATE_TOOL_THRESHOLD`]
/// sichtbare Ziele). Es läuft durch dieselbe Handoff-Erkennung wie
/// `transfer_to_<name>` ([`delegate_tool_role`]).
pub const AGENTS_DELEGATE_TOOL: &str = "agents.delegate";

/// Plan R9, Teil C: der Katalog der sichtbaren Delegationsziele
/// ([`agents_catalog_result`]); nur angeboten, wenn der Aufrufer Ziele sieht.
pub const AGENTS_CATALOG_TOOL: &str = "agents.catalog";

/// Höchstlänge (Zeichen) der Einzeilen-Beschreibung eines Ziels in der
/// Beschreibung von `transfer_to_<name>`.
pub const HANDOFF_DESCRIPTION_CHARS: usize = 90;

/// Die erste Zeile bzw. der erste Satz von `text`, auf `max` Zeichen gekappt
/// (mit `…`). Leer bleibt leer.
pub(super) fn one_line(text: &str, max: usize) -> String {
    let line = text.trim().lines().next().unwrap_or_default().trim();
    let sentence = match line.find(". ") {
        Some(index) => &line[..=index],
        None => line,
    };
    if sentence.chars().count() <= max {
        return sentence.to_owned();
    }
    let mut capped: String = sentence.chars().take(max.saturating_sub(1)).collect();
    capped.push('…');
    capped
}

/// Die gemeinsamen Felder von `transfer_to_<name>` und `agents.delegate`
/// (`task`, `context`, `continue_from`, `user_approved`).
///
/// # Beschreibung
/// Plan R9, Teil C: die Beschreibungen sind bewusst knapp — sie stehen in
/// jeder Delegationsdefinition und damit in jedem Prompt.
pub(super) fn handoff_properties() -> BTreeMap<String, JsonSchema> {
    let string_property = |description: &str| JsonSchema {
        schema_type: Some(JsonSchemaType::String),
        description: Some(description.to_owned()),
        ..JsonSchema::default()
    };
    let mut properties = BTreeMap::new();
    properties.insert(
        "task".to_owned(),
        string_property("Konkreter, eigenständig verständlicher Arbeitsauftrag."),
    );
    properties.insert(
        "context".to_owned(),
        string_property("Optional: Fakten, Pfade, Randbedingungen."),
    );
    // Runde 5, Teil J: Fortsetzung eines beendeten eigenen Kindes derselben
    // Rolle; geprüft von `ManagedAgentSpawner::admit`.
    properties.insert(
        "continue_from".to_owned(),
        string_property(
            "Optional: ID eines eigenen beendeten Kindes dieser Rolle; setzt mit dessen \
             Übergabe fort (max. 3×).",
        ),
    );
    // Runde 9, Teil E4: vorab erteilte Freigaben der Nutzerin.
    properties.insert(
        crate::user_approval::USER_APPROVED_FIELD.to_owned(),
        crate::user_approval::user_approved_schema(),
    );
    properties
}

/// Ein geschlossenes Objekt-Schema mit Pflichtfeldern.
pub(super) fn closed_object(properties: BTreeMap<String, JsonSchema>, required: &[&str]) -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        properties: Some(properties),
        required: Some(required.iter().map(|field| (*field).to_owned()).collect()),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..JsonSchema::default()
    }
}

/// Baut die Werkzeugdefinition `transfer_to_<role>` für ein Delegationsziel.
///
/// # Beschreibung
/// Schema: [`handoff_properties`] (`task` Pflicht), keine weiteren Felder.
/// Die Ausführung übernimmt die Handoff-Erkennung in `drive_turn`
/// ([`handoff_role`]), nicht ein `ToolExecutor`. Plan R9, Teil C: die
/// Beschreibung trägt die Einzeilen-Beschreibung des Ziels (höchstens
/// [`HANDOFF_DESCRIPTION_CHARS`] Zeichen), damit das Modell ohne Katalog
/// weiß, wofür der Agent da ist.
///
/// # Arguments
/// - `role` (`&str`): exakter Rollenname des Delegationsziels.
/// - `description` (`Option<&str>`): Beschreibung des Ziels, falls bekannt.
///
/// # Returns
/// Die Function-Tool-Spezifikation.
pub(super) fn handoff_tool_spec(role: &str, description: Option<&str>) -> ToolSpec {
    let summary = description
        .map(|text| one_line(text, HANDOFF_DESCRIPTION_CHARS))
        .filter(|text| !text.is_empty())
        .map(|text| format!(" {text}"))
        .unwrap_or_default();
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(format!("{HANDOFF_PREFIX}{role}")),
        description: format!(
            "Delegates to '{role}'. Always runs in the background: returns immediately with ids; the result and progress arrive automatically (no waiting or polling).{summary}{}",
            handoff_role_hint(role)
        ),
        parameters: closed_object(handoff_properties(), &["task"]),
        strict: false,
    })
}

/// Plan R9, Teil C: die Werkzeugdefinition `agents.delegate` —
/// `{agent, task, context?, continue_from?, background?, user_approved?}`,
/// `agent` als Enum der sichtbaren Ziele (sortiert, Cache-stabil).
pub(super) fn agents_delegate_tool_spec(names: &[String]) -> ToolSpec {
    let mut properties = handoff_properties();
    properties.insert(
        "agent".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some("Ziel-Agent; Details: `agents.catalog`.".to_owned()),
            enum_values: Some(
                names
                    .iter()
                    .map(|name| serde_json::Value::String(name.clone()))
                    .collect(),
            ),
            ..JsonSchema::default()
        },
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(AGENTS_DELEGATE_TOOL),
        description: "Delegates to a visible agent. Always runs in the background: returns immediately with ids; the result and progress arrive automatically (no waiting or polling)."
            .to_owned(),
        parameters: closed_object(properties, &["agent", "task"]),
        strict: false,
    })
}

/// Plan R9, Teil C: die Werkzeugdefinition `agents.catalog {query?, role?}`.
pub(super) fn agents_catalog_tool_spec() -> ToolSpec {
    let mut properties = BTreeMap::new();
    properties.insert(
        "query".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some("Optional: Suchbegriffe (Name, Beschreibung, Skills).".to_owned()),
            ..JsonSchema::default()
        },
    );
    properties.insert(
        "role".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some(
                "Optional: nur diese Organisationsrolle (worker, child-orchestrator, …)."
                    .to_owned(),
            ),
            ..JsonSchema::default()
        },
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(AGENTS_CATALOG_TOOL),
        description: "Listet die Agenten, an die du delegieren darfst: Rolle, Beschreibung, \
                      Skills, Rechte, Budget, eingebaut/benutzerdefiniert."
            .to_owned(),
        parameters: closed_object(properties, &[]),
        strict: false,
    })
}

/// Ob die Session ein erzeugtes Werkzeug `name` anbieten darf.
///
/// # Beschreibung
/// Wie für `transfer_to_*`: ausgelassen wird es nur, wenn ein registriertes
/// Werkzeug gleichen Namens existiert (die Registry gewinnt) oder die
/// Session-Aktivierung genau dieses Werkzeug ausdrücklich abgeschaltet hat
/// (das Profil ließe es zu, `is_tool_enabled` verneint).
pub(super) fn may_offer_generated_tool(session: &AgentSession, tools: &[ToolSpec], name: &ToolName) -> bool {
    if tools.iter().any(|spec| spec.name() == name.as_str()) {
        return false;
    }
    let activation = session.activation();
    let profile_admits = activation
        .profile()
        .allowlist()
        .as_ref()
        .is_none_or(|allowlist| allowlist.contains(name));
    !(profile_admits && !activation.is_tool_enabled(name))
}

/// Ergänzt die Werkzeugliste einer Modellanfrage um die Delegationswerkzeuge
/// der sichtbaren Ziele.
///
/// # Beschreibung
/// Delegationsziele liefert der Spawner
/// ([`harw_extension_api::AgentSpawner::delegation_targets`]) — bereits nach
/// Sichtbarkeit **und** Plan-Modus gefiltert
/// ([`crate::delegation_visibility::delegable_in_mode`]); dessen Regeln sind
/// die Autorität, nicht das Werkzeugprofil der Session (`Minimal`/`Coding`
/// kennen keine `transfer_to_*`-Namen und würden Handoffs sonst stets
/// verbergen).
///
/// Plan R9, Teil C:
/// - höchstens [`crate::delegation_visibility::DELEGATE_TOOL_THRESHOLD`]
///   Ziele: je Ziel ein `transfer_to_<name>` mit Einzeilen-Beschreibung;
/// - mehr Ziele: nur `agents.delegate` mit `agent` als Enum;
/// - in beiden Fällen `agents.catalog`.
///
/// Ein einzelnes Werkzeug entfällt, wenn ein registriertes Werkzeug gleichen
/// Namens existiert, der Name ungültig ist ([`is_valid_handoff_role`]) oder
/// die Aktivierung es ausdrücklich abschaltet ([`may_offer_generated_tool`]).
/// Danach wird wieder stabil nach Namen sortiert (Prompt-Cache, siehe
/// [`collect_tools`]).
///
/// # Arguments
/// - `session` (`&AgentSession`): liefert die Aktivierung.
/// - `tools` (`&mut Vec<ToolSpec>`): Ergebnis von [`collect_tools`].
/// - `targets` (`&[DelegationTargetInfo]`): nach Namen sortierte Ziele.
pub(super) fn append_handoff_tools(
    session: &AgentSession,
    tools: &mut Vec<ToolSpec>,
    targets: &[DelegationTargetInfo],
) {
    let valid: Vec<&DelegationTargetInfo> = targets
        .iter()
        .filter(|target| {
            let valid = is_valid_handoff_role(&target.name);
            if !valid {
                tracing::warn!(role = %target.name, "turn_loop.handoff_tool_invalid_role");
            }
            valid
        })
        .collect();
    if valid.is_empty() {
        return;
    }
    let mut added = false;
    if valid.len() > crate::delegation_visibility::DELEGATE_TOOL_THRESHOLD {
        let name = ToolName::new(AGENTS_DELEGATE_TOOL);
        if may_offer_generated_tool(session, tools, &name) {
            let names: Vec<String> = valid.iter().map(|target| target.name.clone()).collect();
            tools.push(agents_delegate_tool_spec(&names));
            added = true;
        }
    } else {
        for target in &valid {
            let name = ToolName::new(format!("{HANDOFF_PREFIX}{}", target.name));
            if !may_offer_generated_tool(session, tools, &name) {
                continue;
            }
            tools.push(handoff_tool_spec(
                &target.name,
                target.description.as_deref(),
            ));
            added = true;
        }
    }
    let catalog = ToolName::new(AGENTS_CATALOG_TOOL);
    if may_offer_generated_tool(session, tools, &catalog) {
        tools.push(agents_catalog_tool_spec());
        added = true;
    }
    if added {
        tools.sort_by(|a, b| a.name().cmp(b.name()));
    }
}

/// Ob der Turn-Loop ein erzeugtes Delegationswerkzeug (ohne
/// Registry-Ausführer) dieser Session gerade anbietet.
///
/// # Beschreibung
/// Welche Namen überhaupt erzeugte Delegationswerkzeuge sind, entscheidet
/// allein [`crate::child_approval::is_generated_delegation_tool`]; hier kommt
/// nur die Sichtbarkeit dazu — dieselben Regeln für Modus, Ziele und
/// Aktivierung wie bei der Modellanfrage ([`append_handoff_tools`]). Eine
/// Freigabe darf ein inzwischen abgeschaltetes Werkzeug nicht wieder
/// aufrufbar machen.
pub(crate) fn generated_delegation_tool_available(session: &AgentSession, name: &ToolName) -> bool {
    if !crate::child_approval::is_generated_delegation_tool(name) {
        return false;
    }
    // Ein registriertes Werkzeug besitzt seinen Namen, auch ohne Ausführer.
    if session.registry().tool_providers().iter().any(|provider| {
        provider
            .tools()
            .iter()
            .any(|spec| spec.name() == name.as_str())
    }) {
        return false;
    }
    let plan_mode = session.mode() == crate::mode::InteractionMode::Plan;
    let Some(Ok(targets)) = session_delegation_targets(session, plan_mode) else {
        return false;
    };
    let mut tools = Vec::new();
    append_handoff_tools(session, &mut tools, &targets.targets);
    tools.iter().any(|spec| spec.name() == name.as_str())
}

/// Plan R9, Teil C: schränkt `targets[].role` eines vorhandenen
/// `delegate_wave`-Werkzeugs auf die sichtbaren Ziele ein (Enum).
///
/// # Beschreibung
/// Das Schema von `delegate_wave` ist statisch (die Operation kennt ihren
/// Aufrufer erst beim Aufruf); die Sichtbarkeit kennt nur der Turn-Loop je
/// Anfrage. Ohne Ziele bleibt das Schema unverändert (ein leeres Enum wäre
/// ungültig) — der Aufruf scheitert dann mit einer benannten Meldung.
pub(super) fn restrict_delegate_wave_roles(tools: &mut [ToolSpec], names: &[String]) {
    if names.is_empty() {
        return;
    }
    for spec in tools.iter_mut() {
        let ToolSpec::Function(function) = spec;
        if function.name.as_str() != DELEGATE_WAVE_TOOL_NAME {
            continue;
        }
        let role = function
            .parameters
            .properties
            .as_mut()
            .and_then(|properties| properties.get_mut("targets"))
            .and_then(|targets| targets.items.as_deref_mut())
            .and_then(|item| item.properties.as_mut())
            .and_then(|properties| properties.get_mut("role"));
        if let Some(role) = role {
            role.enum_values = Some(
                names
                    .iter()
                    .map(|name| serde_json::Value::String(name.clone()))
                    .collect(),
            );
        }
    }
}

/// Name des Fan-out-Werkzeugs der Orchestratoren
/// (`harw_core_bridge::DELEGATE_WAVE_TOOL`; `harw-core` hängt nicht von der
/// Bridge ab, deshalb als Literal).
pub(super) const DELEGATE_WAVE_TOOL_NAME: &str = "delegate_wave";

/// Plan R9, Teil C: Ergebnis von `agents.catalog {query?, role?}` über den
/// sichtbaren Zielen des Aufrufers.
///
/// # Beschreibung
/// Zeigt nur, was der Aufrufer ohnehin delegieren darf (dieselbe
/// Sichtbarkeit wie Spawn, inklusive Plan-Modus) — kein Orakel. `role`
/// filtert exakt nach Organisationsrolle; `query` rankt wie `skills.search`
/// nach einfachen Token-Treffern (Name zählt doppelt, dann Beschreibung,
/// Skills, Rolle) und lässt Ziele ohne Treffer weg. Ohne `query` bleibt die
/// Namensreihenfolge.
pub(super) fn agents_catalog_result(
    targets: &harw_extension_api::DelegationTargets,
    arguments: &serde_json::Value,
) -> ToolCallResult {
    let text_field = |name: &str| {
        arguments
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_lowercase)
    };
    let role_filter = text_field("role");
    let tokens: Vec<String> = text_field("query")
        .map(|query| {
            query
                .split(|c: char| !c.is_alphanumeric())
                .filter(|token| !token.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let mut ranked: Vec<(usize, &DelegationTargetInfo)> = targets
        .targets
        .iter()
        .filter(|target| {
            role_filter
                .as_deref()
                .is_none_or(|role| target.role.eq_ignore_ascii_case(role))
        })
        .filter_map(|target| {
            if tokens.is_empty() {
                return Some((0, target));
            }
            let name = target.name.to_lowercase();
            let rest = format!(
                "{} {} {}",
                target.description.as_deref().unwrap_or_default(),
                target.skills.join(" "),
                target.role
            )
            .to_lowercase();
            let score: usize = tokens
                .iter()
                .map(|token| {
                    usize::from(name.contains(token.as_str())) * 2
                        + usize::from(rest.contains(token.as_str()))
                })
                .sum();
            (score > 0).then_some((score, target))
        })
        .collect();
    ranked.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.name.cmp(&right.name))
    });
    let agents: Vec<serde_json::Value> = ranked
        .iter()
        .map(|(_, target)| {
            serde_json::json!({
                "name": target.name,
                "role": target.role,
                "description": target
                    .description
                    .as_deref()
                    .map(|text| one_line(text, 160)),
                "skills": target.skills,
                "profile": target.profile_summary,
                "read_only": target.read_only,
                "budget_tokens": target.budget_tokens,
                "source": if target.custom { "custom" } else { "built-in" },
            })
        })
        .collect();
    let mut result = serde_json::json!({
        "count": agents.len(),
        "agents": agents,
    });
    if targets.plan_mode
        && let Some(object) = result.as_object_mut()
    {
        object.insert(
            "note".to_owned(),
            serde_json::Value::String(
                "Plan-Modus: nur lesende Ziele; Schreib-/Ausführungsziele erst nach \
                 Planfreigabe."
                    .to_owned(),
            ),
        );
    }
    ToolCallResult::success(result)
}

/// Plan R9, Teil C: die Rolle eines `agents.delegate`-Aufrufs und die
/// Handoff-Argumente ohne das Feld `agent` (damit Auftrag, Fortsetzung,
/// Hintergrund und Freigaben exakt wie bei `transfer_to_<agent>` wirken).
///
/// # Returns
/// `None` für jeden anderen Werkzeugnamen; `Some(Err(meldung))` für einen
/// `agents.delegate`-Aufruf ohne gültiges `agent`.
pub(super) fn delegate_tool_role(call: &ToolCall) -> Option<Result<(String, serde_json::Value), String>> {
    if call.name.as_str() != AGENTS_DELEGATE_TOOL {
        return None;
    }
    let agent = call
        .arguments
        .get("agent")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|agent| !agent.is_empty());
    let Some(agent) = agent else {
        return Some(Err(
            "agents.delegate: `agent` fehlt (Name eines sichtbaren Ziels, siehe agents.catalog)"
                .to_owned(),
        ));
    };
    let mut arguments = call.arguments.clone();
    if let Some(object) = arguments.as_object_mut() {
        object.remove("agent");
    }
    Some(Ok((agent.to_owned(), arguments)))
}

/// Plan R9, Teil C/E1: prüft ein Delegationsziel vor dem Spawn gegen die
/// Sichtbarkeit des Aufrufers und liefert eine benannte Ablehnung.
///
/// # Beschreibung
/// - Ziel im Plan-Modus zurückgehalten → Plan-Modus-Meldung
///   ([`crate::delegation_visibility::plan_mode_refusal`]).
/// - `strict` (für `agents.delegate`) und Ziel nicht sichtbar → „Ziel X ist
///   für dich nicht delegierbar; delegierbar sind: …“ (nur sichtbare Namen).
/// - sonst `None`: der Spawner entscheidet wie bisher (für `transfer_to_*`
///   bleibt die Admission die Autorität, auch bei Spawnern ohne
///   Sichtbarkeitsprojektion).
pub(super) fn delegation_refusal(
    targets: &Result<
        harw_extension_api::DelegationTargets,
        harw_extension_api::DelegationUnavailable,
    >,
    role: &str,
    strict: bool,
) -> Option<String> {
    match targets {
        Ok(targets) if targets.get(role).is_some() => None,
        Ok(targets) if targets.is_withheld_by_plan_mode(role) => Some(
            crate::delegation_visibility::plan_mode_refusal(&targets.names()),
        ),
        Ok(targets) if strict => Some(crate::delegation_visibility::not_delegable_message(
            role,
            &targets.names(),
        )),
        Ok(_) => None,
        Err(error) if strict => Some(error.to_string()),
        Err(_) => None,
    }
}

/// Plan R9, Teil C: die Delegationsziele der Session ohne die, deren
/// `transfer_to_<name>` die Aktivierung ausdrücklich abschaltet (etwa
/// `forbidden = ["transfer_to_executor"]` des Matrix-Game-Masters).
///
/// # Beschreibung
/// Ein abgeschaltetes Ziel bleibt so auch über `agents.delegate`,
/// `agents.catalog` und den Kontextblock unerreichbar — sonst wäre das
/// Einzelwerkzeug-Verbot über das Sammelwerkzeug umgehbar.
pub(super) fn session_delegation_targets(
    session: &AgentSession,
    plan_mode: bool,
) -> Option<Result<harw_extension_api::DelegationTargets, harw_extension_api::DelegationUnavailable>>
{
    let spawner = session.registry().spawner()?;
    let activation = session.activation();
    let allowlist = activation.profile().allowlist();
    let disabled = |name: &str| {
        let tool = ToolName::new(format!("{HANDOFF_PREFIX}{name}"));
        let profile_admits = allowlist
            .as_ref()
            .is_none_or(|allowlist| allowlist.contains(&tool));
        profile_admits && !activation.is_tool_enabled(&tool)
    };
    Some(
        spawner
            .delegation_targets(session.id(), plan_mode)
            .map(|mut targets| {
                targets.targets.retain(|target| !disabled(&target.name));
                targets
                    .withheld_by_plan_mode
                    .retain(|target| !disabled(&target.name));
                targets
            }),
    )
}

/// Plan R9, Teil C/E1: der Delegationsschritt eines Werkzeugaufrufs im
/// Turn-Loop.
///
/// # Returns
/// `(handoff, lokales_ergebnis)`:
/// - `(Some((rolle, argumente)), None)`: spawnen wie `transfer_to_<rolle>`
///   (auch `agents.delegate` und `agent.message` an ein beendetes eigenes
///   Kind, Runde 9 E3).
/// - `(None, Some(ergebnis))`: sofort als Werkzeugergebnis beantworten —
///   `agents.catalog`, ein `agents.delegate` ohne `agent`, ein nicht
///   delegierbares Ziel oder die Plan-Modus-Ablehnung.
/// - `(None, None)`: kein Delegationsaufruf; normale Ausführung.
pub(super) fn delegation_step(
    session: &AgentSession,
    call: &ToolCall,
) -> (Option<(String, serde_json::Value)>, Option<ToolCallResult>) {
    let plan_mode = session.mode() == crate::mode::InteractionMode::Plan;
    if call.name.as_str() == AGENTS_CATALOG_TOOL && find_executor(session, &call.name).is_none() {
        let result = match session_delegation_targets(session, plan_mode) {
            Some(Ok(targets)) if generated_delegation_tool_available(session, &call.name) => {
                agents_catalog_result(&targets, &call.arguments)
            }
            Some(Ok(_)) => ToolCallResult::error(
                "agents.catalog: Werkzeug ist für diese Sitzung nicht verfügbar".to_owned(),
            ),
            Some(Err(error)) => ToolCallResult::error(error.to_string()),
            None => ToolCallResult::error(
                "agents.catalog: kein Agent-Spawner in dieser Sitzung".to_owned(),
            ),
        };
        return (None, Some(result));
    }
    let (role, arguments, strict) = if let Some(role) = handoff_role(&call.name) {
        (role, call.arguments.clone(), false)
    } else if find_executor(session, &call.name).is_none()
        && let Some(delegate) = delegate_tool_role(call)
    {
        match delegate {
            Ok((role, arguments)) => (role, arguments, true),
            Err(message) => return (None, Some(ToolCallResult::error(message))),
        }
    } else if let Some((role, arguments)) = message_resume_handoff(session, call) {
        (role, arguments, false)
    } else {
        return (None, None);
    };
    if let Some(targets) = session_delegation_targets(session, plan_mode)
        && let Some(message) = delegation_refusal(&targets, &role, strict)
    {
        tracing::info!(role = %role, reason = %message, "turn_loop.delegation_refused");
        return (None, Some(ToolCallResult::error(message)));
    }
    if let Some(result) = background_only_refusal(session, &arguments) {
        tracing::info!(role = %role, "turn_loop.delegation_background_only_refused");
        return (None, Some(result));
    }
    (Some((role, arguments)), None)
}
