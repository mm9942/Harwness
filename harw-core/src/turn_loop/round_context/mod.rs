//! Round request assembly, budgeting and compaction.

use super::*;

/// Prüft die [`crate::auto_compact::AutoCompactPolicy`] der Session und
/// verdichtet bei Bedarf die Historie an einer sicheren Grenze.
///
/// # Description
/// No-op, wenn die Session keine Policy gesetzt hat
/// ([`AgentSession::auto_compact`] liefert `None` — der Default für jede
/// Session, die sich nicht explizit für Auto-Compaction entscheidet).
///
/// Die Belegung, gegen die die Policy entscheidet, ist die Schätzung des
/// **nächsten** Requests: [`harw_types::TokenUsage::prompt_tokens`] der
/// letzten Runde (bei OpenAI-Semantik genau `input_tokens`, bei Anthropic
/// (`cache_separate`) ungecachter Input + Cache-Read + Cache-Write — sonst
/// würde ein fast vollständig gecachter Prompt die Schwelle nie erreichen)
/// plus deren `output_tokens` (die Antwort steht jetzt im Verlauf) plus
/// `appended_tokens` (seitdem angehängte Tool-Ergebnisse, siehe
/// [`appended_tokens_since`]).
///
/// Hysterese: auch bei fälliger Policy wird nur verdichtet, wenn
/// [`crate::auto_compact::AutoCompactPolicy::worth_compacting`] gegenüber dem
/// Ergebnis der letzten Verdichtung (`last_compaction_tokens_after`) zustimmt
/// — sonst verdichtete jede Runde einen Verlauf, der sich nicht weiter
/// verkleinern lässt. Die Verdichtung selbst läuft über
/// [`run_budget_compaction`] (Ziel: 30 % des Fensters, wie bisher
/// [`crate::compaction::CompactionPlan::for_context_window`]); ein No-op wird
/// weder persistiert noch gemeldet.
///
/// # Arguments
/// - `session` (`&mut AgentSession`): deren Historie ggf. ersetzt wird.
/// - `model` (`&dyn ModelProvider`): für den optionalen
///   Zusammenfassungs-Aufruf.
/// - `store` (`&dyn StateStore`): Ziel der Persistenz nach erfolgreichem
///   Compact.
/// - `last_round_usage` (`&harw_types::TokenUsage`): Nutzung der zuletzt
///   abgeschlossenen Modell-Runde.
/// - `appended_tokens` (`u64`): geschätzte Tokens der seit dieser Runde
///   angehängten Items.
/// - `task_completed` (`bool`): `true`, wenn der gerade beendete Abschnitt
///   einen Plan-Schritt abgeschlossen hat (siehe
///   [`turn_completed_a_plan_step`]).
/// - `last_compaction_tokens_after` (`&mut Option<u64>`): Ergebnis der
///   letzten Verdichtung dieses `drive_turn`-Aufrufs; wird nach einer
///   Verdichtung (auch einem No-op) fortgeschrieben.
///
/// # Concurrency
/// `async`; führt höchstens einen Modellaufruf aus (Zusammenfassung) und
/// einen Store-Aufruf. Fehler beider Seiten werden nur geloggt — ein
/// Compact-Fehlschlag darf den Turn nie abbrechen.
pub(super) async fn maybe_compact(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    last_round_usage: &harw_types::TokenUsage,
    appended_tokens: u64,
    task_completed: bool,
    last_compaction_tokens_after: &mut Option<u64>,
) {
    let Some(policy) = session.auto_compact().copied() else {
        return;
    };

    let tokens_used = last_round_usage
        .prompt_tokens()
        .saturating_add(last_round_usage.output_tokens)
        .saturating_add(appended_tokens);
    let decision = policy.decide(tokens_used, task_completed);
    if !decision.should_compact() {
        return;
    }
    if !policy.worth_compacting(tokens_used, *last_compaction_tokens_after) {
        tracing::debug!(
            tokens_used,
            last_compaction_tokens_after = ?*last_compaction_tokens_after,
            "turn_loop.auto_compact_skipped_hysteresis",
        );
        return;
    }

    let target = policy
        .context_window_tokens()
        .saturating_mul(REGULAR_COMPACTION_TARGET_PERCENT)
        / 100;
    if let Some(outcome) = run_budget_compaction(session, model, store, target, 0, decision).await {
        *last_compaction_tokens_after = Some(outcome.tokens_after);
    }
}

/// Ziel der regulären Auto-Verdichtung in Prozent des Fensters (wie
/// [`crate::compaction::CompactionPlan::for_context_window`]).
pub(super) const REGULAR_COMPACTION_TARGET_PERCENT: u64 = 30;

/// Ziel der Notfall-Verdichtung vor dem Senden, in Prozent von
/// `Fenster − Output-Reserve`.
pub(super) const EMERGENCY_PREFLIGHT_TARGET_PERCENT: u64 = 60;

