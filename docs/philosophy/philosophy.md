# Agent harnesses as an operating-system layer for long-lived, model-heterogeneous AI agents

Architectural synthesis and positioning of Harwness

Starting point

The development of capable AI agents is often treated almost exclusively
as a model problem: which model plans better, writes better code, uses
tools more reliably, or can process more context?

This view is incomplete.

A model produces decisions, proposals and structured intents to act. It
has no processes, files, network connections, jobs, child agents,
permissions or durable state of its own. All of these exist only through
the software surrounding the model.

The central thesis is therefore:

«A model has agentic potential. Only the harness turns that potential
into controlled, long-lived and verifiable work.»

Practical agent performance is better understood as a multiplicative
system:

Effective agent performance
=
Model capability
× Interface quality
× Context quality
× Runtime reliability
× Verification

A very capable model in a weak harness can perform worse than a somewhat
weaker model in a well-adapted runtime. When lifecycle management, tool
interfaces or context construction fail, the overall result approaches
zero regardless of the model.

This synthesis draws on three classes of sources:

1. a static analysis of the current Harwness workspace, spanning more
   than 70 Rust crates and several hundred thousand lines of Rust,
2. the design history drawn from studying "codex-rs", Hermes and
   OpenClaw,
3. current primary sources on agent architectures, context management,
   MCP, durable execution and differing model capabilities.

Statements about implemented architecture refer to the code as it
actually exists, not to a claimed full end-to-end verification.

---

1. Model, agent, SDK and harness are not the same thing

A language model is fundamentally a probabilistic inference system. It
takes context and produces output.

An agent adds a control loop around this model:

Take in context
→ make a decision
→ choose a tool or action
→ observe the environment's result
→ update state
→ continue or stop

Anthropic describes agents accordingly as systems in which the model
dynamically directs its own processes and tool usage. At the same time,
it stresses that agents need ground truth from their environment, clear
stopping conditions, sandboxing and carefully designed tool interfaces.

An agent SDK gives developers abstractions to build such control loops
into software. The OpenAI Agents SDK, for example, provides an agent
loop, function tools, agents-as-tools, handoffs, sessions, guardrails and
tracing.

A harness goes further. It must not only express agents, but run them
durably:

- start and stop processes,
- normalize models and providers,
- execute tool calls,
- bound permissions,
- persist sessions and jobs,
- supervise child agents,
- survive crashes,
- manage approvals,
- assemble context,
- verify results,
- serve clients and remote channels,
- clean up orphaned or superseded work.

A complete personal agent system therefore has at least four layers:

Client plane
TUI, CLI, web, Telegram, mobile clients

Control plane
Gateway, admission, scheduler, jobs, policies, agent registry

Execution plane
Models, tools, MCP, sandbox, processes, child agents

State plane
Sessions, transcripts, jobs, memory, artifacts, audit, approvals

Harwness is moving in exactly this direction. It is neither just an
agents SDK nor just a coding CLI. It is becoming a local agent control
plane with its own runtime.

---

2. Why model diversity is a runtime question

Current models are offered with markedly different capabilities and
operating characteristics.

Some are explicitly positioned for long-horizon tasks with very large,
stable context windows and variable reasoning intensity. Others are
described as agentic models with strong tool-calling ability, while their
own documentation makes clear that the surrounding client must supply
tool schemas on every call, parse tool calls correctly, execute the
functions, and feed the results back into the model's history. Still
others ship with agentic tool calling, configurable reasoning, and large
context windows of their own.

These specifications describe capabilities, but not yet reliable runtime
behavior. Two models with a nominally identical context window and the
same function-calling protocol can differ substantially in:

- how strictly they follow JSON schemas,
- when they choose a tool over free text,
- how they interpret tool errors,
- whether they structure parallel actions sensibly,
- how quickly they lose focus over long histories,
- how well they keep working after a compaction,
- whether they use delegation sparingly or excessively,
- what prompt structure they need for stable work,
- how reliably they recognize a task as finished.

A provider abstraction as thin as this is therefore not enough:

async fn respond(messages, tools) -> ModelResponse

A capable harness needs two separate model properties.

Declared provider capabilities

These describe the technical API:

struct ModelCapabilities {
    context_limit: usize,
    supports_tools: bool,
    supports_parallel_tools: bool,
    supports_streaming: bool,
    supports_reasoning_effort: bool,
    supports_prompt_caching: bool,
    supports_images: bool,
}

Measured runtime profile

This emerges from evaluations and real runs:

struct ModelBehaviorProfile {
    tool_call_reliability: Score,
    long_context_retention: Score,
    delegation_discipline: Score,
    schema_strictness: Score,
    compaction_tolerance: Score,
    retry_sensitivity: Score,
    preferred_task_granularity: TaskGranularity,
    safe_parallelism: usize,
}

The first profile says what the API allows in principle. The second says
how the concrete model actually behaves under a given harness.

A model router should therefore not decide purely by provider name or
benchmark rank, but by role:

Orchestrator
strong at decomposition, conflict resolution and synthesis

Repository worker
strong with large context and longer accumulation

Focused coding worker
strong at narrowly scoped implementation

Verifier
conservative, schema-strict and verification-oriented

Scout
cheap, fast, read-only and highly parallelizable

Model heterogeneity is therefore not noise that should disappear entirely
behind a uniform interface. It is a resource the harness must exploit
deliberately.

---

3. Why large context windows do not replace a memory architecture

A very large context window does not mean as much data as possible
should be poured into it.

Anthropic describes context as a finite resource with diminishing
returns. As context grows, focus, retrieval precision and the ability to
process widely separated dependencies can degrade. Effective context
engineering therefore does not seek the largest possible token set for
the current decision, but the smallest, highest-signal one.

This means:

«Context must be constructed, not simply accumulated.»

A usable agent runtime should distinguish at least between the following
kinds of information:

Working context
data for the immediately current turn

Session history
communication and decision history

Job state
durable state of the current work

Workbench
structured, human-visible work artifacts

Diary
time-oriented record of experience and activity

Memory
curated, longer-term relevant knowledge

Skills
reusable procedural capabilities

Artifacts
files, reports, patches and other concrete results

Harwness does not adopt such product ideas as a single unstructured
prompt folder. Its current crate structure separates, among others, in
`harw-knowledge`:

- memory core,
- recall and topics,
- memory palace,
- diary,
- dream,
- workbench,
- kanban,
- artifacts,
- visibility.

This separation matters. A memory entry is not automatically a skill. A
diary entry is not automatically durable knowledge. A workbench artifact
is not automatically part of the model's context.

Each class of information needs its own promotion, retention and
visibility rules.

---

4. Memory consolidation is a long-lived workflow

The two-phase memory design shows particularly clearly how Harwness
thinks.

Phase 1

Many rollouts can be evaluated in parallel. They produce normalized,
rollout-local memory datasets.

Phase 2

The global memory artifacts are updated serially. Only one consolidation
process may inspect and modify the shared memory workspace at a time.

This is not merely an "LLM summarizes memories" function, but a
long-lived consistency protocol:

Select stage-1 data
→ deterministically sync the workspace
→ compute a git diff against the last success
→ stop without an agent if the diff is empty
→ start an isolated consolidation agent
→ heartbeat its lease throughout
→ check the result
→ update the git baseline
→ persist DB success and watermark

The separation between the following two is particularly strong:

Watermark
= bookkeeping over known input data

Workspace dirtiness
= the actual decision of whether consolidation work is needed

A watermark must not serve as the sole dirty check. The database state
can look current while the file artifacts are incomplete or
inconsistent. The real git diff, in contrast, captures changes, deletions
and agent-produced output together.

Git thereby becomes not merely version control, but:

- baseline,
- change detector,
- audit layer,
- bounded prompt context,
- traceable commit point.

The workflow resembles durable-execution systems: Temporal describes
durable execution as the ability to resume applications at their previous
point after process, network or infrastructure failures.

For a truly robust phase 2, the persistent state should express the
decisive commit boundaries:

Claimed
→ WorkspaceSynced
→ AgentCompleted
→ BaselineCommitted
→ DatabaseCommitted

All steps must be idempotent or safely reconstructable. The agent must
not itself guarantee consistency; it operates within a protocol the
runtime guarantees.

---

5. A goal is desired state, not a long prompt

The goal mechanism plays a central role here.

