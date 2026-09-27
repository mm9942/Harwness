# Documentation

Everything under `docs/`, grouped by purpose. Design and architecture
documents carry a status line under their title (`implemented`,
`partially implemented` or `proposal`) and the date of their last review.
The code is the source of truth; if a document and the code disagree, the
code wins and the document is a bug.

## Getting started

- [Installation](setup/install.md) — *implemented*
- [Build prerequisites](setup/build-prerequisites.md)
- [Local models: vLLM, LM Studio and Ollama](setup/local-models.md)
- [DoD (Defense-on-Device)](setup/dod.md) — *partially implemented*
- [Control plane (`harw web`)](setup/web.md) — *implemented*
- [`crypt_guard` integration in `harw-secrets`](setup/crypt-guard.md)

## Using Harwness

- [`harw` command line](cli.md)
- [Background orchestrators: control, messaging, handoff](guides/background-agents.md)
- [Compiling agents into standalone binaries](guides/agent-compiler.md) — *planned in #22*

## Philosophy

- [Harwness Coding Philosophy](philosophy/coding-philosophy.md)
- [Agent harnesses as an operating-system layer for long-lived, model-heterogeneous AI agents](philosophy/philosophy.md)

## Architecture decisions

Architecture decision records live in [`adr/`](adr/), numbered in order.

| ADR | Status |
| --- | --- |
| [0001 — Agent compiler: IR v2, artifacts and standalone binaries](adr/0001-agent-compiler.md) | accepted, partially implemented |

## Architecture

| Document | Status |
| --- | --- |
| [Architecture: Model and Provider Routing](architecture/model-provider-routing.md) | implemented |
| [Operation Registry — Single Source of Truth](architecture/operation-registry.md) | implemented |
| [Architecture: TuiSessionController (Long-lived)](architecture/session-controller.md) | implemented |

## Design

| Document | Status |
| --- | --- |
| [CONTRACT MASTER — Setup & Install Lifecycle](design/CONTRACT-setup-install.md) | implemented |
| [Agent Composition Contract — Design Only (post-0.2.0)](design/agent-composition-contract.md) | proposal |
| [Harwness Agent Definition DSL](design/agent-definition-dsl.md) | partially implemented |
| [Agent Artifact v1 — the binary format of a compiled agent](design/agent-artifact-v1.md) | proposal |
| [Agent IR v1 — the existing pipeline as an explicit compiler channel](design/agent-ir-v1.md) | partially implemented |
| [Agent-as-tool execution contract](design/agent-tool-loop.md) | implemented |
| [Agents as Tools](design/agents-as-tools.md) | implemented |
| [Build History](design/build-history.md) | implemented |
| [Channel Ingress Layer — Design, with Telegram as First Binding](design/channel-ingress-telegram.md) | implemented |
| [Config scopes: GLOBAL vs. PROFILE](design/config-scopes.md) | implemented |
| [Config Structure — the `.harw/` Tree](design/config-structure.md) | implemented |
| [Harwness Crate Inventory (Procurement List)](design/crates-inventory.md) | partially implemented |
| [Delegation capabilities and the local organization graph](design/delegation-capabilities.md) | implemented |
| [DoD System Operations, eBPF and Authority Core — Implementation Plan](design/dod-system-operations-authority.md) | partially implemented |
| [Hardening Status](design/hardening-gap-analysis.md) | partially implemented |
| [`harw-dod`: Charter of the Facade](design/harw-dod-charter.md) | implemented |
| [`harw-dod`: Crate Decomposition, Responsibilities and Rights Matrix](design/harw-dod-crate-decomposition.md) | implemented |
| [Harwness DoD × Context: Integration Plan and Dependency Doctrine](design/harw-dod-integration-and-dependencies.md) | partially implemented |
| [Extending Sensors: Contract, Harness and Cookbook](design/harw-dod-sensor-extensibility-plan.md) | partially implemented |
| [`harw-lens`: Knowledge Index, Vector Store and Retrieval for Harwness](design/harw-lens-plan.md) | implemented |
| [harw-memory — Long-Term Memory & Continuous Improvement](design/harw-memory.md) | implemented |
| [Harwness Security & Observability Subsystem](design/harw-security-observability-plan.md) | partially implemented |
| [Telemetry Export: OTLP, Prometheus and the Sink Boundary](design/harw-telemetry-integration-plan.md) | implemented |
| [Harwness Interaction Contract — Master Document](design/interaction-contract.md) | implemented |
| [Knowledge & Work Surfaces](design/knowledge-surfaces.md) | implemented |
| [Matrix Game — Umpire-Led Multiplayer Scenarios with Agents](design/matrix-game.md) | implemented |
| [Mediated Process Execution and Sandbox Modules](design/mediated-process-execution.md) | implemented |
| [harw-memory Promotion/Decay Runtime](design/memory-promotion.md) | implemented |
| [Memory v2 — STM/LTM, Self-Learning, Continuous Improvement](design/memory-v2.md) | implemented |
| [Memory v3 — Long-Term Memory (LTM) with Scopes, Facts, and Consolidation](design/memory-v3-ltm.md) | implemented |
| [Provider model refresh and persistent setup](design/model-catalog-refresh.md) | implemented |
| [Model Catalog — Four-Layer Architecture](design/model-catalog-v2.md) | implemented |
| [Model Capabilities & Behavior-Profile Registry](design/model-registry.md) | implemented |
| [harw-plan v1 — First-Class Planning Tool](design/planning-tool-v1.md) | implemented |
| [Provider Auth & Foundry Gateway — Design (Slice 1)](design/provider-auth-gateway.md) | implemented |
| [Design: Multi-Provider Onboarding with a ratatui Setup Screen](design/provider-tui-setup.md) | implemented |
| [Runtime assembly contracts](design/runtime-contracts.md) | implemented |
| [Secrets & Audit](design/secrets-and-audit.md) | partially implemented |
| [Telegram work requests and sandboxed execution](design/telegram-sandbox-work-requests.md) | partially implemented |
| [Token Efficiency](design/token-efficiency.md) | partially implemented |
| [Tool Canon v1 — a Rust-idiomatic design](design/tool-canon-v1.md) | proposal |
| [harw-tui Architecture](design/tui-architecture.md) | implemented |
| [Harwness TUI Interaction Contract](design/tui-command-contract.md) | partially implemented |
| [Roles, Models, Modes, and Approval in the TUI](design/tui-roles-models-modes.md) | implemented |
| [Business Wargaming, Analytical Tradecraft, and Visualization](design/wargaming-and-analysis.md) | partially implemented |

## Planning

Future architecture lives in [`planning/`](planning/README.md), not here. Planning documents describe intended deltas against a pinned baseline commit and are never implemented status; the [migration ledger](planning/90-migration-ledger/MIGRATION_LEDGER.md) records when a plan lands in code.

## Audits and research

- [Agent Capabilities Audit](audits/agent-capabilities-audit.md) — *partially implemented*
- [Tool Inventory: Codex / Hermes / OpenClaw](research/tool-inventory.md) — *implemented*

Raw model metadata used by the model catalog lives in
[`research/models/`](research/models/).

## Migration

- [Migration Guide: 0.1.0 → 0.2.0](migration/0.1.0-to-0.2.0.md)

## Future ideas

- A policy-first core for local Android agents was explored and removed from
  the workspace until it can be built and tested in CI alongside the rest.
- See the *Open* items in [Hardening Status](design/hardening-gap-analysis.md),
  [Business Wargaming](design/wargaming-and-analysis.md) and
  [Token Efficiency](design/token-efficiency.md) for planned work.