/// Ziel der Notfall-Verdichtung nach einer vom Provider gemeldeten
/// Kontextüberschreitung, in Prozent von `Fenster − Output-Reserve`.
pub(super) const EMERGENCY_RECOVERY_TARGET_PERCENT: u64 = 50;

/// Rückfall-Ausgabelimit, wenn weder Session noch Fenster eines liefern
/// (entspricht `context_budget::output_reserve_tokens` ohne Konfiguration).
pub(super) const FALLBACK_OUTPUT_RESERVE_TOKENS: u64 = 16_384;

/// Fenster-Kennzahlen einer Modellrunde (Welle 3), einmal je Runde aus der
/// Session gelesen.
#[derive(Debug, Clone, Copy)]
pub(super) struct RoundBudget {
    /// Kontextfenster des aktiven Modells laut Auto-Compact-Policy; 0, wenn
    /// die Session keine Policy trägt (dann greifen keine Fenster-Wächter).
    pub(super) window_tokens: u64,
    /// Für die Antwort freizuhaltende Tokens.
    pub(super) reserve_tokens: u64,
    /// Effektive Compact-Schwelle der Policy, sofern vorhanden.
    pub(super) threshold_tokens: Option<u64>,
}

impl RoundBudget {
    /// Liest Fenster, Reserve und Schwelle aus der Session.
    ///
    /// # Description
    /// Die Reserve kommt bevorzugt aus der Policy
    /// (`AutoCompactPolicy::output_reserve_tokens`, dieselbe Zahl, mit der
    /// `must_compact_before_send` rechnet); ist dort keine gesetzt, wird sie
    /// über [`crate::context_budget::output_reserve_tokens`] aus Fenster und
    /// [`AgentSession::max_output_tokens`] abgeleitet. Ohne Fenster gilt das
    /// konfigurierte Ausgabelimit bzw. [`FALLBACK_OUTPUT_RESERVE_TOKENS`].
    pub(super) fn of(session: &AgentSession) -> Self {
        match session.auto_compact() {
            Some(policy) if policy.context_window_tokens() > 0 => {
                let window_tokens = policy.context_window_tokens();
                let reserve_tokens = match policy.output_reserve_tokens() {
                    0 => crate::context_budget::output_reserve_tokens(
                        window_tokens,
                        session.max_output_tokens(),
                        None,
                        false,
                    ),
                    reserve => reserve,
                };
                Self {
                    window_tokens,
                    reserve_tokens,
                    threshold_tokens: Some(policy.effective_compact_threshold_tokens()),
                }
            }
            _ => Self {
                window_tokens: 0,
                reserve_tokens: session
                    .max_output_tokens()
                    .unwrap_or(FALLBACK_OUTPUT_RESERVE_TOKENS),
                threshold_tokens: None,
            },
        }
    }

    /// `true`, wenn ein Request mit `estimated_tokens` zusammen mit der
    /// Reserve das bekannte Fenster sprengt (ohne Fenster nie).
    pub(super) fn overflows(&self, estimated_tokens: u64) -> bool {
        self.window_tokens > 0
            && estimated_tokens.saturating_add(self.reserve_tokens) > self.window_tokens
    }

    /// Ziel-Token-Zahl einer Notfall-Verdichtung: `percent` % von
    /// `Fenster − Reserve`.
    pub(super) fn emergency_target(&self, percent: u64) -> u64 {
        self.window_tokens
            .saturating_sub(self.reserve_tokens)
            .saturating_mul(percent)
            / 100
    }

    /// Ziel-Token-Zahl der Notfall-Verdichtung nach einer vom Provider
    /// gemeldeten Kontextüberschreitung: höchstens die Hälfte der Schätzung
    /// des gescheiterten Requests (sonst ändert die Wiederholung nichts) und
    /// bei bekanntem Fenster höchstens
    /// [`EMERGENCY_RECOVERY_TARGET_PERCENT`] % von `Fenster − Reserve`.
    pub(super) fn recovery_target(&self, estimated_tokens: u64) -> u64 {
        let half = estimated_tokens / 2;
        if self.window_tokens > 0 {
            self.emergency_target(EMERGENCY_RECOVERY_TARGET_PERCENT)
                .min(half)
        } else {
            half
        }
    }

    /// Verdoppeltes Ausgabelimit für die Wiederholung nach `MaxTokens` mit
    /// Tool-Calls: `min(2 × current, 2 × Reserve)` und bei bekanntem Fenster
    /// höchstens dessen Hälfte.
    pub(super) fn raised_output_limit(&self, current: u64) -> u64 {
        let mut raised = current
            .saturating_mul(2)
            .min(self.reserve_tokens.saturating_mul(2));
        if self.window_tokens > 0 {
            raised = raised.min(self.window_tokens / 2);
        }
        raised
    }
}

