# Build History

> Status: implemented · Last reviewed: 2026-09-24

This is a compact record of how the Harwness workspace grew from its
foundation crates to its current ~95-crate shape, what the resulting
architectural layers are, and which decisions from that process still hold as
invariants in the code today. It replaces two large planning documents
(`aw-plan.md`, `aw-contract-master.md`) that tracked the work node-by-node;
their normative content is folded in here, their execution-log detail is not.

## How the workspace was built

The workspace grew in two stages.

**Stage 1 — foundation.** An earlier build established the core scheduling,
role, and execution primitives: sessions, turns, the plan/goal system,
budgets, and return paths. Everything described below assumes this layer
already existed and deliberately reuses it — no later subsystem invents its
own scheduler, fan-out mechanism, or result-return path.

**Stage 2 — the AW0–AW7 expansion program.** A second program added five
subsystems on top of that foundation, executed as a dependency-ordered wave
sequence (roughly 90 work nodes across 8 waves plus a cross-cutting UI
track). Each wave's write scope was kept disjoint from parallel waves so many
nodes could land concurrently without colliding on the same files:

| Wave | Focus | What it produced |
|---|---|---|
| AW0 | Vocabulary & foundation | Workspace-wide `Cargo.toml` conventions, `unsafe_code = "forbid"`, shared ID/digest newtypes (`harw-types`), the telemetry vocabulary (`harw-observe`), capability/sensor vocabulary (`harw-dod-cap`), retrieval vocabulary (`harw-lens-types`) |
| AW1 | Self-observation & assembly v2 | File-backed telemetry sink, context assembly rework |
| AW2 | Programs & observers | Context programs, the ten-sensor observation daemon |
| AW3 | Sets & sources | Network egress target sets, retrieval sources |
| AW4 | Boundary & rules | Security rule engine, context ceiling/trust-block separation |
| AW5 | Enforcement & references | Warden (privileged enforcement binary), cross-references |
| AW6 | Agents | Agent roles wired to the new subsystems |
| AW7 | Operations & hardening | systemd units, privilege-tier binaries, hardening passes |
| UI | Control plane | `harw-web`, generated route inventory, the web front end |

The program's own before-work audit found that the fundamentals it built on
already existed almost exactly as assumed, but flagged that the crate-adding
plan itself needed correction before parallel execution could start safely —
most importantly, that new crates had to be reserved in the workspace
manifest as a single up-front step rather than something many parallel agents
edited at once. That correction became its own first node in every wave.

## Resulting architecture: layers and crate groups

The workspace is organized as a small number of layers, each a group of
crates with one responsibility:

- **Vocabulary / shared types** — `harw-types` (IDs, content digests, shared
  enums), `harw-macros` (derive/attribute macros), `harw-config`.
- **Core runtime** — `harw-core`, `harw-core-bridge`, `harw-runtime`,
  `harw-operations` (the `#[operation]` registry — see
  `docs/architecture/operation-registry.md`), `harw-plan` /
  `harw-plan-bridge` (the planning tool), `harw-job-runtime`.
- **Providers & models** — `harw-provider`, `harw-provider-http`,
  `harw-model-catalog`, `harw-oauth`.
- **Memory & knowledge** — `harw-memory` (short-term ring buffer, HOT/WARM/COLD
  long-term tiers, fact store, promotion/decay, consolidation), `harw-knowledge`
  (memory palace, context steward), `harw-research`.
- **Retrieval (Lens)** — `harw-lens-types`, `harw-lens-chunk`, `harw-lens-embed`,
  `harw-lens-index`, `harw-lens-query`, `harw-lens-rank`, `harw-lens-source`,
  `harw-lens-store`, `harw-lens-federation`, `harw-lens` (facade).
- **Telemetry** — `harw-observe` (vocabulary + trait), `harw-observe-file`
  (default sink), `harw-observe-prom`, `harw-observe-otlp` (opt-in sinks
  behind a routing sink, wired to `app.*` metrics only).
- **Tools** — `harw-tools` (executor/spec/schema), plus one crate per tool
  family: `harw-tool-shell`, `harw-tool-fs`, `harw-tool-web`, `harw-tool-browser`,
  `harw-tool-process`, `harw-tool-plan`, `harw-tool-lens`, `harw-tool-doc`,
  `harw-tool-explorer`, `harw-tool-deps`.
