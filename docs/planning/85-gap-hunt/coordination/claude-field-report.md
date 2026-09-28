# Field report: live harw session failures mapped to code

Source: a read-only verification run over Mia's live harw session (the transcript itself stays local).
Each symptom was traced to code and judged `defect` or `by-design`. Findings were then voted on by lens.
This is input to wave 2, not part of `r16-dev-integration`. Line numbers refer to `a856dea`.

## Verdicts

| Symptom | Verdict |
|---|---|
| `F2-job-error-count` | defect |
| `F1-slash-path` | defect |
| `F3-child-job-visibility` | by-design |
| `F5-awaiting-child-pause` | defect |
| `F4-host-lease-vs-tools` | defect |
| `F6-delegate-refused` | defect |
| `F7-budget-overshoot` | defect |
| `F9-archive-unreadable` | defect |
| `F8-web-search-allowlist` | defect |
| `F10-read-guard-cascade` | by-design |

## Confirmed findings (16)

- `harw-tui/src/app.rs:6945` — **Plain Enter clears the composer before classifying, and a rejected line is neither restored nor saved to history** (medium · input-loss · NEW:input-dropped-on-reject)
- `harw-tui/src/app.rs:9089` — **While a turn is running, rejected input is dropped silently with no system line** (low · input-loss · NEW:input-dropped-on-reject)
- `harw-runtime/src/job_wiring.rs:134` — **Let-chain in JobEventRouter::ancestors breaks the MSRV 1.85 rule (incidental, not the cause of F3)** (low · P8 · P8)
- `harw-core/src/turn_loop.rs:5534` — **Handoff tools are offered to, and run for, children whose lifecycle forbids pausing, so the turn ends in AwaitingChild and the child is failed** (high · lifecycle/tool-surface · NEW:handoff-surface-ignores-lifecycle)
- `harw-ops/src/sandbox_lease.rs:348` — **Lease grant message implies every child agent can now run host commands** (low · doc-drift · M3)
- `harw-core/src/child_approval.rs:343` — **Child approval relay rejects the turn-loop-generated delegation tools (agents.delegate, transfer_to_*, agents.catalog) as 'outside role'** (high · authorization · NEW:generated-tool-surface-check)
- `harw-core/src/turn_loop.rs:3796` — **Approved `agents.catalog` fails on resume with "no executor for tool 'agents.catalog'"** (medium · correctness · NEW:generated-tool-surface-check)
- `harw-registry-defaults/src/lib.rs:236` — **Delegation tools other than `delegate_wave` are missing from AUTO_APPROVED_TOOLS, so whether a delegation succeeds depends on approval mode, classifier and learned rules** (medium · consistency · M1)
- `harw-core/src/turn_loop.rs:987` — **Token budget checked only after billing, so the overshoot is one full round and not bounded by the budget** (high · budget-enforcement · M2)
- `harw-core/src/turn_loop.rs:1034` — **Tool calls of the overshooting round still run after the token budget is exhausted** (medium · budget-enforcement · M1)
- `harw-tool-fs/src/read.rs:345` — **fs.read dumps archives and other binaries as lossy text instead of refusing them with a hint** (medium · tool-contract · NEW:binary-read-without-sniff)
- `harw-tool-doc/src/provider.rs:63` — **No read-only tool lists or reads archive members, although the explorer classifies archives** (medium · capability-gap · NEW:missing-archive-reader)
- `harw-core/src/session.rs:872` — **apply_mode empties the session's network scope and keeps NetworkAccess, so every web.* call is denied** (high · defect · NEW:restrict-request-drops-network-scope)
- `harw-ops/src/research.rs:502` — **/research-web gives the researcher-web child reduce_to_read_only, which removes NetworkAccess from a role that only has web tools** (high · defect · M3)
- `harw-core/src/mode.rs:114` — **Plan mode keeps NetworkAccess and web.fetch but hides web.search** (low · defect · NEW:mode-toollist-omits-sibling-tool)
- `harw-tool-job/src/progress.rs:517` — **detect_severity counts passing libtest lines as errors ("error:" matches the path segment "error::"; "panicked" matches test names)** (medium · correctness · NEW:substring-severity-heuristic)

## Unverified (the verifier died at the session limit) (3)

- `harw-config/src/merge.rs:462` — **Untrusted repo config can raise or disable guards.orchestrator_read_limit (and every other guard threshold) when the home config leaves it unset** (medium · config-scope · M1)
- `harw-core/src/guard.rs:717` — **Read-budget hint says 'fast ausgeschöpft' on the last allowed read, although the next read is already denied** (low · guard-hint · NEW:misleading-guard-hint)
- `harw-registry-defaults/src/research_web.rs:150` — **researcher_web_network_scope has no production caller; the documented scope source for researcher-web is wrong** (low · doc-drift · M3)

## Rejected (3)

- `harw-registry-defaults/agents/executor.toml:60` — **executor description still says 'ausschließlich Sandbox-Prozesse' although a lease runs it on the host** (F4-host-lease-vs-tools)
  - Reason: The literal string match is real (executor.toml:60, profile.rs:2003-2004, and golden_ir/builtin-executor.json:6 all say the executor runs 'exclusively sandbox processes'), but the 'gap' framing is refuted. (1) The sandbox-lease/host-mode mechanism is exhaustively and deliberately documented in docs/design/mediated-process-execution.md, with explicit dated user decisions, mandatory per-grant user c
- `harw-tui/src/input.rs:39` — **A line starting with an absolute path is rejected as an invalid command name instead of being sent as chat** (F1-slash-path)
  - Reason: REFUTED — this is deliberate, documented behavior with a tested escape hatch, and the underlying need (referencing a path in chat) is already served by a different, fully-implemented mechanism.

Code confirms the mechanics as described: harw-tui/src/input.rs:30 routes every '/'-prefixed line to parse_command; input.rs:39-47 tokenizes and calls CommandName::parse on the first token; command.rs:15-2
- `harw-core/src/turn_loop.rs:5502` — **80 % wrap-up instruction is lost for roles without a context program and busts the prompt cache for roles with one** (F7-budget-overshoot)
  - Reason: Refuted. The finding's core premise — that uia-explorer/uia-worker "have none" [context]/ContextProgram and therefore fall through ModelRequest::with_context_program's fallback branch onto the dead `context` field — is factually wrong for these two named roles, and for every other built-in role in the registry.

Evidence:
- harw-registry-defaults/agents/uia-explorer.toml:32 and uia-worker.toml:44 

## Traces

### F2-job-error-count: defect