/// Baut den Model-Request einer Runde: Context, Instructions, Tools,
/// Delegationsziele, Kontextmontage und alle Request-Optionen.
///
/// # Description
/// Ausgelagert, damit der Pre-flight-Wächter und die Wiederholungen nach
/// einer Notfall-Verdichtung den Request über exakt denselben Weg neu bauen
/// können. `continuation` hängt die Fortsetzungsanweisung nach einem
/// abgeschnittenen Text an; `max_output_override` ersetzt für genau diese
/// Runde [`AgentSession::max_output_tokens`] (Wiederholung mit verdoppeltem
/// Limit).
///
/// PL-95 S3: die getriggerten Skill-Auszüge der Sitzung
/// ([`crate::skill_injection::SkillInjector`]) kommen als Fragmente der
/// Sektion `skills.triggered` hinter den cache-stabilen Präfix (siehe
/// [`prepare_skill_injection`] und [`append_skill_blocks`]). Festgehalten
/// (`injection`-Ereignis) werden sie erst, wenn das Modell den Request
/// beantwortet hat ([`record_skill_injections`] in `drive_turn`).
///
/// # Errors
/// Fehler von [`collect_tools`] und der Kontextmontage
/// ([`ModelRequest::with_context_program`]).
pub(super) async fn assemble_round_request(
    session: &AgentSession,
    ctx: &TurnInputContext,
    control: &TurnControl,
    turn_id: &harw_types::TurnId,
    total_usage: &harw_types::TokenUsage,
    continuation: Option<&str>,
    max_output_override: Option<u64>,
) -> CoreResult<ModelRequest> {
    let mut fragments = gather_context(session, ctx).await;
    if let Some(instruction) = continuation {
        match continuation_fragment(instruction) {
            Some(fragment) => fragments.push(fragment),
            None => {
                tracing::warn!("turn_loop.continuation_fragment_unavailable");
            }
        }
    }
    // Teil C: ab der Abschluss-Schwelle des Token-Budgets bekommt jede
    // weitere Anfrage die Anweisung, jetzt zusammenzufassen — damit beim
    // harten Limit eine verwertbare letzte Antwort als Teilergebnis vorliegt.
    if control.token_budget_wrap_up_due() {
        match instruction_fragment(
            "budget.wrap_up",
            "turn_loop.token_budget",
            TOKEN_BUDGET_WRAP_UP_INSTRUCTION,
        ) {
            Some(fragment) => fragments.push(fragment),
            None => tracing::warn!("turn_loop.wrap_up_fragment_unavailable"),
        }
    }
    // PL-95 S3: getriggerte Skill-Auszüge dieser Runde (aus den seit der
    // letzten Runde angehängten Verlaufs-Items).
    let skill_fragments = prepare_skill_injection(session)
        .map(|round| round.fragments)
        .unwrap_or_default();
    let mut instructions = load_instructions(session).await;
    let mut tools = collect_tools(session)?;
    let mut workflow_delegation_available = false;

    // Nachtrag F (Delegationsprojektion): EIN deterministischer
    // Kontextblock, NACH den Tools angehängt (stabiler Teil — die Liste
    // ändert sich selten, sortiert vom Spawner geliefert). Leer ⇒ nichts.
    // Dieselben Ziele werden zusätzlich als echte Werkzeugdefinitionen
    // `transfer_to_<role>` angeboten — erst damit kann das Modell den
    // Handoff tatsächlich aufrufen (siehe `append_handoff_tools`).
    //
    // Plan R9, Teil C/E1: der Spawner liefert die Ziele samt Katalogdaten,
    // bereits nach Plan-Modus gefiltert (nur lesende Ziele). Der eigene
    // Modus wird vorher gemeldet, damit Kinder einer extern gefahrenen
    // Wurzel ihn erben. Ein benannter Grund ohne Ziele (Resttiefe 0, kein
    // Spawn-Kontext) lässt die Anfrage ohne Delegationswerkzeuge.
    if let Some(spawner) = session.registry().spawner() {
        let plan_mode = session.mode() == crate::mode::InteractionMode::Plan;
        spawner.note_caller_mode(session.id(), plan_mode);
        match session_delegation_targets(session, plan_mode)
            .unwrap_or_else(|| Ok(harw_extension_api::DelegationTargets::default()))
        {
            Ok(delegation) => {
                append_handoff_tools(session, &mut tools, &delegation.targets);
                let names = delegation.names();
                workflow_delegation_available = !names.is_empty();
                restrict_delegate_wave_roles(&mut tools, &names);
                let via_delegate_tool =
                    tools.iter().any(|spec| spec.name() == AGENTS_DELEGATE_TOOL);
                let catalog = tools.iter().any(|spec| spec.name() == AGENTS_CATALOG_TOOL);
                // Kompakte Top-k-Empfehlung für große Ziellisten: Wunsch-Tags
                // aus der jüngsten User-Nachricht; reine Darstellung, das
                // Werkzeugschema und die Admission bleiben unberührt.
                let wanted = wanted_tags(session.history().last_user_text().unwrap_or_default());
                let offered = delegation.offers(
                    &wanted,
                    crate::delegation_visibility::DELEGATE_TOOL_THRESHOLD,
                );
                if let Some(fragment) =
                    delegation_targets_fragment(&names, via_delegate_tool, catalog, &offered)
                {
                    fragments.push(fragment);
                }
            }
            Err(error) => {
                tracing::debug!(reason = %error, "turn_loop.delegation_targets_unavailable");
            }
        }
    }

    // Workflow guidance is derived from the final tool surface, not the
    // baseline identity captured before composition. Static text only: no
    // repository content or user text is promoted to instruction trust. The
    // same admitted tool set/mode produces identical bytes for prompt caches.
    // This guidance neither grants authority nor changes organization routing.
    if session.activation().is_instructions_enabled("workflow") {
        let names: Vec<&str> = tools
            .iter()
            .map(ToolSpec::name)
            .filter(|name| *name != "delegate_wave" || workflow_delegation_available)
            .collect();
        let workflow = harw_instructions::workflow_capabilities(
            &names,
            session.mode() == crate::mode::InteractionMode::Plan,
        );
        if !workflow.is_empty() {
            instructions.fragments.push(workflow);
        }
    }

    // Programm- und Decken-bewusste Montage (siehe
    // `ModelRequest::with_context_program`s Moduldoku, Abschnitt „Zwei
    // Wege zur Kontextmontage"): läuft nur, wenn die Sitzung **beide**
    // deklariert; sonst fällt sie intern auf denselben Byte-Budget-Pfad
    // zurück — eine Sitzung ohne `ContextProgram` sieht dadurch keine
    // Änderung.
    let program = session.context_program();
    let ceiling = session
        .spawn_context()
        .and_then(|spawn_context| spawn_context.ceiling.as_ref());

    let max_output_tokens = max_output_override
        .or(session.max_output_tokens())
        .map(|tokens| u32::try_from(tokens).unwrap_or(u32::MAX));

    // Kontext-Ledger: die Fakten der gesammelten Fragmente festhalten, bevor
    // die Montage sie verbraucht (nur wenn ein Ledger registriert ist).
    let ledger_facts = session.context_ledger().map(|_| {
        let mut facts = crate::context_ledger_hook::snapshot(&fragments);
        facts.extend(crate::context_ledger_hook::snapshot(&skill_fragments));
        facts
    });

    // 4. Model-Call (provider-neutral).
    let mut request = ModelRequest::with_context_program(
        instructions,
        fragments,
        session.history().clone(),
        tools,
        session.context_budget(),
        program,
        ceiling,
    )?
    .with_reasoning_effort(session.reasoning_effort())
    .with_model_id(session.active_model().cloned())
    .with_provider_id(session.active_provider().cloned())
    .with_tool_result_max_bytes(control.limits().tool_result_max_bytes_hint())
    .with_max_output_tokens(max_output_tokens)
    .with_cancel_token(control.cancel_token().clone())
    .with_identity(request_identity(session));
    // PL-95 S3: die Auszüge hinter den cache-stabilen Präfix; danach trägt
    // der Injektor nur noch, was dieser Request tatsächlich enthält.
    let skill_labels = match ceiling {
        Some(ceiling) if !skill_fragments.is_empty() => {
            append_skill_blocks(&mut request, skill_fragments, program, ceiling)
        }
        _ => Vec::new(),
    };
    if let Some(injector) = session.skill_injector() {
        for label in injector.reconcile(&skill_labels) {
            tracing::debug!(%label, "skills.triggered.omitted");
        }
    }
    if let (Some(ledger), Some(facts)) = (session.context_ledger(), ledger_facts.as_deref()) {
        crate::context_ledger_hook::record_turn(
            ledger.as_ref(),
            session.id().as_str(),
            turn_id.as_str(),
            facts,
            &request.context_assembly,
        );
    }
    Ok(match stream_sink_for_round(session, turn_id, total_usage) {
        Some(sink) => request.with_stream_sink(sink),
        None => request,
    })
}

