# Harwness Interaction Contract — Master Document

Status: draft design document, normative
Role: this is the single entry point for "how does a human (or another
system) talk to Harwness." It does not redefine anything the three annexes
already specify in detail; it states the unified model, resolves the seams
between them, and is the canonical place to look up the full command
grammar, the full knowledge-surface and plugin/skill sub-grammars, the
unified approval model, and the register of decisions still open across the
whole interaction surface.

Normative annexes (this document supersedes their summaries where the two
disagree; the annexes remain authoritative for everything below the level of
detail reproduced here):

- [`tui-command-contract.md`](./tui-command-contract.md) — command grammar,
  prefix system, keybindings, registry architecture.
- [`channel-ingress-telegram.md`](./channel-ingress-telegram.md) — channel
  abstraction, Telegram binding, admission/pairing, approval rendering.
- [`knowledge-surfaces.md`](./knowledge-surfaces.md) — memory/diary/dream/
  palace/workbench/kanban storage, governance, and per-surface semantics.
- [`secrets-and-audit.md`](./secrets-and-audit.md) — harw-secrets envelope
  design and the tamper-evident audit chain backing approval/command audit.
- [`crates-inventory.md`](./crates-inventory.md) — canonical external
  dependency procurement list for all planned crates.

---

## 1. Overview: one interaction model, two front-ends

Harwness has exactly one command grammar, one permission model, one approval
model, and one knowledge-and-work substrate. It has two *front-ends* onto
that single model:

- the **TUI** (`harw-tui`) — a local terminal, full-fidelity front-end: every
  command, every keybinding, every pager/panel surface.
- **channels** (`harw-channel` + per-platform bindings, Telegram first) —
  remote, capability-limited front-ends reached over a messaging surface,
  admitted through a per-channel perimeter gate, exposing a *subset* of the
  same command grammar under the same permission tiers.

Neither front-end has its own copy of business logic. Both dispatch into the
same `CommandRegistry` (§5 of the TUI annex) against the same session/work/
knowledge state; a channel binding can only ever *narrow* what is reachable
(`CommandScope::ChannelReduced`/`TuiOnly` gating, §1.4 of the TUI annex), it
can never grant something the TUI front-end wouldn't also allow. This is the
same "reduce, never widen" rule the channel annex states for capability
sandboxing (§5 of the channel annex) — the interaction layer and the
capability layer share the same directionality on purpose: a remote surface
is a narrower window onto the same trust boundary, never an alternate one.

```
human (terminal)         human (Telegram, Slack, ...)
      |                              |
   harw-tui                   harw-channel-<binding>
      |                              |
      |---- classify_input() --------|          (§3, TUI annex)
      |                              |
      +---------- CommandRegistry ---+          (§5, TUI annex; one instance)
                     |
        session store / work graph / harw-knowledge / harw-catalog
                     |
              ApprovalRequest/Response (§4, this document)
```

Three things are unified here, not left as three separate documents' problem:

1. **The command grammar** (§2) — the TUI annex's full inventory, with the
   knowledge-surface commands (`/memory`, `/diary`, `/dream`, `/palace`,
   `/workbench`, `/kanban`) and the plugin/skill management commands
   (`/plugins`, `/skills`) worked out to the same subcommand-and-typed-argument
   depth as every other domain, instead of being one-line placeholders.
2. **The approval model** (§4) — one `ApprovalRequest`/`ApprovalResponse`
   pair, one audit record shape, whether the decision arrives via a TUI
   keypress or a Telegram inline-keyboard tap.
3. **The open-decision register** (§6) — every unresolved question raised by
   any of the three annexes, in one numbered list with a status, so nothing
   sits unresolved in three different places without anyone noticing they're
   related.

---

## 2. Full command grammar

Everything in §1 (grammar, `CommandSpec` shape, typed-argument system,
scopes, tiers, output surfaces) and §5 (registry architecture) of the TUI
annex applies unchanged and is not repeated here. This section extends §2
of the TUI annex (the command inventory) in the two places it was left
incomplete: §2.6 (knowledge surfaces, one-liners only) and the two catalog
commands that need full sub-grammars (`/plugins`, `/skills`).

### 2.1 Typed-argument additions

The TUI annex's typed-reference table (§1.3) already carries `SkillRef`. This
document adds the remaining references needed to give the knowledge and
plugin/skill domains the same typed-argument treatment as work items and
sessions:

| Type | Surface form | Example | Resolved by |
|---|---|---|---|
| `BundleRef` | bare name or `bundle:<name>@<version>` | `my-tools`, `bundle:acme-toolkit@2` | catalog bundle registry |
| `CardRef` | `card-<n>` or bare `CardId` | `card-77` | kanban board index |
| `LaneRef` | bare lane name, `board:<board-id>/<lane>` when disambiguation is needed | `worker/coding`, `board:acme/triage` | kanban board index, defaulting to the active board |
| `BoardRef` | bare board name | `acme` | kanban board registry |
| `AgentRoleRef` | `role:<name>` or bare name where unambiguous | `role:coding-worker` | agent-role catalog |
| `ArtifactRef` | bare `ArtifactId` (kind-prefixed slug) | `topic/deploy-pipeline`, `palace/on-call` | `KnowledgeIndex::get` |

Corresponding `ArgType`/`ResolvedArg` variants (`Bundle`, `Card`, `Lane`,
`Board`, `AgentRole`, `Artifact`) and `ReferenceResolver` methods
(`resolve_bundle`, `resolve_card`, `resolve_lane`, `resolve_board`,
`resolve_agent_role`, `resolve_artifact`) are added alongside the existing
ones in §5.1/§5.3 of the TUI annex, following the same "structured
`UnresolvedRef` over generic parse failure" rule.

### 2.2 Knowledge surfaces — full subcommand grammar

Supersedes TUI annex §2.6 (which reserved the names only). Tiers use the
TUI annex's `PermissionTier`; parity uses `CommandScope`.

**`/memory`** — durable cross-session agent memory (core + topic layers).

| Subcommand | Args | Tier | Parity | Notes |
|---|---|---|---|---|
| `/memory recall` | `<query> [--tags=t1,t2] [--kind=core\|topic\|palace] [--max=n]` | Op | Y | Bounded `RecallQuery` (§2.3, knowledge annex); prints hits with provenance |
| `/memory write` | `<topic> <text>` | Op | Y | Appends/creates a topic memory file in the caller's authored scope; never touches core memory directly |
| `/memory show` | `<ArtifactRef>` | Obs | Y | Renders one artifact's frontmatter + body |

**`/diary`** — per-agent narrative log.

| Subcommand | Args | Tier | Parity | Notes |
|---|---|---|---|---|
| `/diary show` | `[AgentRef] [--date=YYYY-MM-DD]` | Obs | Y | Renders one day's entries; bare form is caller's own diary, today |
| `/diary note` | `<text>` | Op | R | Out-of-band append outside the automatic end-of-session/compaction triggers; channel form is plain text only, rate-limited the same as any other write path |

The `#` prefix shortcut (§3, TUI annex) is sugar for `/diary note` on both
front-ends — this document confirms that mapping is exact, not merely
similar, so `#` and `/diary note` share one code path and one audit trail.