Verdict: defect. The job error counter uses a substring heuristic, `lower.contains("error:")`, in `detect_severity`. That substring also appears inside every Rust path segment `error::`. Every passing libtest line from a module named `error` therefore counts as an error line, for example "test error::tests::test_gone_classification ... ok". The workspace has 89 files that declare `mod error`, and harw-killer/src/error.rs:214 is the cited test. Doctest lines such as "src/error.rs - error::X (line N) ... ok" match the same way. That explains 208 and later 326 counted errors while grep for FAILED and panicked finds nothing. The count is not documented as approximate anywhere visible to the agent, so this is not by-design. progress.rs:13 says only that the detection is "best effort". JobMeta.errors is documented as "Zahl erkannter Fehlerzeilen" (model.rs:261). job.status emits it as a plain "errors" field (tools.rs:764) and job.list does the same (tools.rs:784).

Path:
1. The monitor feeds each output line, split on \r, into `detect_severity(segment)`: harw-tool-job/src/manager.rs:1516-1531.
2. On `Severity::Error` it runs `state.meta.errors += 1;` and `run.throttle.record_error(...)`: manager.rs:1534-1538. It counts warnings the same way at manager.rs:1540-1542.
3. The heuristic itself is at harw-tool-job/src/progress.rs:515-527. Decisive lines:
   `let is_error = lower.contains("error:") || lower.contains("error[e") || lower.contains("fatal error") || line.contains("FAILED") || lower.contains("panicked") || line.contains("npm ERR!");`
   and `let is_warning = lower.contains("warning:") || ...`
4. The value is exposed as `"errors": meta.errors` (tools.rs:764) and in the progress key and events (manager.rs:1561 and 1569).

Downstream effects of the same root cause:
- False error lines go through NotifyThrottle.record_error (throttle.rs:118). They become JobEvent::ErrorLines notifications to the owning agent (manager.rs:1555-1563), which raise `milestones`.
- job.wait wakes on "new error lines" (tools.rs:488-489), so a passing test run also produces fake error notes and early wait returns.

The existing unit table (progress.rs:674-697) covers only "test error_handling ... ok", where `error_` does not contain `error:`. It never covers the `error::` module-path case, so the gap went unnoticed.

A second false-positive class is `lower.contains("panicked")` on test names. Example: harw-tool-doc/src/error.rs:230 `test_extraction_panicked_message_names_page`, which matches twice over: `error::` and `panicked`. Warnings have the same `warning::` path problem.

The fix belongs in the existing crate harw-tool-job (layer "A", xtask/arch-policy.toml:350-351), and only in progress.rs. No new crate is needed.

I added no tests and edited no files. The central build must run:
- `cargo fmt --all`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p harw-tool-job`, including doctests
- the full workspace tests
- `cargo run -q -p xtask -- gates`

### F1-slash-path: defect

ANSWER. The rejection comes from the TUI's pure input classifier. Treating any line that starts with "/" as a command is documented, and so is the "\/" escape (docs/design/tui-command-contract.md:256-278 §3/§3.1 and :959). So the rejection alone is by-design in a narrow sense. The lost message is a defect, though, and so is the rejection of input that can never be a command.

- **Input loss.** On plain Enter the composer is cleared before the line is classified. A rejected line is then neither restored to the composer nor saved in the Up-arrow history.
- **Rejecting impossible commands.** The name grammar (command.rs:15-20, contract :38 `[a-z][a-z0-9-]*`) never allows a "/" in a command name. A first token such as "root/Harw-....zip" therefore cannot be a command, and nothing in the docs says such lines must be rejected rather than sent as chat.
- **Busy path.** When the same line is typed while a turn is running, it is dropped silently, with no system line at all.

TRACE (idle path, which matches the field text exactly)
1. harw-tui/src/app.rs:6930: plain Enter is caught before the InputEditor sees it.
2. app.rs:6944-6945: `let line = app.input.submission_text(); app.input.clear();`. The composer is emptied before classification.
3. app.rs:6948 calls `classify_line(&line)`, which goes to app.rs:849 `crate::classify_input(line)`.
4. harw-tui/src/input.rs:30: `'/' => parse_command(&line[1..])`. Only the first character is checked.
5. input.rs:40 `tokenize(input)?`, then input.rs:46-47 `CommandName::parse(name.to_owned())?`, with name = "root/Harw-...zip".
6. harw-tui/src/command.rs:15-20: the name is valid only if it is lowercase, digits or '-'. The '/' fails this and command.rs:25 returns `InvalidCommandName { input }`.
7. harw-tui/src/error.rs:52 formats `'{input}' is not a valid command name`, and app.rs:860 turns it into `LineAction::System(format!("Eingabe abgelehnt: {error}"))`. This is the exact text from the report.
8. app.rs:6951: `LineAction::System(text) => bus.send(HarwEvent::SystemMessage(text))`. There is no `remember_input` here; it only happens for Chat/Command at 6953 and 6958. The text is gone from the composer and from the history, which is the reported loss.

The command popup does not intervene: command_popup.rs:269-307 finds no match for "root/…", so Enter falls through to the composer.

Side paths:
- The secondary Submit path, app.rs:7043-7052 (InputEditor Enter, input_editor.rs:1170-1180), calls remember_input first, so the text at least lands in the history. It still ends in the same System line.
- Busy path: app.rs:9081-9091, `LineAction::Quit | LineAction::Ignore | LineAction::System(_) => BusyKeyOutcome::Redraw`. The rejection text is thrown away and the composer was already cleared (input_editor.rs:1179). The user gets no feedback at all.
- There is no "looks like a path" heuristic for "/" anywhere. The only one is for "@" mentions (mention.rs:353-355 `looks_like_path`). No code checks whether the file exists.
- Other callers of classify_input get the fix automatically: command_exec.rs:384 and :488, command_data.rs:66, harw-cli/src/lib.rs:760.
- A line like "/tmp is full" (one path segment) parses as the valid name "tmp". It becomes UnknownCommand, is remembered in the history, and is not lost. Only a file-existence check in the app layer could tell such a line apart; that is optional.
- A path containing an apostrophe (e.g. "/root/it's.zip") fails even earlier with UnterminatedQuote at input.rs:40/132. The path check therefore has to run on the raw first word, before tokenize.

WHERE NEW CODE LIVES. All changes go into the harw-tui crate, which xtask/arch-policy.toml:362-363 places in layer A.
- The classifier rule goes in harw-tui/src/input.rs.
- The composer and busy fixes go in harw-tui/src/app.rs, reusing `insert_paste` (app.rs:9066) and `push_system_text_exported` (app.rs:9045).
- Add one sentence to the contract in docs/design/tui-command-contract.md §3.1 (and the table at :959) so the docs don't drift from the code (M3).

TESTS / BUILD. This was a read-only mapping, so I added no tests and ran no builds. The central build must run:
1. `cargo fmt --all`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test -p harw-tui`, then the full workspace tests including doc tests
4. `cargo run -q -p xtask -- gates`