/// Bereitet die getriggerten Skill-Auszüge einer Runde vor (PL-95 S3).
///
/// # Description
/// `None`, wenn die Sitzung keinen [`crate::skill_injection::SkillInjector`]
/// trägt oder die Sektion `skills.triggered` nicht zustellen kann
/// (Aktivierung aus, keine Kontext-Decke, Decke ohne die Sektion) —
/// fail-closed: dann wird gar nicht erst ausgewählt, und noch aktive Auszüge
/// verlassen den Kontext. Ein Fragment, dessen Label die Aktivierung
/// abschaltet, verwirft schon der Injektor; es belegt weder Dedupe noch
/// Sitzungsbudget.
pub(super) fn prepare_skill_injection(
    session: &AgentSession,
) -> Option<crate::skill_injection::RoundInjection> {
    let injector = session.skill_injector()?;
    let activation = session.activation();
    let deliverable = activation.is_context_enabled(harw_catalog::FRAGMENT_SECTION)
        && session
            .spawn_context()
            .and_then(|spawn_context| spawn_context.ceiling.as_ref())
            .is_some_and(|ceiling| {
                harw_context::SectionName::try_new(harw_catalog::FRAGMENT_SECTION)
                    .is_ok_and(|section| ceiling.sections.contains(&section))
            });
    if !deliverable {
        for label in injector.reconcile(&[]) {
            tracing::debug!(%label, "skills.triggered.undeliverable");
        }
        return None;
    }
    let window_tokens = RoundBudget::of(session).window_tokens;
    let round = injector.prepare_round_with(session.history(), window_tokens, |fragment| {
        activation.is_context_enabled(fragment.label.as_str())
    });
    for delivered in &round.delivered {
        tracing::debug!(
            skill = %delivered.skill,
            section = delivered.section.as_deref().unwrap_or(""),
            kind = delivered.kind.as_str(),
            key = %delivered.key,
            bytes = delivered.bytes,
            "skills.triggered.selected"
        );
    }
    for dropped in &round.dropped {
        tracing::debug!(
            skill = %dropped.skill,
            section = dropped.section.as_deref().unwrap_or(""),
            reason = %dropped.reason,
            "skills.triggered.dropped"
        );
    }
    for skill in &round.suppressed {
        tracing::debug!(%skill, "skills.triggered.imported_suppressed");
    }
    for label in &round.withheld {
        tracing::debug!(%label, "skills.triggered.withheld");
    }
    for label in &round.evicted {
        tracing::debug!(%label, "skills.triggered.evicted");
    }
    Some(round)
}