A goal is not merely a phrase that appears at the start of a chat
session. It describes the durable, desired end state. The plan, by
contrast, is only the current strategy for reaching that state.

Goal
What must be true at the end?

Plan
What strategy are we currently pursuing?

Tasks and jobs
What concrete units of work are being executed?

Verification
What evidence proves the goal has been reached?

This matches the controller principle from distributed systems: a
controller observes the current state and continuously tries to bring it
closer to the desired state.

For coding agents this means:

- A context compaction must not destroy the goal.
- A model switch must not change the goal's semantics.
- A failed worker may change the plan, but must not silently reduce the
  goal.
- New findings may revise the plan.
- "Done" is determined by acceptance criteria and evidence, not by the
  model's sense of completion.

A persisted goal could roughly contain:

struct Goal {
    id: GoalId,
    statement: String,
    invariants: Vec<Invariant>,
    acceptance_criteria: Vec<Criterion>,
    constraints: Vec<Constraint>,
    status: GoalStatus,
    plan_version: u64,
    evidence: Vec<EvidenceRef>,
}

A phrasing like "continue working … get inspired by … and regard:
[architecture review]" already functions as a control hierarchy:

Goal
keep working and finish the desired system

Sources of inspiration
patterns from other coding-agent and personal-agent projects

Architectural regard
invariants, failure windows and the boundaries of the review

Autonomy
the orchestrator may adjust the concrete plan on its own

This prevents the typical agent behavior of asking for permission to
implement again after every analysis.

---

6. The model may propose work, but must never own processes

Many multi-agent systems treat a spawn roughly like this:

Model says "start agent"
→ a new agent is started

That is too weak.

Model output is untrusted intent. Only after server-side admission may a
real unit of work arise from it:

Delegation intent
→ schema and policy check
→ deduplication
→ budget assignment
→ authority reduction
→ create child job
→ assign lease
→ start worker
→ monitor heartbeats
→ persist result
→ parent join
→ cleanup

The model therefore owns neither the child agent nor its process. It only
receives a handle to controlled work.

This separation is decisive, because natural-language prompting cannot
substitute for missing infrastructure. A model can be instructed to
delegate sparingly. It cannot, however, prompt into existence a missing
process reaper, missing fencing, or a persistent parent-child
relationship.

---

7. Multi-agent is a long-lived execution graph

A production-ready multi-agent system is not simply:

await gather(run_agent(a), run_agent(b), run_agent(c))

It is a durable, verifiable execution graph:

Parent job
├── Turn
├── Tool calls
├── Child job A
│   ├── Authority snapshot
│   ├── Budget
│   ├── Lease
│   ├── Heartbeat
│   └── Result
├── Child job B
└── Completion barrier

Every child needs at least:

- a unique identity,
- parent or promotion semantics,
- an idempotent spawn key,
- a fixed budget,
- reduced capabilities,
- a lease owner,
- a cancellation policy,
- join and result semantics,
- a terminal state,
- reaping or tombstone handling.

The current Harwness codebase already has a remarkable number of concrete
mechanisms for this:

- `LeaseToken { work_id, epoch, nonce }`,
- long-lived jobs,
- fencing,
- cancellation,
- retry and budget information,
- a managed child controller,
- child leasing,
- reaping,
- tombstones,
- capability snapshots,
- parallel child runs,
- actor-bound single-use approvals.

The particularly important ordering is:

Persist durable cancellation and fencing
→ leave the store lock
→ signal the old worker

Not the other way around.

If a process signal is sent first and persistence subsequently fails, the
state cannot be reconstructed unambiguously. If, instead, the epoch or
fencing identity is updated first, an old worker can neither renew nor
successfully complete after a reclaim or cancellation.

Leases and heartbeats are established coordination mechanisms. Kubernetes
uses lease objects and updated `renewTime` timestamps to determine
participant availability.

For agents this implies:

«No child without a lease, no lease without fencing, no terminal state
without persisted completion.»

This is how the runtime prevents multi-agent orchestration from turning
into a pile of duplicated, orphaned or semantically stale "zombie
agents".

---

8. Recovery must happen through reconciliation

A plain process restart is not a recovery strategy.

After a gateway restart, the runtime must reconcile:

What does the persistent store claim?
Which workers count as claimed?
Which leases have expired?
Which processes or tasks actually exist?
Which children have a living parent?
Which jobs were durably cancelled?
Which results were already committed?

This produces a reconciliation loop:

Observe
→ detect a deviation
→ determine an allowed correction
→ execute the action
→ persist the new state
→ observe again

This controller approach suits an agent runtime better than a purely
imperative "start, wait, done" model.

It allows:

- reclaiming expired jobs,
- fencing stale workers,
- restarting missing children,
- not double-committing already-persisted results,
- cancelling or promoting orphaned jobs by policy,
- making the gateway process itself replaceable.

---

9. The provisioning layer is the real agent compiler

Tools, skills, MCP, profiles and agent definitions should not simply be
pushed into one large prompt or a global tool list.

An agent instance should be deterministically compiled from several
sources:

User profile
+ agent definition
+ goal and job context
+ active skills
+ available operations
+ MCP catalogs
+ channel restrictions
+ sandbox policy
+ approval policy
+ provider capabilities
────────────────────────
Resolved agent capability view

This capability view should be bound as a snapshot to a turn or job. That
way, the global environment can change later without silently changing
the meaning of work already in progress.

The key terms must stay separate:

Profiles describe user, environment and provider defaults.

Agent definitions describe role, instructions, models, skills, tools,
budgets and authority.

Skills contain procedural knowledge and, where applicable, resources.

Operations are semantic actions that can be exposed across different
surfaces.

MCP is a protocol through which tools, resources and prompts are
provided.

Session is communication and working context.

Job is the long-lived executable unit of work.

Workbench is the human-visible projection of work and results.

Kanban is an organizational view over the workbench and jobs.

The MCP protocol itself is not a complete security boundary. Its own
specification explicitly notes that MCP can enable arbitrary data access
and code execution, and that authorization, consent and access control
must be provided by the implementing application.

Harwness therefore does not treat MCP data as authority. The existing
code binds sessions to server-side-resolved principals, filters
`tools/list` by capability, and is meant to draw scope, budget, sandbox,
tenant and credentials exclusively from trusted composition, never from
the MCP server itself.

This is the right direction:

«An MCP server can describe possibilities. It must not grant itself
rights.»

---

10. Operations are semantic actions, not just tools or commands

The `harw-operations` layer is one of the most important parts of the
current architecture.

An operation is defined once, semantically, and can then be made
available on different surfaces in a targeted way:

Command surface
direct user action, e.g. `/status` or `/model`

Model tool surface
a structured function the model can call

Agent tool surface
an encapsulated child agent or specialized workflow

Channel surface
remotely available, possibly further reduced action

The existing types already distinguish:

- `CommandVisibility`,
- `ApprovalPolicy`,
- `PermissionTier`,
- `Surface`,
- `OpInvocation::Command`,
- `OpInvocation::ModelTool`,
- `OpInvocation::AgentTool`.

This matters, because `/quit`, `/model` or `/attach` should not
automatically be available as a model tool. A shared source does not mean
every surface has the same rights.

An operation can, for example, declare:

#[operation(
    name = "status",
    command = "/status",
    model_tool,
    permission = "observer",
    approval = "none"
)]
async fn status(
    ctx: &OpContext,
    args: StatusArgs,
) -> Result<StatusOutput, StatusError> {
    // ...
}

The proc macro generates mechanical infrastructure from this:

- metadata,
- argument schema,
- adapters,
- a registry entry,
- dispatch,
- a tool specification,
- compile-time validation.

The macro is therefore not merely a boilerplate generator. It becomes a
small compiler for system invariants.

It can, for example, enforce:

- that mutating model tools have an approval policy,
- that command paths are unique,
- that arguments are parsed per surface,
- that an agent operation defines a budget and an authority reducer,
- that not every operation is visible on every channel.

This high implementation density matches the project's general Rust
style: strong types and typestate at the foundation, with a compact,
modular DSL on top.

---

11. Skills and tools are an agent-computer interface

Tool design is often treated as simple API integration. For models,
however, it is its own user interface.

Anthropic reports that, for a coding agent, more time was invested in
optimizing the tools than in the overall prompt. Parameters, path
semantics, descriptions and error shapes directly affect whether a model
can use a tool reliably.