### F3-child-job-visibility: by-design

ANSWER: This is by design. job.status only shows a job to the session that started it and to that session's ancestors. Children and siblings of the creator are refused, and the refusal reads as "unknown job". The root session started job-20260927-183731-002, so the job's owner is the root with an empty ancestor list. The delegated root-orchestrator child is a descendant of the root, not an ancestor, so it can never pass the check. No mechanism exists anywhere in the code that lets a parent grant a child read access to its jobs. Relaying the status over agent.message is currently the only way.

TRACE (file:line):
1. The child has the job tools. Orchestrator roles without a shell get JOB_CONTROL_TOOLS (status, logs, stop, list, wait) through JobWiring::control_provider (/home/user/Harwness/harw-registry-defaults/src/profile.rs:2410-2418, role filter at 1090-1106). This is wired in /home/user/Harwness/harw-runtime/src/children.rs:1180-1210. The doc comment there says "Besitzprüfung: nur Jobs der eigenen Nachfahren" (children.rs:1178-1179). Root and all children share one JobManager (/home/user/Harwness/harw-runtime/src/job_wiring.rs:5-6, 244-246).
2. Ownership is fixed when the job starts: `let owner = JobOwner::new(session.as_str(), self.shared.lineage.ancestors(session));` (/home/user/Harwness/harw-tool-job/src/tools.rs:845-846). For the root session the lineage is empty: `if root == Some(session) { return chain; }` (job_wiring.rs:115-116). So the owner is {session: root, ancestors: []}.
3. The child calls job.status. The caller identity is taken from the child's own execution context: `let caller = Caller::Agent(context.session_id().as_str());` (tools.rs:1009), then manager.status (tools.rs:1010).
4. `JobManager::status` goes through `self.entry(id, caller)?` (/home/user/Harwness/harw-tool-job/src/manager.rs:879-880). Inside entry: `Some(entry) if caller.may_control(&lock(&entry.state).meta.owner) => Ok(entry), _ => Err(JobError::NotFound(..))` (manager.rs:571-576). `Caller::Agent(session) => owner.may_control(session)` (manager.rs:161-165).
5. This is the deciding rule, in /home/user/Harwness/harw-tool-job/src/model.rs:183-186: `!caller.is_empty() && (self.session == caller || self.ancestors.iter().any(|entry| entry == caller))`. The child's session is neither the root nor in [], so the result is false and NotFound is returned.
6. The message the child got is word for word manager.rs:300-302: "unknown job `{id}` (or not started by you or one of your sub-agents)". It deliberately does not distinguish "does not exist" from "not yours". job.list uses the same filter (manager.rs:887-891), so the job does not appear in the child's list either.

WHERE THIS IS DOCUMENTED:
- /home/user/Harwness/harw-tool-job/src/model.rs:149-151: "Steuern (`status`, `logs`, `stop`, `wait`) dürfen genau der Erzeuger und seine Vorfahren; Geschwister und Kinder des Erzeugers nicht."
- A unit test pins it: model.rs:355 `assert!(!owner.may_control("grandchild"));`
- The crate and module docs say the same: /home/user/Harwness/harw-tool-job/src/lib.rs:19-20 and manager.rs:22-23.
- The tool descriptions say "Status of one of your jobs" (tools.rs:401) and "List the jobs you (or your sub-agents) started" (tools.rs:483).
- The integration test shows a "stranger" being refused with "unknown job" (/home/user/Harwness/harw-tool-job/src/tools/tests.rs:247-271).
- Only the operator UI (/jobs) sees everything: `Caller::Operator => true` (manager.rs:156-164).

DOES A GRANT PATH EXIST? No.
- JobOwner has only `session` and `ancestors` (model.rs:163-169).
- Caller has only `Agent(&str)` and `Operator` (manager.rs:153-158).
- Nothing in the workspace matches share_job, job_ids, inherit_jobs, read_jobs or visible_jobs.
- The child factory has no job-sharing field.
- The design notes (/home/user/Harwness/docs/planning/README.md:348-372, "tool-facing ownership rules") describe no grant.

INCIDENTAL, NOT THE CAUSE: job_wiring.rs:134-135 uses a let-chain inside JobEventRouter::ancestors. The workspace MSRV is 1.85 (/home/user/Harwness/Cargo.toml:165), so that line breaks the no-let-chains rule and the MSRV build. It is reported as a separate low finding.

IF YOU WANT CHILDREN TO SEE PARENT JOBS: this is a policy change, so it needs a decision (DEC) first, not a bug fix.
- Suggested shape: add a read-only check, `JobOwner::may_read(caller, caller_ancestors)`. It is true when may_control holds, or when owner.session appears in the caller's ancestor chain (the caller descends from the creator).
- Resolve the caller's lineage at call time in tools.rs through `self.shared.lineage.ancestors(caller)`, and pass it via a new `Caller::Descendant { session, ancestors }` or a separate `JobManager::read_entry`.
- Use it only for status, logs, list and wait. stop keeps may_control.
- Possibly make it opt-in per delegation.
- All of this lives in harw-tool-job (model.rs, manager.rs, tools.rs). The runtime side (lineage source) is harw-runtime/src/job_wiring.rs. Both crates are layer A in /home/user/Harwness/xtask/arch-policy.toml:338-351, so no layer rule is affected.
- Test idea for that feature: root starts a job, a child whose lineage is [root] gets Ok from job.status, logs, list and wait, but NotFound from job.stop, and an unrelated session still gets NotFound.

I added no tests and edited no files. Commands the central build must run if the let-chain is fixed: cargo fmt --all; cargo clippy --workspace --all-targets -- -D warnings; cargo test -p harw-runtime job_wiring; cargo run -q -p xtask -- gates.

### F5-awaiting-child-pause: defect

ANSWER: The fail-closed check is intended behaviour. The defect is that the turn loop still offers the handoff tools, and executes calls to them, for a child whose lifecycle forbids pausing. The child here is a child-orchestrator with `allow_pause = false`. `delegate_wave` is a blocking tool, so the wave passes. The later call (`transfer_to_intel-web-researcher`, `agents.delegate{agent:"intel-web-researcher"}`, or `agent.message` to a finished wave child) goes down the handoff path. That path ends the child's turn with `TurnOutcome::AwaitingChild`, and the spawner rejects it as a lifecycle violation. The runtime advertised the tool and told the model to use it, so this is not a mistake in the agent definition.