/// Hält jede Skill-Lieferung des eben beantworteten Requests als
/// `injection`-Ereignis fest (PL-95 S3), genau einmal je Lieferung.
///
/// Die Antwort macht jeden noch unbeantworteten aktiven Auszug zur
/// Lieferung ([`crate::skill_injection::SkillInjector::mark_answered`], mit
/// `turn_id` als Herkunft). Bestätigt ist eine Lieferung erst, wenn ihr
/// Ereignis dauerhaft angehängt ist
/// ([`crate::skill_injection::SkillInjector::pending_deliveries`], danach
/// [`crate::skill_injection::SkillInjector::confirm_recorded`]). Ein
/// Store-Fehler wird nur geloggt — die Messung darf den Turn nie abbrechen —
/// und die Lieferung bleibt offen: die nächste beantwortete Runde hängt sie
/// erneut an, mit der `turn_id` der ersten Antwort (PR #154).
pub(super) async fn record_skill_injections(
    session: &AgentSession,
    store: &dyn StateStore,
    turn_id: &harw_types::TurnId,
) {
    let Some(injector) = session.skill_injector() else {
        return;
    };
    injector.mark_answered(turn_id.as_str());
    for pending in injector.pending_deliveries() {
        let delivered = &pending.delivery;
        let record = pending.to_record();
        if let Err(error) = store.record_skill_injection(session.id(), &record).await {
            // Offen lassen: die nächste beantwortete Runde versucht es erneut.
            tracing::warn!(%error, skill = %delivered.skill, "turn_loop.skill_injection_record_failed");
            continue;
        }
        injector.confirm_recorded(&pending);
        tracing::info!(
            skill = %delivered.skill,
            section = delivered.section.as_deref().unwrap_or(""),
            kind = delivered.kind.as_str(),
            key = %delivered.key,
            bytes = delivered.bytes,
            "skills.triggered.injected"
        );
    }
}