A good tool interface must therefore:

- be clearly scoped,
- have unambiguous parameters,
- return structured errors,
- not demand unnecessary formatting work,
- accept no authority in its input,
- allow idempotent retry,
- bound relevant results,
- require verification or approval on mutation.

A harness should also be able to offer tools model-dependently. A model
that frequently confuses many similar tools can get a reduced toolset.
Another can meaningfully use progressive tool discovery or hierarchical
catalogs.

Skills complement this interface with procedural knowledge:

Tool
What can technically be executed?

Skill
How, when and under what conditions should a capability be used?

Operation
What semantic action is offered?

Policy
Who may execute this action, over which surface?

This separation is considerably cleaner than one giant system prompt in
which capabilities, rules, tutorials and permissions are all mixed
together.

---

12. Authority must be reduced monotonically

One of the most important Harwness invariants is, in essence:

«Untrusted input must never expand rights.»

This implies a monotone authority pipeline:

System maximum
∩ Principal capabilities
∩ Agent definition
∩ Job scope
∩ Channel restrictions
∩ Child-agent reduction
∩ Sandbox policy
=
Effective authority

Every further step may only keep or reduce rights, never increase them.

This applies especially to:

- Telegram or other remote channels,
- MCP clients,
- skills,
- model tool arguments,
- child agents,
- resumed jobs.

A model-generated tool call must therefore never itself determine:

- tenant,
- workspace,
- work ID,
- worker ID,
- budget,
- retry policy,
- sandbox,
- credentials,
- permission tier.

These values must be resolved server-side from the principal, session,
agent definition, job and policy.

The current Harwness design states exactly such boundaries: remote and
MCP data carry no authority, job scope is immutable, lease tokens are
never serialized over MCP, and principal resolution happens server-side.

This is one of the points where Harwness thinks beyond many of today's
agent frameworks. It models not only what an agent can do, but also where
the permission to do it comes from.

---

13. Observability is part of the protocol

Tracing must not be wrapped around the agent loop as an afterthought. It
must reflect the system's semantic state transitions.

A sensible span hierarchy is:

agent.session
└── agent.turn
    ├── context.assemble
    ├── model.request
    ├── model.response
    ├── tool.request
    │   └── tool.execute
    ├── approval.wait
    ├── child.spawn
    │   └── child.turn
    ├── artifact.persist
    └── verification

The current Harwness codebase already emits, among others:

- turn spans,
- tool-request events,
- tool-completion events,
- usage aggregation,
- turn-failure events,
- approval and handoff states.

The TUI now consumes operation adapters as well as session and turn
events. In the TUI path, however, it still creates a local `AgentSession`
and an `InMemoryStateStore`. The gateway, too, still has local in-memory
sessions. The intended architecture of a fully thin client is clearly
visible, but not yet fully carried through everywhere.

In the long run, only the gateway should own the runtime:

Gateway
owns sessions, jobs, providers, tools, memory and events

TUI
sends intents and renders event streams

Remote channel
sends reduced intents and receives reduced results

Then the TUI can be closed, restarted, or replaced by another client
without agent work dying with it.

---

14. The main failure classes of today's harnesses

Interface mismatch

The provider supports tools, but the adapter loses tool calls,
tool-result IDs, reasoning parameters or usage data.

Context pollution

The harness attaches ever more history, tools, skills and files to every
turn, unfiltered.

Lifecycle leakage

Children have no unambiguous parents, leases, deadlines or terminal
states.

Duplicate work

Retries and restarts create new work even though the original job still
exists or has already completed.

Authority leakage

Tool or channel input is allowed to influence scope, workspace,
credentials or sandbox.

Zombie accumulation

Stale workers stay active because fencing, reaping or reconciliation are
missing.

Non-durable progress

A long agent run exists only in RAM and disappears when the process
dies.

Provider over-normalization

Different models are reduced to the smallest common interface, so their
distinctive capabilities go unused.

Prompt-based governance

Safety and lifecycle rules are explained to the model instead of being
enforced by the runtime.

Completion without evidence

The model declares the task done without tests, a diff, an artifact, or
other objective proof.