MAP (exact path):
1. Lifecycle state machine. `LifecycleMachine { allow_pause, allow_rerun, max_attempts }` is at harw-agent-dsl/src/executable.rs:546-553. Its contract is at :528-541: "Steht `allow_pause` auf `false`, darf ein Kind niemals in einen Wartezustand wie `AwaitingApproval` oder `AwaitingChild` übergehen … die Runtime bricht die Sitzung fail-closed ab".
   - Every child orchestrator inherits `allow_pause = false` from harw-registry-defaults/agents/child-orchestrator-base.toml:47. The same file admits `delegate_wave` (:74) and `agent.message` (:80).
   - The flag is copied into `ChildRecord.allow_pause` at harw-core/src/child_controller.rs:7673 (field at :855).
2. Turn states. `TurnOutcome::AwaitingChild` is at harw-core/src/turn_loop.rs:1125. It is the only transition into awaiting_child, via the handoff branch:
   - `delegation_step` (turn_loop.rs:2373-2409) treats three calls as a handoff: `transfer_to_*` (:2388), `agents.delegate` (:2390-2396) and `agent.message` to your own finished child (`message_resume_handoff`, :2783-2807).
   - The handoff branch runs at :4869-4988: `spawner.spawn_child(...)` at :4924, then `session.begin_handoff(...)` at :4974 ("state ⇒ WaitingForChild; Loop pausiert"), then `return Ok(TurnOutcome::AwaitingChild{..})` at :4984.
   - The same pattern after an approval: :3868/:3882.
   - Wave children are recorded as resumable at child_controller.rs:5893-5894. That is why `agent.message` to a wave child also becomes a handoff.
3. Why the child sees the handoff tools. When the request is built (turn_loop.rs:5527-5548), `append_handoff_tools(session, &mut tools, &delegation.targets)` runs for every session that has a spawner (:5534). The targets come from `ManagedAgentSpawner::delegation_targets_for` (child_controller.rs:7376-7470). That filter checks role, depth and plan mode, but never `allow_pause`.
   - `append_handoff_tools` (turn_loop.rs:2057-2104) adds `transfer_to_<target>` for up to 8 targets, otherwise `agents.delegate` (threshold at delegation_visibility.rs:147), plus `agents.catalog`.
   - `may_offer_generated_tool` (:2016-2027) offers a tool unless the profile admits it AND explicitly disables it, so `[tools].admitted` does not stop them.
   - The prompt block (:1283-1293) even says "Delegierbare Ziele (transfer_to_<name>): …".
   - `SpawnContext` (session.rs:337-368) has no pause flag. No code in turn_loop.rs reads `allow_pause`.
4. The check that fires is child_controller.rs:5834-5846. `pause_label` maps `AwaitingChild` to "awaiting_child" (:7085), then:
   `Some(label) if !record.allow_pause => { let reason = format!("child paused but its lifecycle forbids pausing: {label}"); self.set_failed(child, &reason); ... ChildEndCause::PauseForbidden(...)`.
   - The German prefix comes from child_comms.rs:496-497: `"unzulässige Pause: {}"`.
   - `AwaitingApproval` has a relay for no-pause children (:5751-5767, child_approval.rs). `AwaitingChild` has none.
5. Delegate_wave works because it is a normal blocking tool call, per harw-core-bridge/src/delegate_wave.rs:8-11: "ein Handoff übergibt den Turn an das Kind (`TurnOutcome::AwaitingChild`), `delegate_wave` ist ein gewöhnlicher, blockierender Werkzeugaufruf".

WHY THIS IS A DEFECT AND NOT BY-DESIGN: The codebase's own rule is not to advertise tools that a child with `allow_pause = false` can never finish (harw-registry-defaults/src/profile.rs:3203-3206, :1377-1379, tests/tool_admission_coverage.rs:16). The handoff surface breaks that rule, so the model is steered into a path that is certain to fail. Built-in guidance makes it worse: wargaming-orchestrator/system.md:16 recommends `agent.message` / `continue_from` towards children. No test covers `awaiting_child` for a no-pause child. The only fail-closed test is `a_child_forbidden_to_pause_fails_closed_on_a_paused_turn`, for `awaiting_approval` (child_controller.rs:10850-10878).

Side observations (unclear, not reported as findings):
- `[delegation].targets` narrows only `delegate_wave`, via `DelegateWavePolicy` (harw-runtime/src/children.rs:1285-1296). It does not narrow the handoff/catalog surface. So a family orchestrator whose declared targets leave out intel-web-researcher can still be offered it. matrix-game-master.toml works around this by listing `transfer_to_*` under `forbidden`.
- The grandchild admitted at turn_loop.rs:4924 before the parent fails may never run. Not verified.

WHERE THE FIX SHOULD LIVE (xtask/arch-policy.toml):
- New trait hook `AgentSpawner::caller_may_pause(&self, caller) -> bool`, default `true`, in harw-extension-api/src/capabilities.rs (layer I).
- Its implementation in harw-core `ManagedAgentSpawner` (layer A), read from `ChildRecord.allow_pause`, with root/external root = true.
- The forwarder in harw-runtime/src/children.rs `DeferredManagedSpawner` (layer A, next to :2454-2485).
- The gate in harw-core/src/turn_loop.rs.
- Do NOT make `delegation_targets` empty for these callers: `delegate_wave` needs it (delegate_wave.rs:860).
- This fix touches several files (P2 contract wave).