/// Rendert die Skill-Auszüge einer Runde hinter den cache-stabilen Präfix
/// (PL-95 S3).
///
/// # Description
/// Die Auszüge laufen durch eine eigene Montage gegen das Kontextprogramm
/// der Sitzung (`exclude`, `must_include`, Detailgrad; ohne Programm ein
/// leeres) und ihre Decke (Sektion, Vertrauen, Sektionsbudget). Als
/// Gesamtbudget bleibt, was die reguläre Montage von der Decke übrig lässt.
///
/// Platzierung: jeder Provider rendert `system_prompt` und danach
/// `instruction_fragments` der Reihe nach. Der Instruktionsblock wird
/// deshalb als **letztes** Element an [`ModelRequest::instruction_fragments`]
/// gehängt — System-Prompt, gebundene Skill-Fragmente und L0-Katalog bleiben
/// ein byte-identischer Präfix, auch wenn ein neuer Auszug hinzukommt. Der
/// Datenblock (importierte Skills, `Evidence`) kommt in
/// [`ModelRequest::data_block`] hinter den Verlauf. Ohne Programm liest kein
/// Provider `ModelRequest::context` (F-016/G-023); dieser eine Weg gilt
/// deshalb für beide Pfade. Ein Montagefehler wird geloggt; der Request
/// bleibt dann ohne Auszüge.
///
/// # Returns
/// Die Labels der aufgenommenen Auszüge.
pub(super) fn append_skill_blocks(
    request: &mut ModelRequest,
    fragments: Vec<harw_context::Fragment>,
    program: Option<&harw_agent_dsl::executable::ContextProgram>,
    ceiling: &harw_context::ContextCeiling,
) -> Vec<String> {
    let empty_program = harw_agent_dsl::executable::ContextProgram::default();
    // Programmpfad: die reguläre Montage hat ihren Teil des Gesamtbudgets
    // der Decke schon verbraucht (ohne Programm montiert sie nicht).
    let spent = match program {
        Some(_) => {
            u32::try_from(request.context_assembly.estimated_context_bytes).unwrap_or(u32::MAX)
        }
        None => 0,
    };
    let budget = harw_context::ContextBudgetSpec {
        total: harw_lens_types::BudgetSpec {
            total: ceiling.budget.total.total.saturating_sub(spent),
        },
        per_section: ceiling.budget.per_section.clone(),
    };
    let assembled = match crate::context_budget::Assembly::gather(fragments)
        .admit(program.unwrap_or(&empty_program), ceiling)
        .and_then(|admitted| admitted.budget(&budget))
    {
        Ok(budgeted) => budgeted.render(),
        Err(error) => {
            tracing::warn!(%error, "turn_loop.skill_injection_montage_failed");
            return Vec::new();
        }
    };
    let included: Vec<&harw_context::Fragment> = assembled
        .sections
        .iter()
        .flat_map(|section| section.fragments.iter())
        .collect();
    let instruction_rank = harw_context::TrustClass::Instruction.trust_rank();
    let has_instruction = included
        .iter()
        .any(|fragment| fragment.trust.trust_rank() == instruction_rank);
    let has_data = included
        .iter()
        .any(|fragment| fragment.trust.trust_rank() != instruction_rank);
    let labels: Vec<String> = included
        .iter()
        .map(|fragment| fragment.label.as_str().to_owned())
        .collect();
    let detail = program
        .map(crate::model::section_detail_map)
        .unwrap_or_default();
    let blocks = assembled.render_trust_blocks_with_detail(&harw_observe::NullSink, &detail);
    if has_instruction {
        request.instruction_fragments.push(blocks.instruction_block);
    }
    if has_data {
        request.data_block = Some(match request.data_block.take() {
            Some(mut existing) => {
                existing.push_str("\n\n");
                existing.push_str(&blocks.data_block);
                existing
            }
            None => blocks.data_block,
        });
    }
    let assembly = &mut request.context_assembly;
    assembly
        .included_fragment_labels
        .extend(labels.iter().cloned());
    assembly.estimated_context_bytes = assembly
        .estimated_context_bytes
        .saturating_add(assembled.spent.0 as usize);
    for (label, reason) in &assembled.omissions {
        let text = match reason {
            harw_context::OmissionReason::OverBudget => "over-budget",
            harw_context::OmissionReason::BelowCeiling => "below-ceiling",
            harw_context::OmissionReason::ExcludedByProgram => "excluded-by-program",
            harw_context::OmissionReason::Superseded => "superseded",
        };
        assembly
            .omitted_fragment_labels
            .push(label.as_str().to_owned());
        assembly
            .omission_reasons
            .push((label.as_str().to_owned(), text.to_owned()));
    }
    labels
}

/// Setzt den Skill-Injektor der Sitzung nach einer echten Verdichtung zurück
/// (PL-95 S3): gelieferte Auszüge dürfen erneut kommen, das Sitzungsbudget
/// beginnt neu.
pub(super) fn reset_skill_injection_after_compaction(session: &AgentSession) {
    if let Some(injector) = session.skill_injector() {
        injector.reset_after_compaction();
    }
}