**`/dream`** — idle-time consolidation, a governed `harw-job-runtime` job.

| Subcommand | Args | Tier | Parity | Notes |
|---|---|---|---|---|
| `/dream run` | `[--now] [--budget=Duration]` | Maint | - | Enqueues a dream job; idle-time default, `--now` forces immediate scheduling |
| `/dream status` | `[--mine]` | Obs | Y | Lists running/recent dream jobs as governed work items — same shape as `/kanban` |
| `/dream review` | `<WorkId> [--approve=ArtifactRef,...] [--reject=ArtifactRef,...]` | Maint | - | Opens a `DreamReport`; per-proposal approve/reject is the only path a proposal becomes a committed artifact |

**`/palace`** — the linked long-term memory graph.

| Subcommand | Args | Tier | Parity | Notes |
|---|---|---|---|---|
| `/palace show` | `<ArtifactRef>` | Obs | Y | Node body + outgoing links + computed backlinks |
| `/palace search` | `<query> [--max-hops=n] [--max=n]` | Obs | Y | Graph-aware recall variant; bounded per §2.3 of the knowledge annex |
| `/palace promote` | `<ArtifactRef>` | Maint | - | Starts topic→palace promotion (knowledge annex §2.5); lands as an immediate write only if the caller's scope authorizes direct promotion, else a pending review item |

**`/workbench`** — persistent scratch/working-set surface. Parity is decided
per subcommand rather than for the surface as a whole (this resolves TUI
annex open question §7.3 for `/workbench` specifically — see §6, item 3):

| Subcommand | Args | Tier | Parity | Notes |
|---|---|---|---|---|
| `/workbench pin` | `<path> [note]` | Op | - | Requires local filesystem path resolution against the TUI's own working directory; meaningless from a remote channel |
| `/workbench note` | `<text>` | Op | R | Pure text append, no path resolution — safe to reduce-expose on channels |
| `/workbench hypothesis` | `add\|confirm\|reject <text>` | Op | R | Structured list mutation, no path resolution |
| `/workbench` (bare) | `[--scope=session\|project:<slug>]` | Obs | - | Opens the always-visible TUI panel (knowledge annex §5.2); has no channel analogue by construction |

**`/kanban`** — visual board projection of the work graph.

| Subcommand | Args | Tier | Parity | Notes |
|---|---|---|---|---|
| `/kanban create` | `<title> --lane=<LaneRef> [--assignee=<AgentRoleRef>] [--parent=<CardRef>]` | Op | Y | Creates a card; already a `WorkId` if the lane is a worker lane and the card is created `Ready` |
| `/kanban show` | `<CardRef>` | Obs | Y | Full card state plus its `WorkId`'s job history |
| `/kanban claim` | `<CardRef>` | Op | Y | Thin wrapper over `harw-job-runtime::claim`; subject to the `RiskLevel::High` approval gate (knowledge annex §6.4) |
| `/kanban complete` | `<CardRef>` | Op | Y | Moves to `Done`, or to `Blocked{ReviewRequired}` if the card is tagged `review-required` |
| `/kanban block` | `<CardRef> --reason=<Dependency\|NeedsInput\|Capability\|Transient\|ReviewRequired>` | Op | Y | |
| `/kanban unblock` | `<CardRef>` | Op | Y | |
| `/kanban archive` | `<CardRef>` | Maint | R | Rejected outright if unresolved child cards exist (structural invariant, not just a permission gate); reduced on channels because the dependent-card check is easiest to review with the full board visible |

### 2.3 `/plugins` — extension bundle management

`/plugins` in the TUI annex (§2.5) was a one-line placeholder
(`[--list | --enable=name | --disable=name]`). This document replaces it with
the full subcommand grammar below; the flag-based form in the annex is
superseded.