I added no tests and edited nothing. Commands the central build must run: `cargo fmt --all`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace` (including doc tests, at minimum `-p harw-core -p harw-extension-api -p harw-runtime -p harw-core-bridge`); `cargo run -q -p xtask -- gates`; `cargo deny check`.

### F4-host-lease-vs-tools: defect

The tool boundary works as designed, and no role that is meant to run host commands lacks a way to do so. Both children told the truth: `root-orchestrator` and `uia-explorer` forbid `shell.exec` on purpose. What is wrong is the text the models use to pick a child: the grant message does not say that only roles that have `shell.exec` gain anything, and the `executor` description still says it runs only sandbox processes. Both are low-severity M3 (doc drift) defects.

**What the lease does.** The grant text is in `/home/user/Harwness/harw-ops/src/sandbox_lease.rs:346-351`:
`handles.registry.mark_global_approval();` followed by "Sandbox-Lease erteilt, …; shell.exec läuft jetzt auf dem Host (für diese harw-Sitzung inkl. aller Kind-Agenten)." It only sets a process-wide flag. That flag is checked in `/home/user/Harwness/harw-tool-shell/src/exec.rs:604-612` (`determine_effective_host`), which decides where an existing `shell.exec` call runs. It never adds a tool to a role. Every child registry does get the wiring (`harw-registry-defaults/src/profile.rs:2523-2531`, `harw-runtime/src/assembly.rs:2300`, `harw-runtime/src/children.rs:1704`), so the lease does reach every child that has `shell.exec`. The design doc says the same: "every `shell.exec` call … and all its child agents runs on the host" (`docs/design/mediated-process-execution.md:94-99, 129-138`).

**Per-role tool sets (by design):**
- `root-orchestrator.toml:110` `forbidden = [..., "shell.exec", ...]`; profile `Planning` (`profile.rs:1967-1970`); its rulebook says "Kein Schreiben, kein `shell.exec`, kein Web — das tun Worker" (`knowledge/roles/root-orchestrator.md:18`).
- `uia-explorer.toml:78-81` forbids `shell.exec`; its header says "kein `shell.exec`" (lines 18-21). Profile `UiaExplorer` (`profile.rs:1770-1776`). The UIA's `explore` op starts it and describes it as "read-only Explorer-Kindagent" (`harw-ops/src/explore.rs:499, 565`).

**Paths that do exist:**
- From the UIA: `uia.md:10` says "Frage/Shell/`cargo test` → `uia-worker`", and sudo goes to `uia-shell-worker` (`:13`). Both admit `shell.exec` (`uia-shell-worker.toml:61`; `profile.rs:2020, 2030`).
- From the root orchestrator: `[delegation].targets` includes "executor" (`root-orchestrator.toml`), `executor` maps to the `ShellExecution` profile (`profile.rs:2009`), and under a lease its `shell.exec` runs on the host.

So the UIA sent host work to the two read-only children. That is a model routing mistake, but two texts make it likelier:

1. **Grant message.** "inkl. aller Kind-Agenten" in `sandbox_lease.rs:348-351` (also in `:304-305`, `:360-361` and the summary at `:191`) reads as "every child can now run host commands". It names neither the roles that actually have `shell.exec` nor the delegation targets to use. `uia.md:49-53` (the "Sandbox & Host-Zugriff" section) also says nothing about routing after a grant.
2. **`executor` description.** `executor.toml:60` still says "Führt ausschließlich eng beauftragte Sandbox-Prozesse aus". That has been false since the process-wide lease (2026-09-21). The text reaches `transfer_to_*` descriptions and `agents.catalog` (`assembly.rs:5113-5122`). `root-orchestrator.md` never names `executor` as the way to run shell or host commands, and never mentions the lease. The `delegate_wave` role field only points to `agents.catalog` (`delegate_wave.rs:1121-1122`).

**Where the fixes live.** Both crates are layer A in `xtask/arch-policy.toml` (`harw-ops` at :324, `harw-registry-defaults` at :336). Only text and data change; there is no new API.

**Build rule.** I ran no cargo or rustc and added no tests. The central build must run: `cargo fmt --all`; `cargo clippy --workspace -D warnings`; `cargo test -p harw-ops -p harw-registry-defaults` (including the golden IR test `tests/context_program_golden.rs` and `embedded_agents` tests) plus the full workspace tests and doctests; `cargo run -q -p xtask -- gates`; `cargo deny check`; `make -C dod clippy test`; `actionlint`.

### F6-delegate-refused: defect

ANSWER. The refusal is deterministic, and it comes from the child-approval relay, not from the role's rights. When the root-orchestrator child calls the turn-loop-generated tool `agents.delegate`, that call needs an approval. The relay then checks the "role surface" with `find_executor`, which only knows registered executors. `agents.delegate` never has one, so the relay rejects it with REASON_TOOL_OUTSIDE_ROLE, the text the child paraphrased. The earlier run worked because the model picked `delegate_wave` instead. That tool is admitted, registered and auto-approved, so it never reaches the relay.

MAP (exact path)
1. Offer. The root-orchestrator admits `delegate_wave` but not `agents.delegate` (harw-registry-defaults/agents/root-orchestrator.toml, [tools].admitted). Separately, the turn loop adds generated delegation tools from the spawner's visible targets. With more than 8 targets it offers only `agents.delegate` (harw-core/src/delegation_visibility.rs:147 `DELEGATE_TOOL_THRESHOLD = 8`; harw-core/src/turn_loop.rs:2076-2081). This happens even though the role's allowlist lacks it: `may_offer_generated_tool` suppresses a tool only if the profile admits it AND activation disabled it (turn_loop.rs:2021-2026). By design, the spawner rules are the authority here (turn_loop.rs:2033-2039). The root-orchestrator's candidates are every Worker role, 3 child orchestrators and agent-steward (child_controller.rs:7447-7457), so it is always well over 8. Depth only matters at remaining_depth == 0, where no tool is offered (child_controller.rs:7443-7445).
2. Approval before dispatch. Every call first goes through `check_approval` (turn_loop.rs:4741). Handoff and `agents.delegate` detection only runs after that (turn_loop.rs:4837). `agents.delegate`, `agents.catalog` and `transfer_to_*` are not in AUTO_APPROVED_TOOLS, while `delegate_wave` is, with the rationale "Ein Child-Orchestrator läuft mit allow_pause = false und könnte eine Rückfrage nie beantworten" (harw-registry-defaults/src/lib.rs:225-236). What happens next depends on the approval mode (lib.rs:824-889):
   - FullAccess: Allow, so the spawn works.
   - AlwaysAsk: AskUser.
   - Delegated with auto gate: an LLM classifier decides. With a TUI relay present, deny or ask becomes AskUser (runtime/auto_classifier.rs:1885-1901).
3. Relay. root-orchestrator has `allow_pause = false`, so the child controller calls `relay_child_approvals` (child_controller.rs:5751-5766). The relay then does:
   `if find_executor(session, &pending.call.name).is_none() { ... reject(REASON_TOOL_OUTSIDE_ROLE) }` (harw-core/src/child_approval.rs:343-353)
   The constant is "Freigabe nicht möglich: dieses Werkzeug gehört nicht zur Werkzeugfläche deiner Rolle" (child_approval.rs:78-79). No provider registers `agents.delegate` or `agents.catalog`; they exist only as turn_loop constants (turn_loop.rs:1825, 1829). The turn loop itself dispatches `agents.delegate` only when `find_executor(..).is_none()` (turn_loop.rs:2390). The resume path would spawn it correctly after an Approve (turn_loop.rs:3793-3796). So the relay blocks a call the loop offered and could execute, and it never asks the user.
4. Why answers differ for the same role:
   - (a) The model's choice between `delegate_wave` (always works) and `agents.delegate` (broken in pause-forbidden children).
   - (b) The approval mode: FullAccess works, ask is refused, auto depends on the classifier.
   - (c) Learned rules. The UIA root asked for its own `agents.delegate` call, and "Künftig erlaubt (dauerhaft im Projekt)" (harw-tui/src/app.rs:8278) stored an allow rule. The AllowRuleSet is shared unchanged with children (harw-runtime/src/approval.rs:508-514, 584). From then on, the child's DefaultApprovalPolicy returns Allow via that rule (lib.rs:884) and the relay is never reached. So the same role will now "work", which makes the bug look flaky.
   The existing test only covers a registered tool outside the surface (harw-core/src/child_controller/tests/teil_o.rs:303-343). There is no test for generated delegation tools.

This is not by-design: `agents.delegate` is catalogued as an always-available spawn tool (harw-registry-defaults/src/capability_catalog.rs:399, 415-422). It is not an environment issue either: the path is pure code.

WHERE NEW CODE LIVES. All affected crates are layer A (xtask/arch-policy.toml:282-283 harw-core, 336-339 harw-registry-defaults/harw-runtime):
- Surface predicate for generated delegation tools: harw-core/src/turn_loop.rs, next to `delegation_step`, exported `pub(crate)`; call it from child_approval.rs.
- Auto-approval list: harw-registry-defaults/src/lib.rs.
- Tests: harw-core/src/child_controller/tests/teil_o.rs and the lib.rs test module.

Fixes must avoid let-chains. The existing `delegation_step` already uses them (turn_loop.rs:2390-2391, 2402-2403) with rust-version 1.85 (Cargo.toml:165). This is widespread (34 files) and outside this symptom, so it is not reported as a finding.

TESTS ADDED: none (read-only brief). The central build must run, in the cloud environment (CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0):
- cargo fmt --all
- cargo clippy --workspace --all-targets -- -D warnings
- cargo test -p harw-core -p harw-registry-defaults -p harw-runtime (including doctests)
- cargo run -q -p xtask -- gates
- cargo deny check

### F7-budget-overshoot: defect

ANSWER: Defect. Each child's token budget is checked only once per model round, before the model call, and only against usage that has already been billed. Nothing projects the cost of the request about to be sent. So the overshoot equals the fresh tokens of one whole round. That amount is bounded by the tool-result caps (up to 64 KiB per result, times the number of calls in one response) plus output, not by the budget. This contradicts the module's own promise that only estimation error can push past the limit (harw-core/src/child_handoff.rs:182-185: "sodass das Limit insgesamt eingehalten bzw. nur um Schätzfehler überschritten wird"). A second defect makes it worse: the 80 % wrap-up instruction never reaches uia-explorer or uia-worker, so these children never wind down.

MAP
1. Budget per role (configuration values, by design): harw-registry-defaults/agents/uia-explorer.toml:92-95 (max_tokens = 60000, max_tool_calls 40) and uia-worker.toml:126-132 (20000, 16). They are read through executable_agent_ir, then spawn_contract().budget(), then budget_from_spec (harw-core/src/child_controller.rs:7668-7671, 7062-7078). A parent cannot raise them (harw-core-bridge/src/delegate_wave.rs:449 "effort, budget and sandbox of a child are fixed by its role"). A continuation gets a fresh budget, at most 3 times (child_handoff.rs:28-36, 87).
2. Reserve: enforce_child_budget subtracts handoff_reserve_tokens (child_controller.rs:5089-5104; formula at child_handoff.rs:190-194). 60000 leaves a turn cap of 52000 (8000 reserve). 20000 leaves 15000 (5000 reserve, the 25 % ceiling).
3. Check: TurnControl::model_checkpoint (harw-core/src/turn_loop.rs:987-1031) compares used_before + meter.usage.fresh_tokens() against the cap. It is called only at turn_loop.rs:4140, before the request is assembled. Usage is recorded after the response (4311). tool_checkpoint (1034-1054) ignores the token budget. "Fresh" means uncached input plus cache writes plus output (harw-types/src/usage.rs:55-76).
4. Result: child_controller.rs:5230-5315 detects the stop, runs compaction and formats the header (child_handoff.rs:420-424). "verbraucht" excludes the compaction call, by design (child_handoff.rs:404).

WHY THE NUMBERS FIT: explorer 84218 − 52000 means the last round had ≥ 32218 fresh tokens. That is about two tool results at the per-result cap CHILD_TOOL_RESULT_MAX_BYTES = 64 KiB (child_controller.rs:1367, 8411-8412), roughly 16k tokens each. worker 35312 / 30682 against 15000 means one round of 15-20k, which one fs.read or web result can produce alone. The checkpoint before that round cannot see the new tool results because they have not been billed yet.

WRAP-UP IS LOST: assemble_round_request pushes TOKEN_BUDGET_WRAP_UP_INSTRUCTION as a context fragment (turn_loop.rs:5502-5511). Roles without a [context] table get no context program (child_controller.rs:8179-8181), and the uia TOMLs have none. For those roles with_context_program falls back to with_context_budget (harw-core/src/model.rs:364-372). That path puts the fragments into ModelRequest::context, and model.rs:57-60 states that no wire builder reads that field. The builders only read system_prompt and instruction_fragments (harw-provider-http/src/anthropic.rs:565-569, lib.rs:2903-2906). The only test for this (child_controller.rs:14039-14044) counts `format!("{:?}", request.context)` as "seen", which hides the gap.

On the program path (roles with [context], for example researcher-web at 60000), the instruction block is appended to the system prompt (model.rs:314-320). The system prompt carries a cache breakpoint (harw-provider-http/src/cache_strategy.rs:291-301). So at 80 % the whole cached prefix becomes a cache write again and counts as fresh. The billed usage for the entire context lands in one round.

BY DESIGN / CONFIGURATION, not defects: the budget sizes and the 2-3 continue_from rounds they cause (TOML values plus the 25 % reserve on small budgets); a per-round checkpoint as such (documented at turn_loop.rs:650-655 and child_controller.rs:5084-5086); compaction tokens left out of "verbraucht".

WHERE THE FIX GOES: harw-core (layer A, xtask/arch-policy.toml:282-283), all in harw-core/src/turn_loop.rs. No new crate or cross-ring dependency. Budget values stay in the harw-registry-defaults TOMLs if the operator wants larger ones.

Aside, outside F7: harw-provider-http/src/cache_strategy.rs:187-188 and 291-317 use let-chains while rust-version is 1.85 (Cargo.toml:165). That is a P8 issue for a separate wave.

Build rule: read-only. No files edited, no tests added. For the fixes below, the central build must run: cargo fmt --all; cargo clippy --workspace --all-targets -- -D warnings; cargo test -p harw-core (including doctests) and -p harw-provider-http; cargo run -q -p xtask -- gates.

### F9-archive-unreadable: defect

Two defects: fs.read dumps a classified archive as lossy text instead of refusing it, and no read-only tool can list or read archive members. Both are code problems, not configuration or host issues.

1) Classification is correct. /home/user/Harwness/harw-explorer/src/filetype.rs:157-158 maps "zip|tar|gz|tgz|xz|txz|bz2|tbz2|7z|zst|rar|crate|jar|war|whl|lz4|lzma" to FileKind::Archive. For unknown extensions, filetype.rs:177 uses the infer magic bytes as the fallback. harw-explorer/src/lib.rs:58 gives the label "archive". explore.find puts that label in each match (/home/user/Harwness/harw-tool-explorer/src/provider.rs:254), and its description (provider.rs:448-453) does not say how to open such a file.

2) fs.read never looks at file content. The docs promise a type check: "Typ-Check" (/home/user/Harwness/harw-tool-fs/src/read.rs:7) and "prueft den Typ" (read.rs:249). The spec says "Liest eine Textdatei" (/home/user/Harwness/harw-tool-fs/src/provider.rs:296). The only check is `if !metadata.is_file()` (read.rs:345). After that the file goes straight to the chosen mode (read.rs:354-358). Byte mode returns `String::from_utf8_lossy(&raw)` (read.rs:395). Line and tail mode call decode_line, which returns `String::from_utf8_lossy(bytes)` (read.rs:601). A ZIP therefore comes back as up to 400 "lines" or 64 KiB of U+FFFD and control bytes, which is the reported symptom. The spec's only non-text hint is "Fuer PDFs doc.read_pdf verwenden" (provider.rs:302), and nothing enforces even that. The sibling tool fs.grep does have a guard: BINARY_SNIFF_BYTES = 8192 (grep.rs:116) and a NUL check that skips the file (grep.rs:248-252).

3) No archive tool exists. None of the harw-tool-* crates or harw-tools mention archive, zip or unzip in their sources. harw-tool-doc offers only doc.read_pdf (harw-tool-doc/src/provider.rs:63, tool.rs:498). Nothing in docs/ plans an archive tool or documents that fs.read returns binary as lossy text. The fs.read tests have no binary fixture; the only related assertion checks that there is no U+FFFD on valid UTF-8 (read.rs:916). So this is not by-design, and a shell worker with unzip -p was the only way in. The zip crate 8.6.0 (feature deflate) is already in Cargo.lock:8320, pulled in by harw-browser-thirtyfour/Cargo.toml:28; flate2 is at Cargo.lock:1663.

Should fs.read refuse? Yes, in every mode, with a hint. Lossy decoding makes the bytes useless even in byte mode. The rule should be: refuse when the file matches an archive, PDF or image signature, or when a NUL byte appears in the first 8 KiB (the same check fs.grep uses). Text that is not UTF-8 but has no NUL bytes (Latin-1 logs) should still be read lossily, so existing behaviour does not regress.

Where new code should live (xtask/arch-policy.toml): harw-tool-fs (line 348), harw-tool-doc (line 344), harw-tool-explorer (line 346) and harw-explorer (line 286) are all layer A, and A.may_depend_on = ["*"]. The binary check belongs in harw-tool-fs/src/tree.rs next to read_bounded (line 260), as one helper shared by read.rs and grep.rs. The archive tools fit best in harw-tool-doc (doc.list_archive, doc.read_archive_member), next to doc.read_pdf, reusing its safe-open code (tool.rs:150) and size cap. A new crate would need a new [packages] entry.

No files were edited, no tests were added, and nothing was built. Once the fixes land, the central build must run: cargo fmt --all; workspace clippy with -D warnings; cargo test -p harw-tool-fs -p harw-tool-doc -p harw-tool-explorer plus doc tests; cargo run -q -p xtask -- gates (the arch gate matters if a new crate or dependency is added); cargo deny check (if tar is added); actionlint.

### F8-web-search-allowlist: defect

**Answer: this is a code defect.** With the default configuration, no harw session can reach its own search backend, and in fact no host at all. Configuration and the root sandbox include html.duckduckgo.com. The session layer then empties the host scope but keeps NetworkAccess. The host lease has nothing to do with it.

**Search backend.** The default provider is `duckduckgo` (harw-config/src/web_toml.rs:175). Its endpoint is `https://html.duckduckgo.com/html/` (harw-tool-web/src/search.rs:113). web.search checks that host by hand (search.rs:1005-1013) through `require_host_access`. That function first checks NetworkAccess (harw-tools/src/sandbox_guard.rs:163-171) and then the host scope (:172-175). The second check is the error in the report: "Host '…' ist nicht in der erlaubten Host-Liste dieser Sandbox". So the failing sandbox had NetworkAccess but a scope without the DDG host.