/// Verdichtet die Historie auf ein Token-Ziel
/// ([`crate::compaction::compact_for_budget`]) und persistiert das Ergebnis.
///
/// # Description
/// Gemeinsamer Weg für die reguläre Auto-Verdichtung ([`maybe_compact`]),
/// den Pre-flight-Wächter und die Wiederholung nach einer
/// Kontextüberschreitung. Nach einem erfolgreichen Lauf ist eine per
/// Modellwechsel vorgemerkte Verdichtung erledigt
/// ([`AgentSession::set_pending_compaction`]`(false)`). Ein No-op
/// (`outcome.no_op`) wird nicht persistiert.
///
/// # Returns
/// `Some(outcome)` nach einem erfolgreichen Lauf (auch No-op), `None`, wenn
/// die Verdichtung scheiterte (nur geloggt — ein Compact-Fehlschlag bricht
/// den Turn nie ab; der Aufrufer entscheidet, ob er ohne Verdichtung
/// weitermacht).
///
/// `overhead_tokens` ist der Nicht-Historien-Anteil des zuletzt gebauten
/// Requests (System-Prompt, Fragmente, Werkzeugschemata; Teil C): das Ziel
/// gilt dann für den ganzen Request, nicht nur für die Historie. Die
/// reguläre Auto-Verdichtung übergibt `0` (ihr Ziel ist ein Historienziel).
pub(super) async fn run_budget_compaction(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    target_tokens: u64,
    overhead_tokens: u64,
    reason: crate::auto_compact::CompactDecision,
) -> Option<crate::compaction::CompactionOutcome> {
    match crate::compaction::compact_for_budget(
        session,
        model,
        target_tokens,
        overhead_tokens,
        Some(reason),
    )
    .await
    {
        Ok(outcome) => {
            session.set_pending_compaction(false);
            if outcome.no_op {
                tracing::info!(
                    target_tokens,
                    reason = ?reason,
                    tokens_before = outcome.tokens_before,
                    "turn_loop.compaction_no_op",
                );
                return Some(outcome);
            }
            tracing::info!(
                target_tokens,
                reason = ?reason,
                tokens_before = outcome.tokens_before,
                tokens_after = outcome.tokens_after,
                summarized = outcome.summarized,
                elided_results = outcome.elided_results,
                "turn_loop.compaction_applied",
            );
            reset_skill_injection_after_compaction(session);
            if let Err(error) = store.save_history(session.id(), session.history()).await {
                tracing::warn!(%error, "turn_loop.compaction_save_history_failed");
            }
            Some(outcome)
        }
        Err(error) => {
            tracing::warn!(%error, reason = ?reason, "turn_loop.compaction_failed");
            None
        }
    }
}

/// Schätzt die Tokens der seit `mark` angehängten Verlaufs-Items, die in der
/// Nutzung der letzten Modellrunde noch nicht enthalten sind.
///
/// # Description
/// Assistant-Text und Tool-Calls ab `mark` stammen aus der Antwort selbst
/// und sind über `output_tokens` bereits gezählt; Reasoning geht nie an das
/// Modell. Gezählt werden alle übrigen Items (vor allem Tool-Ergebnisse) über
/// ihre JSON-Größe, umgerechnet mit der kalibrierten Bytes/Token-Rate der
/// Session.
pub(super) fn appended_tokens_since(session: &AgentSession, mark: usize) -> u64 {
    let bytes = session
        .history()
        .items()
        .iter()
        .skip(mark)
        .filter(|item| {
            !matches!(
                item,
                TurnItem::AssistantMessage(_) | TurnItem::ToolCall(_) | TurnItem::Reasoning(_)
            )
        })
        .map(|item| serde_json::to_vec(item).map_or(0, |encoded| encoded.len()))
        .fold(0_u64, |total, len| {
            total.saturating_add(u64::try_from(len).unwrap_or(u64::MAX))
        });
    session.token_calibration().bytes_to_tokens(bytes)
}

/// Geschätzte Tokens eines gebauten Requests außerhalb seiner Historie
/// (System-Prompt, Fragmente, Datenblock, Werkzeugschemata), mit derselben
/// Rechnung wie [`crate::compaction::CompactionBudget::with_request_overhead`].
pub(super) fn request_overhead_tokens(
    request: &ModelRequest,
    calibration: &crate::context_budget::TokenCalibration,
) -> u64 {
    crate::context_budget::estimate_request_tokens(request, calibration).saturating_sub(
        crate::compaction::estimate_history_tokens(&request.history, calibration),
    )
}

/// Fehler „Kontext erschöpft": der Request passt auch nach einer
/// Notfall-Verdichtung nicht in das Fenster des aktiven Modells.
///
/// # Description
/// Als [`crate::model::ModelError::ContextLength`] geformt (nicht
/// wiederholbar, dieselbe Kategorie wie eine Provider-Meldung), damit
/// Aufrufer und Beobachter ihn ohne neue Fehlervariante einordnen können.
pub(super) fn context_exhausted_error(
    estimated_tokens: u64,
    reserve_tokens: u64,
    window_tokens: u64,
) -> CoreError {
    CoreError::Model(crate::model::ModelError::ContextLength {
        message: format!(
            "Kontext erschöpft: die Anfrage umfasst geschätzt {estimated_tokens} Tokens, \
             mit {reserve_tokens} Tokens Antwort-Reserve passt sie auch nach einer \
             Notfall-Kompaktierung nicht in das Kontextfenster von {window_tokens} Tokens. \
             Bitte den Verlauf kürzen (/compact), einen neuen Verlauf beginnen oder ein \
             Modell mit größerem Fenster wählen."
        ),
    })
}

