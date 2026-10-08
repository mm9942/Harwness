# PL-90 — Intent-Driven Composed Agents

> DRAFT; baseline dev@197a92e92d438bf6bf93b129bb964f4852686149; documentation, not runtime implementation.

This dossier defines the composed-agent facade, adaptive model-turn chains, bounded delegated orchestration, write-capable corrective cycles and domain recipes. Read [Current Architecture](01-current-architecture.md), [Adaptive Intent Contracts](02-adaptive-intent-contracts.md), [Domain Applications](03-domain-applications.md), and [Migration & Verification](04-migration-verification.md).

**Principle:** externally an agent is one accountable, intent-addressable unit; internally it is a dynamically evolving, evidence-directed execution graph composed of model turns, tools, child agents, nested chains, cells, checkpoints and jobs. Evidence changes its route, never silently its authority or agreed objective.

**CURRENT:** agent DSL, roles, named delegation, WorkDriver, gap-hunt workflows, Matrix Game, writer/reviewer, LaTeX, context/learning infrastructure. **PARTIAL IMPLEMENTATION (W01 + pure part of W02, tests green locally):** `harw-plan-bridge::intent_cycle` — fail-closed transition admission for the full transition family, provenance-bearing evidence, runtime-derived progress, epoch/sequence checkpoint fence and non-widening resume over `AuthoritySnapshot`; not yet wired to a job runner, provider, executor or tool surface (see [04 §7](04-migration-verification.md#7-implementation-status)). **PLANNED:** generic durable model-turn chain and composed-agent runtime. **DELTA:** contracts, adapters, preflight and gates specified in this dossier.