**The allowlists it has to pass, all correct on paper:**
- **Process egress policy:** `install_web_tools` always adds the backend host (harw-registry-defaults/src/research_web.rs:330-335, 347-355, 364).
- **Root sandbox scope:** `root_network_scope` (harw-runtime/src/sandbox.rs:258-273) adds the search host when the lists are non-empty. By default they are: `[research].network_allow_hosts` = docs.rs, crates.io, doc.rust-lang.org, static.crates.io (harw-config/src/research_toml.rs:84-91). The assembly builds the root from this (harw-runtime/src/assembly.rs:1897-1901), and the tests at sandbox.rs:560-572 and rights_matrix.rs:353-381 show DDG is in the root scope.

**Where it breaks.** The root session is built with `.with_spawn_context(self.spawn_context.clone())` (assembly.rs:6022). That calls `apply_mode` (harw-core/src/session.rs:694-696), and line 872 does:
`context.sandbox = base.restrict(&PermissionRequest::from_permissions(ceiling.iter()));`
- `from_permissions` sets `network_scope: NetworkScope::empty()` (harw-authority/src/lib.rs:71-75).
- `restrict` intersects the scopes (lib.rs:882-887), so the result is always empty.
- The Chat, Work and Shell ceilings keep NetworkAccess (harw-core/src/mode.rs:351-356).
- Tools run against exactly this sandbox (harw-core/src/turn_loop.rs:3283-3289).
- Children are reduced from it, so they inherit the empty scope (harw-core-bridge/src/agent_tool.rs:2130-2135), and `create_governed_session` wipes it again for each child (harw-core/src/session_manager.rs:71-72).