/// Harte Verdichtung am Beginn eines neuen Auftrags-Turns einer bestehenden
/// Orchestrator-Session (Addendum D).
///
/// # Description
/// No-op, wenn die Session keine Policy trägt oder
/// [`crate::auto_compact::AutoCompactPolicy::turn_start_target_tokens`]
/// `None` ist (der Default; nur Root-/Sub-Orchestrator-Sessions setzen ihn,
/// siehe `child_controller.rs`). Sonst wird die Verlaufs-Token-Zahl aus
/// [`crate::compaction::estimated_history_tokens`] geschätzt; überschreitet
/// sie das Ziel, läuft [`crate::compaction::compact_session`] mit einem
/// [`crate::compaction::CompactionPlan::for_target_tokens`]-Plan und
/// [`crate::auto_compact::CompactDecision::TurnStart`] als Grund, bevor die
/// erste Modellrunde dieses Turns beginnt. Das Ergebnis wird bei Erfolg über
/// `store.save_history` persistiert, wie bei [`maybe_compact`].
///
/// # Arguments
/// - `session` (`&mut AgentSession`): deren Historie ggf. ersetzt wird.
/// - `model` (`&dyn ModelProvider`): für den optionalen
///   Zusammenfassungs-Aufruf innerhalb von `compact_session`.
/// - `store` (`&dyn StateStore`): Ziel der Persistenz nach erfolgreichem
///   Compact.
///
/// # Concurrency
/// `async`; höchstens ein Modellaufruf und ein Store-Aufruf. Fehler beider
/// Seiten werden nur geloggt — ein Fehlschlag darf den Turn nie abbrechen.
pub(super) async fn maybe_hard_compact_at_turn_start(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
) {
    let Some(policy) = session.auto_compact().copied() else {
        return;
    };
    let Some(target_tokens) = policy.turn_start_target_tokens() else {
        return;
    };
    let estimated_tokens = crate::compaction::estimated_history_tokens(session.history());
    if estimated_tokens <= target_tokens {
        return;
    }

    let mut plan = crate::compaction::CompactionPlan::for_target_tokens(target_tokens);
    let (summary_provider, summary_model) = session.compaction_summary_model();
    plan.summary_provider = summary_provider.cloned();
    plan.summary_model = summary_model.cloned();
    match crate::compaction::compact_session(
        session,
        model,
        &plan,
        Some(crate::auto_compact::CompactDecision::TurnStart),
    )
    .await
    {
        Ok(outcome) => {
            tracing::info!(
                bytes_before = outcome.bytes_before,
                bytes_after = outcome.bytes_after,
                items_dropped = outcome.items_dropped,
                summarized = outcome.summarized,
                "turn_loop.turn_start_hard_compact_applied",
            );
            if !outcome.no_op {
                reset_skill_injection_after_compaction(session);
            }
            if let Err(error) = store.save_history(session.id(), session.history()).await {
                tracing::warn!(%error, "turn_loop.turn_start_hard_compact_save_history_failed");
            }
        }
        Err(error) => {
            tracing::warn!(%error, "turn_loop.turn_start_hard_compact_failed");
        }
    }
}

/// Prüft, ob der aktuelle Turn (alles ab der letzten `UserMessage`) einen
/// Plan-Schritt abgeschlossen hat.
///
/// # Description
/// Sucht rückwärts durch die Historie bis zur letzten `UserMessage` und
/// prüft jeden darin enthaltenen `ToolCall`, dessen `tool_name` mit `"plan"`
/// beginnt: gilt als „Plan-Schritt abgeschlossen", wenn die JSON-Argumente
/// (beliebig verschachtelt) ein Feld `"status"` mit dem Wert `"completed"`
/// oder `"done"` enthalten.
///
/// Zum Zeitpunkt dieser Implementierung existiert im Repository noch kein
/// konkretes Plan-Tool (`harw-plan`/`harw-plan-bridge` definieren bislang
/// keinen `ToolProvider`, siehe Bericht des Aufrufers) — Namenspräfix und
/// Argument-Form folgen daher wörtlich dem Vertrag, nicht einer bestehenden
/// Implementierung.
///
/// # Arguments
/// - `session` (`&AgentSession`): dessen Historie geprüft wird.
///
/// # Returns
/// `true`, wenn mindestens ein passender `ToolCall` im aktuellen Turn
/// gefunden wurde.
pub(super) fn turn_completed_a_plan_step(session: &AgentSession) -> bool {
    let items = session.history().items();
    let turn_start = items
        .iter()
        .rposition(|item| matches!(item, TurnItem::UserMessage(_)))
        .unwrap_or(0);
    items[turn_start..].iter().any(|item| {
        let TurnItem::ToolCall(call) = item else {
            return false;
        };
        if !call.tool_name.starts_with("plan") {
            return false;
        }
        json_contains_completed_status(&call.arguments)
    })
}

/// Sucht rekursiv in einem `serde_json::Value` nach einem Feld `"status"`
/// mit dem Wert `"completed"` oder `"done"`.
pub(super) fn json_contains_completed_status(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => map.iter().any(|(key, nested)| {
            if key == "status"
                && let serde_json::Value::String(status) = nested
                && (status == "completed" || status == "done")
            {
                return true;
            }
            json_contains_completed_status(nested)
        }),
        serde_json::Value::Array(items) => items.iter().any(json_contains_completed_status),
        _ => false,
    }
}