These failures reinforce each other. A lost tool call can trigger a
retry; the retry creates a second child; both write to the same
workspace; the parent compacts; after a restart the runtime knows
neither worker anymore.

The result looks like a "dumb model", but is in reality a missing
operating system.

---

15. Reference flow for a long-lived coding goal

A complete flow should look roughly like this:

1. User states a goal
2. Runtime persists the desired state and acceptance criteria
3. Orchestrator produces a versioned plan
4. Plan is translated into a job graph
5. Admission checks budget, authority and parallelism
6. Model router picks suitable models per role
7. Workers receive bounded capability snapshots
8. Tools and child jobs run under leases and fencing
9. Events, traces and artifacts are written durably
10. Verifiers check tests, diff and target criteria
11. Controller reconciles deviations or errors
12. Successful results are committed
13. The memory pipeline extracts and consolidates relevant findings
14. The goal gets evidence and a terminal status

The orchestrator may change its strategy. The goal and its invariants
stay stable.

---

16. The central design principles

From research, the codebase and systems engineering, the following
invariants emerge:

1. The model states intent; the runtime owns authority and lifecycle.

2. Every long-lived effect is persisted before or atomically with its
   execution.

3. Fencing happens before signalling a superseded worker.

4. Every spawn has parent, promotion, budget, lease and cleanup
   semantics.

5. Capabilities may only be reduced along the whole pipeline, never
   expanded.

6. Provider adapters must be semantically lossless, not merely
   syntactically uniform.

7. Context is constructed per turn, not accumulated without bound.

8. Memory is a promotion pipeline, not an uncontrolled long-term
   transcript.

9. Clients own no runtime.

10. Recovery happens through reconciliation against durable state.

11. Observability reflects semantic states and is not optional logging.

12. A task is only done once verifiable evidence satisfies the
    acceptance criteria.

13. Multi-agent parallelism is a budget, not an end in itself.

14. Tools, skills, operations and policies remain separate concepts.

15. Proc macros may hide boilerplate, but must never create unverifiable
    authority.

---

17. Positioning Harwness

Harwness is best described neither as "yet another Codex" nor merely as
an agent framework.

It is an emerging:

«local, model-heterogeneous agent operating environment with a control
plane, a long-lived job runtime, capability provisioning, a knowledge
system and interchangeable clients.»

Its lines of inspiration are clear:

- "codex-rs" provided examples of how a serious coding-agent runtime,
  sandbox and terminal integration can be built in Rust.
- Hermes inspired parts of the provider, skill, memory and personal-agent
  layers.
- OpenClaw inspired the gateway, channel, diary, dreaming and
  durably-running agent functions.
- Harwness's own agents SDK, operation syntax, jobs layer, authority
  logic, MCP/skill provisioning and overall composition form a
  self-contained model, however.

The original contribution does not lie in having invented every single
feature. It lies in the synthesis and in the boundaries between the
components:

Session is not job.
Job is not agent.
Agent is not model.
Skill is not tool.
Tool is not permission.
Workbench is not memory.
Client is not runtime.
MCP is not authority.
Goal is not plan.

Exactly this separation is what later makes extensibility possible,
without the system collapsing into a pile of implicit side effects.

---

Conclusion

The next generation of personal AI agents will not arise from stronger
models alone.

Models will increasingly have larger contexts, better tool calling,
longer reasoning horizons and stronger coding capabilities. These
advances, however, simultaneously raise the demands on their operating
environment.

The longer and more autonomously an agent works, the more important the
following become:

- long-lived state,
- controlled delegation,
- recovery,
- context engineering,
- authority boundaries,
- model-dependent runtime profiles,
- verification,
- transparent clients,
- deterministic memory pipelines.

The decisive technical question is therefore not only:

«Which model is the smartest?»

But rather:

«Which runtime can reliably translate the strengths of different models
into durable, controlled and verifiable work?»

Harwness addresses exactly this second question.

The project's long-term value therefore does not lie merely in a better
TUI or another tool-calling abstraction. It lies in a complete agent
operating layer that treats models as interchangeable decision organs and
itself controls every real resource — processes, rights, work, knowledge
and lifecycles.

A model can plan, delegate and program.

But only a good harness ensures that the result is a resilient system,
not a pile of zombies.