This is a regression from ff9c499 (2026-09-21). Before it, `SandboxSpec::restrict(&PermissionSet)` explicitly kept the network scope ("Der NetworkScope bleibt unverändert", harw-sandbox/src/lib.rs at ff9c499^:1115-1133). `narrowed_sandbox` gets the same migration right with `.with_network_scope` (assembly.rs:1314-1325); session.rs does not. harw-core has no test that looks at `network_scope` at all.

**The two symptom paths:**
- **The research_web failure:** in a TUI (UIA) session, `/research-web` runs `uia-explorer` (harw-ops/src/research.rs:476-484) with `reduce_to_read_workspace_network`. That keeps NetworkAccess and the already empty parent scope, which gives exactly the reported host-list error.
- **The researcher-web child:** from a non-UIA caller, `child_reducer("researcher-web")` falls through to `reducer_for_role`, which returns `reduce_to_read_only` (research.rs:502-507, harw-ops/src/explore.rs:99-106). That becomes `{ReadWorkspace}` with an empty scope (agent_tool.rs:1986, 2055-2060). The child's only tools are web.* (harw-registry-defaults/src/profile.rs:2607-2610), and every call is denied. This contradicts the registry mapping `researcher-web → ReadNetwork` (harw-registry-defaults/src/authority.rs:428), delegate_wave (harw-core-bridge/src/delegate_wave.rs:1244) and the role's own TOML ("Reducer: `reduce_to_read_network`"). In Plan mode, web.search is additionally hidden (finding 3).