| Subcommand | Args | Tier | Parity | Notes |
|---|---|---|---|---|
| `/plugins list` | `[--enabled-only]` | Obs | Y | Lists bundles from the current `CatalogSnapshot` (§8.1 of the knowledge annex's sibling `harw-catalog` crate) with enabled/disabled state |
| `/plugins inspect` | `<BundleRef>` | Obs | Y | Manifest contents: declared tools, skills, MCP servers, requested capability grants |
| `/plugins install` | `<path\|url> [--yes]` | Owner | TuiOnly | See §2.3.1 — manifest validation, policy check, mandatory operator confirmation; `--yes` still requires the confirmation prompt to have been shown in the same invocation, it is not a silent-skip flag |
| `/plugins enable` | `<BundleRef>` | Owner | TuiOnly | Affects only runs started after this point — see §2.3.2 |
| `/plugins disable` | `<BundleRef>` | Owner | TuiOnly | Same immutable-snapshot rule as `enable` |
| `/plugins remove` | `<BundleRef>` | Owner | TuiOnly | Deletes the bundle from the catalog source; a bundle in use by a still-running `CatalogSnapshot` continues to work until that run ends |
| `/plugins verify` | `<BundleRef> [--digest] [--provenance]` | Obs | Y | Recomputes and checks the bundle's content digest and provenance attestation; the same check `/doctor --section=catalog` runs across every installed bundle |

#### 2.3.1 Install flow: never a silent privilege widening

`/plugins install` always runs, in order:

1. **Manifest validation** — schema-checked, requested capability grants
   enumerated explicitly (tools, MCP servers, filesystem/network scope
   requests); a manifest that requests something the schema doesn't know how
   to express is rejected, not best-effort accepted.
2. **Policy check** — `harw-policy` evaluates the requested grants against
   the operator's own policy ceiling; a bundle cannot request more than the
   installing operator's own tier could grant directly.
3. **Operator confirmation** — the full, human-readable capability diff is
   printed (*"this bundle will be able to: read/write `sandbox.network`,
   invoke tool `fs::write_file`, register MCP server `acme-internal`"*) and
   requires an explicit `y`/`yes` response in the same TUI session. There is
   no channel path for this step at all — `/plugins install` is `TuiOnly`
   precisely so this confirmation cannot be rushed through a chat reply.

Steps 1–3 are the same three steps `/doctor --section=catalog` re-runs on
every already-installed bundle (verification is a subset of installation's
own checks, run again, not a separate mechanism).

#### 2.3.2 Enable/disable and `CatalogSnapshot` immutability

`harw-catalog` builds one `CatalogSnapshot` at the start of each run (session,
job, or dream job) and that snapshot does not change for the life of the run
— this is inherited, not introduced here, but it is the reason
`/plugins enable`/`disable` are documented as affecting *later* runs only.
Disabling a bundle mid-run does not retroactively strip a capability from a
session already holding it; the session's snapshot is what it is until the
session ends. A `/doctor` check flags any long-lived session still running
against a stale snapshot after a disable, so operators are not surprised by
this, but the mechanism is deliberate: yanking a capability out from under a
live tool call mid-execution is a worse failure mode than a bounded delay
before the change takes effect.

### 2.4 `/skills` — skill management

`/skills` in the TUI annex (§2.5) was also a placeholder
(`[SkillRef] [--search=text] [--install]`); this supersedes it.

| Subcommand | Args | Tier | Parity | Notes |
|---|---|---|---|---|
| `/skills list` | `[--source=bundle\|workspace\|user] [--search=text]` | Obs | Y | Source is one of the three provenance tiers a skill can come from |
| `/skills show` | `<SkillRef>` | Obs | Y | Renders the skill's declared inputs, budget defaults, and source |
| `/skills run` | `<SkillRef> [args...]` | Op | R | Executes as a governed work item (`WorkId`, `Budget`, same job-runtime primitive as a dream job or a kanban worker card) — see §2.4.1 |
| `/skills reload` | — | Maint | TuiOnly | Rebuilds the skill catalog into a new `CatalogSnapshot`; running runs are untouched (same rule as §2.3.2) |
| `/skills new` | `<name>` | Maint | TuiOnly | Scaffolds a new user-tier skill under the workspace's skill directory |
| `/skills enable` | `<SkillRef>` | Owner | TuiOnly | |
| `/skills disable` | `<SkillRef>` | Owner | TuiOnly | |

#### 2.4.1 `run`: reduced parity means approval-gated, not unavailable

`/skills run` is reachable from a channel, but only with an `ApprovalRequest`
resolved first (§4) — this is what "reduced channel parity (run only with
approval)" means concretely: the command is admitted, dispatched, and turns
into a governed work item exactly as it would from the TUI, but the
dispatch step is preceded by the same approval flow a `RiskLevel::High`
kanban claim goes through (knowledge annex §6.4), rendered as a Telegram
inline keyboard or a TUI `/review` prompt depending on which front-end asked.
A TUI-invoked `/skills run` may skip the prompt only if the resolved policy
for that skill/tier combination doesn't require one; the *reduction* on
channels is that the approval step can never be skipped there, regardless of
policy, since a remote caller's identity confidence is inherently lower.

### 2.5 Updated inventory totals

Counting every subcommand in §2.2–§2.4 as one command (consistent with how
the TUI annex counts `/export`'s flags as one command, not several):

- TUI annex base inventory (§2.1–§2.5, §2.7–§2.8, excluding the six
  knowledge-surface placeholders and the two placeholder catalog rows that
  are now superseded): **44 commands**
  (56 total in the annex − 6 knowledge one-liners − 2 superseded
  `/plugins`/`/skills` placeholder rows, `/export` counted once).
- Knowledge surfaces (§2.2 above): **22 subcommands**
  (`/memory` 3, `/diary` 2, `/dream` 3, `/palace` 3, `/workbench` 4,
  `/kanban` 7).

- `/plugins` (§2.3): **7 subcommands**.
- `/skills` (§2.4): **7 subcommands**.

**Grand total: 44 + 22 + 7 + 7 = 80 commands/subcommands** in the unified
grammar, up from the TUI annex's headline count of 56 (which undercounted
the knowledge surfaces at one line each and left both catalog domains as
flag-based placeholders rather than full subcommand sets).

### 2.6 Ist-Stand (2026-09) — im Code vorhanden, Vertrag nachgezogen

Die folgenden Commands existieren bereits in `harw-ops/src/`, sind in diesem
Vertrag aber (noch) nicht erfasst. Je eine Zeile, Zweck aus dem
Modul-Doc-Kommentar:

- **`/analyze`** — Bottom-up-Analyse eines Workspace über Analyst-Kindagenten,
  fan-out von Crate-Ebene zu Crate-Ebene mit abschließender Synthese.
- **`/add-workdir`** — gibt zusätzliche Arbeitsverzeichnisse für die laufende
  Sitzung frei (registriert sie in der geteilten `ExtraRootsCell`).
- **`/bug-report`** — löst manuell einen lokalen Bug-Report aus und schreibt
  ihn über `harw_ops::bug_report::write_bug_report`.
- **`/diff`** — read-only Git-Diff-Operation, delegiert ausschließlich an
  `shell.exec` über einen gehärteten `GitDiffPlan`.
- **`/explore`** — stellt eine gebundene Frage an ein read-only Kind
  (`explorer`) und validiert das Ergebnis gegen den `ResearchFinding`-Vertrag.
- **`/context-proposal`** — Prüffläche für `ContextProposal`: auflisten,
  ansehen, annehmen, ablehnen (Annehmen markiert nur, ändert kein
  Kontextprogramm).
- **`/goal`** — Ziel-Operation über `harw-plan`; beschreibt den gewünschten
  Endzustand, `achieve`/`abandon` sind command-only (nicht modellseitig).
- **`/mode`** — Anzeige und Wechselabsicht des Interaktionsmodus (`chat`,
  `plan`, `explore`, `work`, `shell`); TUI-only, kein Modell-Tool, damit ein
  Modell nicht sein eigenes Werkzeug-Ceiling anheben kann.
- **`/research-deps`** und **`/research-web`** — gebundene Recherche durch
  read-only Kinder; gleiche Struktur, unterscheiden sich nur in Kindrolle und
  Vorgabe-Quellklassen.
- **`/plan`** — Plan-Operation über `harw-plan`/`harw-plan-bridge`, Command
  und Modell-Tool, jede Mutation läuft über `PlanStore::apply`.
- **`/usage`** — zeigt aufgezeichnete Token-Nutzung und Wächter-Ereignisse der
  aktuellen Sitzung aus dem `StateStore`-Snapshot.
- **`/stop`** — bricht einen laufenden Job kontrolliert ab; Command und
  Modell-Tool mit `approval = "always"`.
- **`/sandbox-lease`** (neu, Nutzerentscheidung 2026-09-21) — Command **und**
  Modell-Tool für die direkte Host-Freigabe von `shell.exec`. Das Modell-Tool
  (`harw-ops/src/sandbox_lease.rs`, `model_tool` ohne Zusatz-Approval, denn
  der Dialog *ist* die Freigabe; Argument `reason`) löst einen
  `HostPermitPrompt` aus (Vorauswahl `SessionLease`) und wartet bis zu 300 s:
  `SessionLease` → `mark_session_approved` (TTL-befristet, alle
  `shell.exec`-Aufrufe der Sitzung laufen danach auf dem Host); `SingleExecution`
  → `mark_single_use` (nur der nächste Aufruf); Ablehnung/Timeout liefern
  einen Fehlertext. Der Command `/sandbox-lease [status|revoke]`
  (`busy = "immediate"`) ist rein lokal: `status` zeigt die aktive Freigabe,
  `revoke` beendet eine Sitzungsfreigabe sofort über
  `revoke_session_approval` + `ledger.revoke_session`. `/status`
  (`harw-ops/src/status.rs`) zeigt zusätzlich die Zeile „Host-Lease: aktiv
  bis … / Einzelfreigabe / aus". Details zur Host-Ausführung selbst stehen in
  `mediated-process-execution.md`.
- **`/permissions`** — Übersicht über Workspace-Identität, Sandbox-Rechte,
  Freigabemodus und Allow-/Deny-Regeln.
- **`/effort`** — setzt die providerneutrale Reasoning-Stärke für nachfolgende
  Turns, live, für die laufende Sitzung; operator-only, TUI-only, kein
  Modell-Tool. Grammatik: `show | clear | minimal | low | medium | high |
  xhigh | max` (Alias `/reasoning`) — hat einen strukturellen Zwilling,
  `/uia-effort` (siehe §2.6.1).

### 2.6.1 Modell-/Provider-/Effort-Befehle (Ist-Stand 2026-09, Welle 1–2)

Vollständiges Befehlsinventar für die Modell-, Provider- und
Reasoning-Effort-Achse, wie in `harw-ops/src/model.rs`, `provider.rs` und
`effort.rs` implementiert. `/provider switch` und `/uia-provider switch`
**existieren nicht mehr** — ein Provider+Modell-Wechsel läuft ausschließlich
atomar über `/model switch <id>` bzw. `/uia-model switch <id>`, die den
konfigurierten Provider des Ziel-Modells auflösen und an
`provider::handle_switch_core`/`handle_uia_switch_core` delegieren, auch wenn
das Ziel-Modell zu einem anderen Provider gehört als der aktuell aktive.

| Befehl | Grammatik | Wirkung | Sichtbarkeit | Permission |
|---|---|---|---|---|
| `/model` (Alias `/m`) | `show \| list \| switch <id>` | `switch` wechselt Provider+Modell **atomar**, live, über den `SessionController` | `tui_only` | `operator` |
| `/uia-model` | `show \| list \| switch <id>` | wie `/model`, aber für die gepinnte UIA-Auswahl (`uia_provider`/`uia_model`); `switch` ist live | `tui_only` | `operator` |
| `/uia-worker-model` (neu) | `show \| list \| switch <id>` | `switch` validiert die Modell-ID gegen den *effektiven* UIA-Provider und persistiert nur `uia_worker_model` in der Profil-`config.toml` — **kein** Live-Wechsel, kein eigenes `uia_worker_provider`-Konzept (der Worker teilt sich den Provider mit der UIA); wirkt ab der nächsten Sitzung | `tui_only` | `operator` |
| `/effort` (Alias `/reasoning`) | `show \| clear \| minimal \| low \| medium \| high \| xhigh \| max` | Live-Sitzungseinstellung über den `SessionController`, wirkt sofort auf nachfolgende Turns | `tui_only` | `operator` |
| `/uia-effort` (neu) | dieselbe Grammatik wie `/effort` | persistiert `reasoning.uia` in der Profil-`config.toml`, **kein** Live-Override (bewusster Unterschied zu `/effort`), wirkt ab der nächsten Sitzung | `tui_only` | `operator` |
| `/provider` (Alias `/p`) | `show \| list \| test` | rein lesend; `switch` fällt in den Unbekannt-Unterbefehl-Zweig und verweist auf `/model` | `tui_only` | `operator` |
| `/uia-provider` | `show \| list \| test` | wie `/provider`, für die UIA-Pin-Auswahl; `switch` verweist auf `/uia-model` | `tui_only` | `operator` |
| `/provider-concurrency` | `<ProviderRef> <n \| unlimited>` | verstellt `max_concurrency` eines Providers **live** über den `DynamicConcurrencyLimiter`: Erhöhen gibt Permits sofort frei, Senken ist lazy (laufende Requests werden nie abgebrochen, nur die Wiederauffüllung gedrosselt, bis das Ziel erreicht ist); Empfehlung bei wiederholten HTTP-429-Antworten: senken, nicht erhöhen | `tui_only` **und** `model_tool` (die UIA kann sich selbst drosseln) | `operator` (Command); `approval = "always"` für den Tool-Aufruf (die Macro unterstützt nur eine statische Freigabestufe je `model_tool`, daher gilt sie auch fürs Senken) |

**Sichtbarkeit der Concurrency-Grenze:** `/provider show` (über die geteilte
`format_load_status`-Hilfsfunktion, `harw-ops/src/provider.rs`) und `/status`
(`harw-ops/src/status.rs`) zeigen beide dieselben vier Werte aus
`harw_provider_http::ProviderLoadStatus`: die Concurrency-Grenze (`unlimited`,
falls kein Limiter installiert), die Anzahl freier Permits, die aktuelle
Rate-Limit-Wartezeit und die Anzahl seit Start beobachteter HTTP-429-Antworten
(inkl. Hinweis, bei wiederholten 429ern zu senken statt zu erhöhen). Der
`DynamicConcurrencyLimiter` ist für den OpenAI-kompatiblen **und** den
Anthropic-Pfad derselbe Mechanismus — `AnthropicMessagesProvider::
configure_concurrency` installiert denselben Limiter-Typ wie der
OpenAI-kompatible Provider (`harw-provider-http/src/anthropic.rs`).

**Anmerkung zur Granularität:** `OperationMeta.busy` ist eine Eigenschaft der
gesamten Operation, nicht des Unterbefehls — `/model` und `/provider` sind
auf dieser Ebene als Ganzes `busy = "immediate"`. Die TUI verfeinert das
jedoch zur Laufzeit über `busy_availability_for`
(`harw-tui/src/command_exec.rs`): nur `show`/`list` (und das bare
`/provider`, das äquivalent zu `show` ist) lösen tatsächlich sofortigen
Dispatch während eines laufenden Turns aus; `/model switch <id>`, bare
`/model` (öffnet den Picker) und `/provider test` werden trotz
`OperationMeta.busy = "immediate"` bis Turn-Ende eingereiht. Das entspricht
der ursprünglich engeren Nutzerentscheidung 4 ("nur `show`") — nicht jeder
Unterbefehl dieser beiden Operationen ist sofort verfügbar, siehe §2.6.3.

### 2.6.2 Trennung UIA / Orchestrator (Provider-Ebene)

- Korrektur (war zuvor als "zwei unabhängige Provider-Clients" beschrieben):
  Es gibt **einen** Router über alle aktivierten Provider
  (`RoutingModelProvider`, gebaut einmal in
  `harw-provider-http/src/lib.rs::build_provider_with_load_registry`, mit
  genau einem HTTP-Client je aktiviertem Provider). Die UIA bekommt darüber
  **keinen** eigenen Client mehr, sondern nur eine eigene Standardroute:
  `build_uia_model`/`build_uia_worker_model`
  (`harw-runtime/src/model.rs`) umhüllen den gemeinsamen Router mit
  `UiaDefaultRouteProvider`, der `provider_id`/`model_id` eines Requests
  **nur dann** mit `uia_provider`/`uia_model` auffüllt, wenn der Request sie
  noch nicht selbst trägt — eine Live-Wahl über `/uia-model switch`
  (die den Request explizit mit ihrer eigenen `provider_id`/`model_id`
  versieht) hat also stets Vorrang vor der Standardroute. Der
  `build_uia_model`-Doc-Kommentar hält fest: "seit der Vereinheitlichung mit
  dem Vorgabe-Router kann [der Aufbau] nicht mehr fehlschlagen — es wird kein
  zweiter HTTP-Client gebaut." Weicht `uia_provider` vom `default_provider`
  ab, scheitern UIA-Anfragen dadurch nicht mehr zur Laufzeit. Die
  Concurrency-Grenze (`DynamicConcurrencyLimiter`, §2.6.1) ist **eine**
  Grenze je Provider, nicht je Route — eine UIA-Anfrage über die
  Standardroute und eine Orchestrator-Anfrage an denselben Provider teilen
  sich dasselbe Kontingent.
- `uia_worker_model` pinnt ausschließlich die `model_id` über einen
  `PinnedModelProvider`, **nie** die `provider_id` — der Provider bleibt
  zwingend derselbe wie der effektive `uia_provider`.
- Die gesamte `uia-worker`-Rollenfamilie läuft **immer als genau eine
  Instanz, nie parallel**: `harw-core/src/child_controller.rs` und
  `harw-core-bridge/src/agent_tool.rs` deckeln `slots`/`max_parallel` für
  diese Rollen intern auf `1`, unabhängig vom Aufrufer-Parameter (ein
  `analyze(max_parallel: 4)` darf das nicht umgehen). Die UIA-Root-Session
  selbst kann heute ohnehin nicht als Kind-Session ein zweites Mal
  gleichzeitig entstehen (kein passender Spawn-Codepfad).

### 2.6.3 Busy-Verfügbarkeit während eines laufenden Turns

Grammatik-Metadatum `OperationMeta.busy` (`BusyAvailability::Immediate` vs.
`DeferredUntilTurnEnd`, Default) ist auf allen 13 vorgesehenen Operationen
gesetzt und `harw-tui`'s `CommandSpec` übernimmt es bereits:

| Sofort während eines Turns | Bis Turn-Ende eingereiht (Auswahl) |
|---|---|
| `/status` `/ps` `/usage` `/help` `/diff` `/work` `/review` `/model show`/`list` `/provider show`/`list` (inkl. bare `/provider`) `/approve` `/deny` `/cancel` `/stop` `/sandbox-lease` (`status`/`revoke`) | `/mode` `/effort` `/uia-effort` `/uia-model` `/uia-worker-model` `/uia-provider` `/provider-concurrency` `/permissions` `/plugins` `/skills` `/new` `/compact` `/memory` `/export` `/quit` `/model switch`/bare `/model` `/provider test` |

`/cancel` und `/stop` wirken auf den `JobStore` (Hintergrund-Jobs), **nicht**
auf den laufenden Turn selbst — dafür bleibt Ctrl+C exklusiv zuständig (siehe
§2.6.4).

**Fertig:** Die Metadaten-Verdrahtung (`harw-ops`,
`harw-tui/src/command.rs`/`registry.rs`) sowie der eigentliche Sofort-Dispatch
in der TUI (`queue_busy_key` in `harw-tui/src/app.rs`, delegiert an
`busy_availability_for`/`dispatch_slash_command` in
`harw-tui/src/command_exec.rs`) sind vollständig verdrahtet. `queue_busy_key`
verzweigt bei jedem abgeschickten Slash-Befehl auf `busy_availability_for`:
`BusyAvailability::Immediate` läuft sofort über `dispatch_slash_command`,
alles andere landet weiterhin in `deferred_input`. `busy_availability_for`
verfeinert `/model` und `/provider` zusätzlich unterhalb der
`OperationMeta`-Ebene (§2.6.1, Anmerkung zur Granularität): nur `show`/`list`
sind tatsächlich sofort, `switch` und `test` bleiben eingereiht.

### 2.6.4 Ctrl+C — harter Interrupt

Nutzerauftrag: Ctrl+C soll ein echter harter Interrupt sein (Modellaufruf,
Shell-/Sandbox-Prozesse, Kind-Agenten), nicht nur ein kooperativ geprüftes
Signal.

- **Modellaufruf:** `harw-core/src/turn_loop.rs` racet den Modellaufruf
  gegen `CancelToken::cancelled()` (`tokio::select!`, `biased`); ein Treffer
  **und** ein `Err(ModelError::Cancelled)` aus dem Provider (racet dort
  ebenfalls gegen den `CancelToken` aus `ModelRequest`) münden beide in
  denselben `cancel_turn(...)`-Pfad. **Fertig.**
- **Shell-/Subprozesse:** `harw-tool-shell/src/exec.rs` racet beide
  `timeout_at`-Wartepunkte zusätzlich gegen `cancel.cancelled()`; ein Treffer
  löst SIGKILL über die bestehende `terminate()`-Funktion aus und liefert
  `Err(ToolsError::Cancelled)`. **Fertig.**
- **Kind-Agenten:** `ManagedAgentSpawner::register_parent_cancel_token` (in
  `harw-core/src/child_controller.rs`) ist verdrahtet — `harw-tui/src/app.rs`
  ruft es beim Start jedes Turns auf (kurz vor `drive_turn_animated`, mit
  demselben `CancelToken`, der auch `TurnControl::with_cancel` mitgegeben
  wird), sodass danach admittierte Kinder `cancel.child()` erben. Ein
  Registrierungsfehler (z. B. weil die Session selbst ein admittiertes Kind
  ist) ist nicht fatal für den Turn, sondern wird nur geloggt
  (`tui.turn.register_parent_cancel_token_failed`). **Fertig.**
- **Doppel-Tap Idle/Busy:** `ChatApp::pending_quit`/`hard_quit_requested`
  (`harw-tui/src/app.rs`) vereinheitlichen den `QuitArm`/
  `QUIT_HINT_WINDOW`-Mechanismus (2 Sekunden Fenster) über Idle- und
  Busy-Pfad: `handle_busy_event` cancelt beim ersten Ctrl+C-Druck den
  laufenden Turn kooperativ (`active_cancel.cancel(...)`) und armt
  `pending_quit`; ein zweiter Druck derselben Taste binnen des Fensters setzt
  `hard_quit_requested`, das `run_loop` direkt nach dem laufenden
  `run_turn_streaming(...)`-Aufruf prüft und dann sofort beendet — derselbe
  Ausgang wie `HarwEvent::Quit` im Idle-Pfad. **Fertig.**

### 2.6.5 Reasoning-Effort-Standards (Rangfolge)

Nutzerentscheidung: Standard-Reasoning-Effort ist zusätzlich pro Provider,
pro Modell und pro Agenten-Definition konfigurierbar; bei Konflikt gilt die
Rangfolge **Provider > Modell > Agenten-Definition > Rolle** (`reasoning.*`
bleibt der Boden-Fallback).

- **Fertig:** die Config-Felder — `ProviderToml.default_reasoning_effort`
  (`harw-config/src/provider_toml.rs`) und
  `ModelToml.default_reasoning_effort` (`harw-config/src/model_toml.rs`)
  existieren mit rundtrip-getesteter TOML-Serialisierung.
- **Fertig:** die vierstufige Auflösungsfunktion, die die Rangfolge zur
  Spawn-Zeit anwendet, existiert in zwei bewusst identischen Ausprägungen
  (Schichtungsregel: `harw-core` darf nicht von `harw-runtime` abhängen):
  `harw_runtime::guard_wiring::resolve_default_reasoning_effort`
  (`harw-runtime/src/guard_wiring.rs`) für die UIA-Root-Session (aufgerufen
  aus `harw-runtime/src/assembly.rs`, nur wenn
  `organizational_role == AgentRoleId::UserInterface` und `spec.reasoning_effort`
  keine explizite Live-Einstellung trägt) und eine gespiegelte Fassung in
  `harw-core/src/child_controller.rs`
  (`resolve_child_default_reasoning_effort`) für Kind-Agenten, die über
  `ChildRegistryFactory::reasoning_effort_defaults_for_role(_task)` bedient
  werden — `harw-runtime/src/children.rs`s `RuntimeChildRegistryFactory`
  überschreibt diese Methode mit den tatsächlich für die aufgelöste
  Provider-/Modell-ID hinterlegten `default_reasoning_effort`-Werten.
  `role_effort_weights_from_config` bleibt weiterhin nur die unterste Ebene
  (Rolle) und dient beiden Auflösungsfunktionen als Boden-Fallback.
  **Bestehende Einschränkung (kein Bug, dokumentiert):** Ein Kind ohne eigene
  interne Modellstelle (`internal_point_for_role` liefert `None`, d. h. kein
  passender `internal_models`-Eintrag) läuft auf dem geerbten
  Eltern-Hauptmodell; dessen Provider-/Modell-ID ist der
  `RuntimeChildRegistryFactory` nicht bekannt, daher liefert
  `reasoning_effort_defaults_for_role(_task)` für ein solches Kind `(None,
  None)` und die Rangfolge fällt direkt auf die Rollen-Ebene durch — Provider-
  und Modell-Standard greifen nur für Kinder mit einer aufgelösten internen
  Modellstelle.

### 2.6.6 Reasoning-Sichtbarkeit (UIA)

Nutzerauftrag: Denkinhalt der UIA soll sichtbar werden, vor allem in der
UIA-Sitzung.

**Fertig, Ende-zu-Ende verdrahtet:** `harw-core/src/turn_loop.rs` liest
`response.reasoning: Option<OpaqueReasoning>` nach jedem Modellaufruf aus.
Für Anthropic-Blöcke extrahiert `extract_thinking_text` das lesbare
`"thinking"`-Textfeld; `"redacted_thinking"`-Blöcke und verschlüsseltes
OpenAI-Reasoning liefern keinen Text und erzeugen bewusst **kein**
`TurnItem::Reasoning` (kein Fehler). Der extrahierte Text wird als
`ReasoningItem` in `session.history_mut()` gepusht und als
`TurnEvent::ItemAdded { item: TurnItem::Reasoning(...) }` emittiert — **nur**
für Sessions mit `organizational_role == AgentRoleId::UserInterface` (dasselbe
Kriterium wie `is_uia_root_session`); Nicht-UIA-Sessions verwerfen Reasoning
weiterhin unverändert. `harw-tui/src/app.rs` rendert das Item bereits über
die vollständig verdrahtete `ReasoningHistoryCell` (gedimmter Text,
`·`-Präfix). `to_model_messages` (`history.rs`) überspringt
`TurnItem::Reasoning` beim nächsten Modellaufruf.

### 2.6.7 Export-Lesbarkeit (Ist-Stand 2026-09-21)

`/export` (§2.1 der TUI-Annex) ist um mehrere Lesbarkeits-Korrekturen
ergänzt, ohne die Grammatik zu ändern:

- Das Startdatum (`ExportMeta.started_at`) wird lesbar formatiert
  (`jiff::Timestamp`, lokaler Offset, RFC-3339-Stil) statt als rohe
  Unix-Sekunde ausgegeben; der Dateiname behält weiterhin den
  Sekunden-Token. Bei fortgesetzter Session (`-r`) stammt das Datum vom
  Session-Start aus dem Session-Store, nicht vom TUI-Start.
- Tool-Argumente und -Ergebnisse werden über einen neuen Helfer
  `render_json_block` dargestellt: ist ein String-Blatt (vor allem `value`)
  selbst JSON, wird es geparst und als verschachteltes Pretty-JSON
  ausgegeben; sonst kommt ein eigener ` ```text ` -Block mit echten
  Zeilenumbrüchen statt einer einzelnen, potenziell zehntausende Zeichen
  langen Zeile.
- Jeder Eintrag ist zusätzlich auf `ExportOptions.max_chars_per_entry`
  (Default 4000 Zeichen) gekappt, mit Marker `_[gekürzt: N Zeichen]_`,
  zeichengrenzen-sicher wie das bestehende `truncate_markdown`; die globale
  `--max-chars`-Kappung des gesamten Exports bleibt zusätzlich bestehen.
- Überschriften (`#…`-Zeilen) in User- und Assistant-Text werden um zwei
  Ebenen herabgestuft (maximal `######`), Fenced-Code-Blöcke werden dabei
  übersprungen.
- Tool-Einträge (ToolCall/ToolResult/Reasoning), die auf eine
  Nutzernachricht folgen, landen jetzt unter der Überschrift „## harw" statt
  fälschlich unter „## Du".
- Scheitert das Kopieren in die Zwischenablage beim reinen `/export` (z. B.
  weil harw in tmux ohne Zwischenablage läuft), schreibt es stattdessen die
  Datei über den bestehenden `default_export_path` und meldet den Pfad. Die
  OSC-52-Ausgabe wird in tmux zusätzlich mit einer
  DCS-Passthrough-Hülle (`\ePtmux;…\e\\`) umschlossen, wenn `TMUX` gesetzt
  ist.

---

## 3. `$` prefix — reserved, unassigned for v1

The TUI annex (§3) already reserves `$` without assigning it, listing two
candidates: variable/env interpolation (`$SESSION`) and a budget/cost query
shorthand. This document makes that reservation the final v1 decision,
rather than an open question, for one reason: **grammar squatting**.

Every other prefix in §3 of the TUI annex (`/`, `!`, `!!`, `#`, `@`) has an
unambiguous, singular meaning decided before a single line of parser code
exists. `$` currently has two live candidates that are not obviously
compatible — variable expansion is a *pre-parse* text-substitution concern
(closer to shell behavior, would need to interact with quoting/escaping
rules in §1.1 and §3.1 of the TUI annex before any command even sees its
arguments), while a budget/cost query is an *ordinary command* dressed as a
prefix, more consistent with the existing `/status`-shaped commands than
with a fifth ambient sigil. Picking one now, before either has a full design
pass, risks committing the prefix to whichever candidate got written down
first rather than whichever is actually right — and once a prefix ships and
gets muscle-memory usage, changing its meaning is a breaking change no
version bump inside v1 gets to make casually.

**Decision for v1: `$` is parsed, recognized as a reserved prefix character,
and rejected with a structured error** (a new `CommandError::ReservedPrefix
{ prefix: '$' }` variant, following the same "structured error over silent
fallthrough" convention as `CommandError::UnresolvedRef`) **rather than
falling through to ordinary chat text.** This keeps the door open for either
candidate — or a third one — without any input written today silently
changing meaning later. Candidate designs remain tracked as an open decision
(§6, item 1).

---

## 4. Unified approval model

The TUI annex's `/approve`/`/deny` commands (§2.3) and the channel annex's
Telegram inline-keyboard approval flow (§4.2) are **one model**, not two
that happen to look similar. This section is the single specification both
front-ends implement against; where either annex's prose differs in detail
from what follows, this document's version is normative.

### 4.1 One request/response pair

`harw-protocol::approvals` defines exactly one `ApprovalRequest` and one
`ApprovalResponse` shape, used for every approval-gated action in the
harness: a `/approve`/`/deny` pending work item, a `RiskLevel::High` kanban
claim (knowledge annex §6.4), a channel-invoked `/skills run` (§2.4.1
above), and a `/dream review` per-proposal decision.

```rust
// harw-protocol/src/approvals.rs (shape referenced by both annexes; stated
// here once, normatively, rather than duplicated with drift risk)

/// One pending decision point, addressed at exactly one WorkId (or, for a
/// dream review, one artifact-proposal within a DreamReport).
pub struct ApprovalRequest {
    pub id: ApprovalId,
    pub work_id: WorkId,
    pub summary: String,          // human-readable, rendered verbatim on every front-end
    pub risk: RiskLevel,
    pub requested_at: chrono::DateTime<chrono::Utc>,
    pub timeout_at: chrono::DateTime<chrono::Utc>,
    pub decisions: &'static [ReviewDecision], // which buttons/subcommands are offered
}

/// The one outcome type every front-end produces, however the human
/// expressed it (`/approve`, `a` keybinding, or a tapped inline-keyboard
/// button).
pub struct ApprovalResponse {
    pub request_id: ApprovalId,
    pub decision: ReviewDecision,   // Approve | Deny { reason: String } | ...
    pub actor: ApprovalActor,       // Tui { session: SessionKey } | Channel { channel: ChannelId, peer: PeerId }
    pub decided_at: chrono::DateTime<chrono::Utc>,
}
```

### 4.2 Front-end mapping — same request, different rendering

| Front-end | Presentation | Decision capture |
|---|---|---|
| TUI, work-graph panel | `/review <WorkId>` opens the diff/plan; `a`/`d` keybindings (§4.2, TUI annex) or `/approve`/`/deny` commands | Keypress or command both construct the same `ApprovalResponse` — the keybinding is sugar for the command, not a separate code path |
| TUI, other surfaces | `/approve <WorkId> [--note=text]` / `/deny <WorkId> --reason=text` | Same |
| Telegram | Inline keyboard rendered from `ApprovalRequest.decisions` (channel annex §4.2) | A `callback_query` maps to `ApprovalResponse` via the in-flight-approvals table keyed by `(chat_id, message_id)` (channel annex §4.2); typed `yes`/`no` fallback text is parsed into the same response type when inline keyboards aren't available |

No front-end constructs an `ApprovalResponse` through a path the other
doesn't also use — the registry dispatch step that consumes an
`ApprovalResponse` cannot tell, and does not need to tell, which front-end
produced it.

### 4.3 One audit rule, not two

Every `ApprovalResponse`, regardless of origin, is journaled with the same
fields: `ApprovalRequest.id`, `WorkId`, the deciding identity (`SessionKey`
for TUI, `SenderRef`+`ChannelId`+`PeerId` for a channel), the decision,
`decided_at`, and — for a channel origin — the raw wire event id (e.g.
Telegram `update_id`) for forensic replay, exactly as the channel annex
already specifies for its own case (§4.2, §5). This document's contribution
is stating that the TUI path writes the *same* audit record shape (with
`ApprovalActor::Tui` in place of `ApprovalActor::Channel`), so an
operator auditing approvals never needs to know or care which front-end a
given decision came through — one audit query, one report format, one
retention policy.

### 4.4 Timeout default

Consistent with the channel annex's Telegram-specific default (§4.2): an
`ApprovalRequest` that receives no `ApprovalResponse` before `timeout_at`
resolves to `ReviewDecision::Deny` on every front-end, not just Telegram.
The TUI additionally reflects this in the work-graph panel (the item moves
out of the pending-review state with a `timed-out-denied` annotation,
mirroring how Telegram edits the message to show the timeout outcome) so
neither front-end's view of an approval can silently disagree with the
harness's actual recorded decision.

---

## 5. Terminology note: two different "operator-only" concepts

Resolving a naming collision found while merging the three documents (see
§7, item 4, for the full list of naming issues resolved): the phrase
"operator-only" appears in two places that are **not the same axis** and
must not be conflated:

- `PermissionTier::Owner` (TUI annex §1.5) is a **command-permission tier** —
  the ceiling on which slash commands a given caller identity may invoke.
  It is strictly ordered against `Observer < Operator < Maintainer < Owner`.
- `VisibilityScope::OperatorOnly` (knowledge annex §7) is a **knowledge-
  artifact visibility scope** — a property of who may read/write a given
  `KnowledgeArtifact` (e.g. dream output defaults to `OperatorOnly`), enforced
  by `harw-policy` independently of which command tier the reading identity
  otherwise holds.

Where this document's plugin/skill requirements describe a command as
"OperatorOnly", that is shorthand for **`PermissionTier::Owner` combined
with `CommandScope::TuiOnly`** (§2.3, §2.4 above) — the highest command tier,
reachable only from the local terminal. It is not `VisibilityScope
::OperatorOnly` and should never be written that way in code or future docs;
the two enums live in different crates (`harw-tui`/`harw-config` vs.
`harw-knowledge`/`harw-policy`) and answer different questions ("can this
identity run this command" vs. "can this identity see this artifact").

---

## 6. Open Decisions register

Every unresolved question raised across the three annexes plus this
document, in one place, numbered, with a status. `open` = genuinely
undecided; `leaning` = a direction is favored but not locked; `blocked-on`
= cannot be resolved until a named dependency exists.

| # | Decision | Status | Source | Notes |
|---|---|---|---|---|
| 1 | `$` prefix final semantic (variable expansion vs. budget/cost query vs. other) | open | TUI annex §7.1 | Prefix reservation itself is now decided (§3, this document); the *assignment* remains open |
| 2 | `/spawn` budget accounting: hard cap vs. soft warning for child-agent token/time budget charged against the parent | blocked-on `harw-job-runtime::Budget` design | TUI annex §7.2 | Same dependency as item 12 below |
| 3 | Command versioning across future channel-native command registration (e.g. re-generating native Telegram bot commands from the internal registry as the registry evolves) | open | TUI annex §7.4 | Channel annex §4.1 already covers per-scope `setMyCommands` generation from a fixed registry snapshot; versioning *across* registry changes is the open part |
| 4 | `/broadcast` fan-out ordering and partial-failure reporting across multiple `ChannelRef`s | blocked-on `harw-catalog` channel registry existing | TUI annex §7.5 | |
| 5 | Chord timeout tunability per-action vs. one global default | open | TUI annex §7.6 | |
| 6 | Pairing-code delivery channel: same unauthenticated DM vs. operator-only side channel | open | Channel annex §Open-1 | Low sensitivity either way per the annex's own framing |
| 7 | `shared_session` topic mode: does the audit log still record the physical `thread_id` even though it isn't part of `SessionKey`? | leaning yes (non-key attribute on the journaled event) | Channel annex §Open-2 | Not yet checked against the session-store journal's actual event schema |
| 8 | Local Bot API server support: fully-trusted extension of the adapter's process vs. its own sandboxing story | open, deferred out of v1 | Channel annex §Open-3 | |
| 9 | Webhook multiplexing: one listener per binding vs. one shared listener by path | leaning toward always-upstream reverse proxy, plain HTTP on loopback | Channel annex §Open-4 | |
| 10 | Cross-channel `TenantId` reuse / session handoff (e.g. continue a Telegram conversation on Slack) | open, flagged for a later `harw-session-store` note | Channel annex §Open-5 | |
| 11 | Palace confidence decay: auto-downgrade `Established` → `Provisional` after inactivity, or surface-only staleness | leaning surface-only | Knowledge annex §Open-1 | Must be decided before `Confidence` enum locks |
| 12 | Workbench retention default for project-scoped surfaces: property of `WorkbenchScope` vs. one global config key | open | Knowledge annex §Open-2 | |
| 13 | Dream job `max_tool_calls` concrete default | blocked-on `harw-job-runtime::Budget` units finalizing | Knowledge annex §Open-3 | Same dependency as item 2 above — both should be resolved in the same pass once `Budget` lands |
| 14 | Kanban storage under truly concurrent OS-process writers (file-locking mechanism, if `harw-job-runtime` ever spawns separate processes rather than threads) | blocked-on `harw-job-runtime` process-vs-thread worker model decision | Knowledge annex §Open-4 | Currently theoretical; becomes real the moment process-based workers ship |
| 15 | Who is authorized to grant `ExplicitlyGranted` palace-visibility widening (the promoting agent, or only an operator) | open, likely folds into `harw-policy`'s approval model | Knowledge annex §Open-5 | |
| 16 | Should `CardState::Blocked{ReviewRequired}` transitions require a linked diary entry rather than a free-text comment | open | Knowledge annex §Open-6 | |
| 17 | `/plugins install` capability-diff format: fixed human-readable template vs. structured + rendered per front-end (only matters if a future channel front-end ever needs to show *any* part of an install flow, which §2.3 currently rules out entirely by making it `TuiOnly`) | open, low priority while install stays TuiOnly | This document, §2.3.1 | Revisit only if a future decision loosens install's `TuiOnly` scope |
| 18 | `CardRef`/`LaneRef`/`BoardRef` ambiguity resolution when the same short name exists on two boards the caller can see | open | This document, §2.1 | The surface form sketch allows a `board:<id>/<lane>` disambiguator but the *default* resolution order (active board first? error on ambiguity?) isn't specified |
| 19 | Whether `/skills run`'s approval requirement should vary by declared skill risk tier (analogous to kanban's `RiskLevel::High` gate) rather than uniformly requiring approval on every channel invocation | open | This document, §2.4.1 | Current rule is intentionally conservative (always require approval from a channel); a risk-tiered skill catalog could relax this for low-risk skills once one exists |

**Register total: 19 open decisions** (5 carried from the TUI annex, 5 from
the channel annex, 6 from the knowledge annex, 3 newly raised while writing
this document).

---

## 7. Naming inconsistencies found and resolved

While merging the three annexes into one grammar, the following naming
issues surfaced. All are resolved as stated here; the annexes themselves are
left unedited (per this task's scope), so this section is the canonical
correction where an annex's wording would otherwise mislead a reader of the
combined system.

1. **`/plugins` and `/skills` had two different definitions.** The TUI
   annex (§2.5) gave both commands one-line, flag-based forms
   (`/plugins [--list | --enable=name | --disable=name]`,
   `/skills [SkillRef] [--search=text] [--install]`). This document's §2.3
   and §2.4 are the superseding, complete subcommand grammars — the annex's
   flag forms should be read as an earlier sketch, not a second valid
   grammar. No code should implement both.
2. **`OperatorOnly` used for two unrelated concepts.** The knowledge annex's
   `VisibilityScope::OperatorOnly` (an artifact-visibility scope) and this
   task's own requirement language ("install/remove/enable/disable =
   OperatorOnly") describe different axes — the latter is resolved to mean
   `PermissionTier::Owner` + `CommandScope::TuiOnly` (§5, this document).
   Future documents should say "Owner-tier, TUI-only" for the command axis
   and reserve "OperatorOnly" exclusively for `VisibilityScope`.
3. **`/workbench` parity was left as a single deferred note.** The TUI
   annex's parity table (§6) marks `/diary`, `/palace`, `/workbench` as
   "Reduced / TUI-only (see per-command note in the knowledge-surface design
   doc)" without the knowledge annex actually specifying a per-command split
   for `/workbench`. §2.2 of this document resolves it at the subcommand
   level (`pin` and the bare panel form are TUI-only; `note` and
   `hypothesis` are `ChannelReduced`) rather than leaving the whole surface
   as one undifferentiated deferral.
4. **`AgentRoleRef` vs. `AgentRef` were never reconciled.** The knowledge
   annex's kanban `Card.assignee` and `Lane::Worker { agent_role:
   AgentRoleRef }` use `AgentRoleRef` (a *class* of agent, e.g. a worker
   role), while the TUI annex's typed-argument table only defines
   `AgentRef` (a specific running agent/session-child). These are genuinely
   different things — a kanban lane is bound to a *role*, not a running
   instance — so both are kept as distinct typed arguments; `AgentRoleRef`
   is added to the ref-resolver system in §2.1 of this document rather than
   overloading `AgentRef` to mean two things depending on context.
5. **`ArtifactId` vs. a missing typed command argument.** The knowledge
   annex defines `ArtifactId` as the stable identifier for every
   `KnowledgeArtifact`, but no command in the TUI annex's typed-reference
   table could address one — `/palace show <node-id>` and `/memory show`
   (knowledge annex §8.2) used a bare "node-id"/"topic-id" string with no
   parser-level type. §2.1 of this document introduces `ArtifactRef` as the
   typed argument every knowledge-surface command uses, closing that gap.
6. **Command inventory totals were inconsistent by construction, not error.**
   The TUI annex's "56 commands" headline (§2, footer) never claimed to
   count the knowledge surfaces or the catalog placeholders at full
   subcommand depth — it explicitly flagged both as reserved-names-only
   (§7.3, TUI annex). §2.5 of this document is the corrected, full count
   (80) once those two areas are worked out; this is a completion of scope
   the TUI annex itself deferred, not a contradiction of it.

---

## 8. Summary of what changed relative to reading the three annexes alone

- The command grammar grew from 56 headline commands to **80**, once
  knowledge surfaces (22 subcommands) and the two catalog domains
  (`/plugins` 7, `/skills` 7) are worked out to full depth instead of
  one-liners/placeholders (§2).
- Six new typed arguments (`BundleRef`, `CardRef`, `LaneRef`, `BoardRef`,
  `AgentRoleRef`, `ArtifactRef`) join the ref-resolver system (§2.1).
- One approval model, not two — TUI keypresses/commands and Telegram inline
  keyboards construct the identical `ApprovalResponse`, audited identically
  (§4).
- `$` stays reserved and unassigned for v1, now for a stated reason
  (grammar squatting) rather than as an unexplained gap (§3).
- Nineteen open decisions from across the three annexes are tracked in one
  register with status (§6), and six naming inconsistencies the merge
  surfaced are resolved with an explicit corrected reading (§7).