- **Security / DoD (Defense-of-Depth)** — a separate, excluded workspace at
  `dod/` (27 sensor, rule, and escalation crates under `dod/crates/`, prefixed
  `harw-dod-*`), plus the privileged binaries `harw-sentinel` and
  `harw-warden`. Kept out of the main workspace deliberately: it has a much
  smaller, audited dependency budget than the rest of the tree.
- **Sandbox / egress / secrets** — `harw-sandbox`, `harw-egress`, `harw-secrets`,
  `harw-authority`.
- **Front ends** — `harw-tui` (terminal UI), `harw-web` (HTTP control plane,
  same operation registry as the CLI and TUI — no second authority path),
  `harw-cli`, `harw-channel` / `harw-channel-telegram*` (chat ingress).
- **Agents & extensions** — `harw-agent-dsl`, `harw-extension-api`,
  `harw-mcp-client` / `harw-mcp-server`.

## Key decisions that still hold

These are invariants from the AW0–AW7 program that are load-bearing in the
current code, not just historical rationale:

- **One `NetworkScope`, not two competing types.** Rather than add a parallel
  `EgressSet` type, the existing `NetworkScope` (`harw-sandbox`) was extended
  in place with typed egress targets (host / DNS suffix / CIDR), keeping
  `allows(&str)` unchanged and adding `allows_addr(IpAddr)` alongside it. All
  call sites kept working without changes.
- **`TelemetrySink` has a fixed shape.** Receiver is `&self`, return type is
  `()`, and the trait is synchronous — deliberately decided early because
  these three points are exactly where sink traits drift once many crates
  depend on them (roughly 40+ downstream crates in this case).
- **One confidence vocabulary.** Four independent `Confidence` types existed
  before this program. Two structurally identical ones (model catalog
  provenance, memory epistemic status) were merged into a single type in
  `harw-types`; a third, unrelated lifecycle enum in the knowledge/memory
  palace was renamed to `PalaceStatus` rather than folded in, because it is a
  lifecycle state, not a confidence scale.
- **`Sensor` and `Finding<S>` live in different crates.** The sensor trait
  lives in `harw-dod-signals`; the typed finding/triage state machine lives
  entirely in `harw-dod-rules`. No sensor crate mints a `Finding` itself —
  sensors only ever report raw signals.
- **`authorize` stays private.** The DoD capability/action-authorization
  function is `pub(crate)`, enforced by `compile_fail` doctests, so that
  authorization can only happen through the facade that names the specific
  action being authorized — never by constructing `Action`/`Authorized`
  directly.
- **CI gates over prose.** Structural invariants (forbidden dependency edges
  and pure-crate hulls, a per-binary privilege budget for the four DoD
  binaries, the Warden's runtime-dependency budget and its no-C-build rule)
  are enforced by `cargo run -p xtask -- gates` (`xtask/src/gates.rs`),
  which reads the dependency graph of the product workspace and the separate
  `dod/` workspace together, not by documentation asking contributors to
  remember a rule. CI runs the gates in the root job. A gate that checked
  nothing counts as red, not green. The earlier write-scope gate was retired
  together with the build plan's write-scope table.
- **No C build in the Warden's dependency hull.** The privileged enforcement
  binary's transitive dependencies were checked to confirm none carry a
  native build script; this is verified, not assumed.
- **Vocabulary crates stay thin.** Shared cross-cutting types migrate to
  `harw-types` rather than one subsystem crate re-exporting from another
  (which would pull in that crate's whole dependency tree for one enum).

## Known, documented gaps

A handful of built components are intentionally not wired to a production
caller yet, and are recorded as such in the code rather than left as silent
dead code: the DoD sensor facade (`harw-dod`) is deliberately not consumed by
the control-plane server (it belongs in an observation daemon like
`harw-sentinel`, not a request-forwarding server); `harw-dod-netpolicy` has
no enforcer wired in `harw-warden` because the evaluated approaches all
exceeded the accepted dependency budget; a confidential-data embedding
backend for `harw-lens-embed` was rejected because the only production-ready
local model pulls in on the order of 170 extra crates and a C toolchain, so
it stays HTTP-only. These are scoped, load-bearing decisions, not omissions.

## Where the detail went

Field-level type contracts that were pinned during the parallel build (exact
struct/enum shapes for `harw-observe`, `harw-types`, `harw-lens-types`,
`harw-context`, `harw-dod-cap`, `harw-dod-signals`) now live as doc comments
on those types in the code itself, which is the current source of truth. The
day-by-day correction log (wave-by-wave findings, retest results, node
counts) is not reproduced here; nothing normative in it survived that is not
already captured above or in the crates' own documentation.