**Host lease: by design, and irrelevant here.** It is the shell host-work phase (harw-runtime/src/auto_classifier.rs:1134-1138, 1190-1195). harw-tool-shell has no NetworkScope or NetworkAccess reference, so an active lease never widens egress.

**Side note.** Compiled agents build their root scope only from manifest hosts, without the search host (assembly.rs:1883-1889). That is the explicit manifest choice, so not reported as a finding.

**Where the fixes live.** All four go into existing layer-A crates (xtask/arch-policy.toml: harw-core :282, harw-ops :324, harw-registry-defaults :336, harw-runtime :338). No new crate is needed. Fix 1 is the root cause. Fix 2 is independent of it.

I added no tests and edited no files. The central build should run, with the session's single build setting:
- `cargo fmt --all`
- `cargo clippy --workspace --all-targets -D warnings`
- `cargo test -p harw-core -p harw-ops -p harw-runtime -p harw-registry-defaults -p harw-core-bridge -p harw-tools -p harw-tool-web` (including doc tests)
- `cargo run -q -p xtask -- gates`
- `cargo deny check`

### F10-read-guard-cascade: by-design

The F10 guard behaved as designed and documented. It did not cause the failed plan: the explorers' fixed role budgets (F7) did, and the guard has no coupling to those budgets. Two adjacent defects turned up on the guard's config and hint path; they are listed as findings.

**Where the message comes from**
- `SessionGuardState::observe_orchestrator_read` (harw-core/src/guard.rs:684-727) counts each read tool per session (line 696).
- It returns Deny when `count > limit` (706-716) and Warn when `count >= warn` (717-724). The warn text is "Lesezugriff {count} von {limit} – der eigene Überblick ist fast ausgeschöpft" (721-722).
- Defaults are warn 4 and limit 5 (guard.rs:230-231). A read tool is one in `ORCHESTRATOR_READ_TOOLS` or with prefix `explore.`/`deps.source_` (582-594, 622-627).
- The turn loop asks this guard before approval, so a denied read never reaches the approval handler (turn_loop.rs:4730-4742 via `apply_session_guard` 3080-3126).
- It applies only when `session_guard_applies` (3055-3060) and `is_read_budgeted_orchestrator` (session.rs:1206-1214) are true. That means child sessions with role `RootOrchestrator` or `ChildOrchestrator`. The "root-orchestrator" in the report is such a child, reached via `transfer_to_root-orchestrator`.

**Why this is documented, not a bug**
- The role prompt says: "Überblick max. 5 Lesezugriffe (README, Baum, Manifest), Details immer delegieren (`delegate_wave`)" (harw-registry-defaults/knowledge/roles/root-orchestrator.md:14-15).
- The config docs say the same (harw-config/src/harness_config.rs:391-399), and the field table marks it "Profil darf nur verengen" (scope.rs:404-406).
- The existing test pins the behaviour: warn on reads 4 and 5, deny read 6 (guard.rs:1037).

**No stale counter across attempts**
- Each admit creates a new governed session (child_controller.rs:8141), which starts with a fresh `SessionGuardState::default()` (session.rs:623).
- A continuation is a new child with a fresh budget (delegate_wave.rs:61-68).
- The "4 attempts" match `max_attempts = 4` in harw-registry-defaults/agents/root-orchestrator.toml:23. Each attempt therefore had 5 fresh reads.

**How it interacts with delegation budgets (it doesn't)**
- The read guard only counts calls. It knows nothing about delegation.
- Every child in a wave is capped at the caller's remaining budget (delegate_wave.rs:889 and 661-663). That cap is combined with the child's own agent-definition budget, and the tighter limit wins per dimension (agent_tool.rs:1283-1331).
- For explorers the tighter value is always the role's own 60000 tokens, 40 calls, 180 s (harw-registry-defaults/agents/explorer.toml:71-73). The root orchestrator's 1.5M (root-orchestrator.toml:137) never binds.
- The model cannot give a wave more budget: `delegate_wave` rejects a `budget` field ("effort, budget and sandbox of a child are fixed by its role, never by tool arguments", delegate_wave.rs:447-450).
- So the cascade runs like this: after 5 reads the orchestrator has to delegate. The explorers get broad tasks and hit their fixed 60k/40/180 s cap (F7). The orchestrator still cannot read for itself, so its only options are `continue_from` or new waves. The failing part is F7's explorer budget and the task split, not F10's counter.

**Should it count bytes or turns, or be configurable per task?**
- Today it is configurable only globally. `[guards] orchestrator_read_limit` / `orchestrator_read_warn` in the trusted home layer are set freely (merge.rs:427-431, guard_wiring.rs:292-297). `0` turns the budget off (guard.rs:690-695).
- The spawner hands the same `GuardPolicy` to every child (child_controller.rs:8430). Root and child orchestrators therefore share one number, with no per-role or per-task override.
- Counting calls treats `fs.list`/`explore.tree` the same as a large `fs.read`. That is a design choice, not a defect.

**Recommendation (design change, not a defect fix)**
1. Keep the call count; this report does not justify counting bytes. But stop counting cheap structure calls: `fs.list`, `fs.glob`, `explore.tree`, `explore.projects`, or give them a separate larger allowance. This is the "Baum" the prompt itself names as overview.
2. Add a per-role override in the agent definition, e.g. `[spawn] read_budget = N` next to `[spawn.budget]`, resolved into a per-session `GuardPolicy` at child admission (child_controller.rs:8430).
3. As an operator workaround right now: `[guards] orchestrator_read_limit = 12` in the home config.

Where new code should live (xtask/arch-policy.toml):
- The counting rule belongs in harw-core (ring A), guard.rs.
- The per-role value belongs in the agent definition under harw-registry-defaults/agents (A), plumbed through harw-core child_controller.
- Guard default constants for finding 1 belong in harw-config (ring I). harw-config must not depend on harw-core.
- A harw-runtime (A) test should assert that those constants equal `GuardPolicy::default()`.

Tests I added: none (read-only). The central build must run, in this order:
1. `cargo fmt --all`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace` (including doc tests), in particular `-p harw-core guard` and `-p harw-config --test config_scope_merge`
4. `cargo run -q -p xtask -- gates`
5. `cargo deny check`
6. `make -C dod clippy test`
7. `actionlint`

