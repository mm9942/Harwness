# Telemetry Export: OTLP, Prometheus and the Sink Boundary

> Status: implemented · Last reviewed: 2026-09-24

**Purpose:** how telemetry leaves the system, what dependencies that costs,
and why none of them sit in the core.
**Related:** `harw-dod-integration-and-dependencies.md` (dependency
doctrine D1–D9)

---

## 0. The base decision

`harw-observe` defines `TelemetrySink` (`harw-observe/src/sink.rs`) and
knows no backend. This was a deliberate research decision: as of this
review, OpenTelemetry-Rust's first-party crates (`opentelemetry`,
`opentelemetry_sdk`, `opentelemetry-otlp`) are still pre-1.0, version-locked
across the family, with breaks in minor releases — and traces trail logs
and metrics to stability, the reverse of the order in Go and Java.

A dependency like that in the core of a system built for years would be a
recurring rebuild. Behind its own trait, it's an adapter you swap.

**Rule:** no type from a backend crate ever appears in a public signature
of `harw-observe`, `harw-dod` or `harw`. This is dependency doctrine D5, and
this is its most important application.

---

## 1. Three sinks, three crates

| Crate | Transport | Dependencies | Purpose |
|---|---|---|---|
| `harw-observe-file` | JSONL on disk | none beyond `serde_json` | default; always available, air-gap-friendly, forensically usable |
| `harw-observe-prom` | pull, text exposition | hand-written rendering, minimal HTTP handler | classic ops integration without a collector |
| `harw-observe-otlp` | push, OTLP/JSON over HTTP | hand-written OTLP/JSON body construction over `hyper` — **not** the `opentelemetry`/`opentelemetry-otlp` crates | when a collector exists and export is wanted |

All three implement `TelemetrySink`. None knows about the others. The core
knows about none of them. All three are implemented.

### 1.1 `harw-observe-file` is the default, not the fallback

A line-oriented JSONL file with timestamp, metric key, labels and value
costs almost nothing, has no dependency, runs with no network, and is
exactly what you want during a security incident: a local, append-only file
you can take with you. Implemented under `harw-home`, rotating by size,
with a checksum written for each closed file.

For the sentinel, this is the only sink active by default.

### 1.2 `harw-observe-prom` is hand-built

**Implemented as planned:** `harw-observe-prom` has no client-library
dependency beyond `harw-observe`/`harw-macros` — see `Cargo.toml`. The
Prometheus text format (`# HELP`, `# TYPE`, metric lines) is rendered
directly from `MetricKey` (`format.rs`); a client library would have
introduced a second metric model to reconcile against Harwness' own. The
endpoint itself (`endpoint.rs`) is a minimal HTTP handler bound to loopback
or a Unix socket, never `0.0.0.0` — treated as an operator-tier surface,
since an open metrics endpoint is a map of the system.

### 1.3 `harw-observe-otlp`: implemented, and more conservative than planned

This document originally proposed wrapping the OpenTelemetry crate family
behind an adapter (via `opentelemetry`/`opentelemetry-otlp`, with an HTTP
transport preferred over gRPC/tonic to keep the tree small). **The actual
implementation goes further: it does not depend on the OpenTelemetry crate
family at all.** `harw-observe-otlp/src/lib.rs` builds the OTLP/JSON
request bodies itself (module doc: "the OTel adapter isolation, and why it
exists" — citing doctrine D5 by name as the reason) and ships them over a
hand-written HTTP transport built on `hyper`/`hyper-util` (reusing feature
flags already resolved elsewhere in the workspace, so this adds no new
crate to the lockfile beyond feature unions). This sidesteps the
version-churn risk entirely rather than merely isolating it behind an
adapter — there is no OTel dependency to isolate.

**The warden never links this crate.** It writes to the file sink; the
sentinel exports.

---

## 2. Routing by namespace

Where network/security disjointness meets telemetry — not every metric may
go everywhere. Implemented in `harw-observe/src/routing.rs`.

| Namespace | Default | Rationale |
|---|---|---|
| `context.*`, `plan.*`, `goal.*`, `model.*` | all configured sinks | operational data, non-sensitive |
| `sensor.*`, `host.*` | all configured sinks | host health, non-sensitive |
| `security.*` | **file sink only** | finding rates and rule hits map out the system's weak points |
| `warden.*` | **file sink only** | same, plus the action history |
| meta zero-counters | all sinks | their value is zero; their existence isn't information |

Exporting `security.*` is a deliberate, operator-confirmed configuration
step, never a default — otherwise an attacker reading the metrics sees
exactly which rules fire and which are blind.

---

## 3. Cardinality as a contract

`Cardinality`, a field on `MetricKey`, is enforcement, not documentation.

**Forbidden as a label, without exception:** full paths, PIDs, session IDs,
turn IDs, IP addresses, usernames, filenames, process arguments, model
response text — each of these is unbounded-cardinality, personally
identifying, or both.

**Allowed:** closed enums (role, step kind, omission reason, severity,
sensor status), bounded enumerations (model ID, provider ID, section name,
sensor ID), small numeric ranges (core number).

**Edge cases with a rule:** cgroup identity is labeled as role plus depth,
never a full path. An executable digest is never a label, only ever an
event field. Plan nodes are labeled by kind, never by task ID.

Enforcement is two-tiered: the metric macro rejects an undeclared label at
compile time, and the sink rejects an oversized label set at runtime, with
a dedicated exceeded-cardinality counter. What can't be measured without
overloading a time-series store is carried as an event, not a metric — the
actual dividing line between the two streams.

---

## 4. Traces, spans and the bridge

Existing `tracing` call sites stay as they are. Implemented pieces:
`TraceContext` (`harw-observe/src/trace.rs`) carries only `String`/
`Option<String>` fields (no OTel type), with validating constructors.

**Open:** a `tracing-opentelemetry` layer bridging Rust `tracing` spans
into OTLP traces. No dependency on `tracing-opentelemetry` exists in the
workspace as of this review. This is consistent with §1.3: since
`harw-observe-otlp` deliberately avoids the OTel crate family for metrics
export, a trace bridge through `tracing-opentelemetry` (which pulls in that
family) would reintroduce the dependency this design avoided. If a trace
export path is wanted, either OTLP/JSON traces would need to be hand-built
the same way metrics export is, or the OTel-family dependency would need to
be reconsidered — this decision has not been made.

**Exemplars** (linking a histogram point to a specific trace ID) remain the
main payoff of a trace bridge and stay open along with it.

---

## 5. Dependency analysis (updated against the actual implementation)

### 5.1 What the OTLP sink costs — actual, not the originally planned cost

| Layer | Crates | Character |
|---|---|---|
| Body construction | none (hand-written OTLP/JSON) | in-house, `harw-observe-otlp/src/schema.rs` and friends |
| Transport | `hyper`, `hyper-util`, `http-body-util`, `bytes`, `tokio` | pure Rust; these crates and versions were already resolved elsewhere in the workspace, so no net-new crate enters the lockfile |
| Serialization | `serde`, `serde_json` | already in the tree |

This is meaningfully cheaper than the `opentelemetry`/`opentelemetry-otlp`
route originally evaluated in this document — no pre-1.0, family-versioned
dependency at all.

### 5.2 What the Prometheus sink costs

Confirmed near-zero: `harw-observe`/`harw-macros` only.

### 5.3 What the file sink costs

Confirmed: `serde_json`, `blake3`, `jiff` — all already in the tree.

### 5.4 Assessment against the doctrine

| Rule | OTLP | Prom (hand-built) | File |
|---|---|---|---|
| D1 pure Rust | met | met | met |
| D2 no C build | met | met | met |
| D5 not in public types | met — there is nothing OTel to isolate | met | met |
| D6 build it yourself for a small surface | met, more thoroughly than originally proposed | met | met |
| D7 warden budget | never linked | never linked | linked |
| D8 CI gates | `cargo deny` covers it like any crate | trivial | trivial |

---

## 6. Build history (informational)

The subsystem was built in this order: sink trait and routing vocabulary
first (no export); the file sink and self-observation metric emission
next, giving local numbers with no dependency; the Prometheus sink with its
loopback endpoint after that; namespace routing tightened (`security.*`/
`warden.*` restricted to the file sink) once the security subsystem needed
it; the OTLP sink last, built hand-rolled rather than via the OTel crate
family per §1.3/§5.1. All of the above is implemented; the tracing-to-OTLP
trace bridge (§4) remains open.

---

## 7. Checks

- **Content freedom.** A property test over every redaction
  implementation: no sink output ever contains an omitted-class value or
  the plaintext of a digest field.
- **Cardinality limit.** A load test that drives a high-cardinality sensor
  and checks the sink rejects rather than grows unbounded.
- **Routing.** A test that a `security.*` metric without operator
  confirmation lands only in the file sink.
- **Naming rules.** A golden test of the Prometheus output against a
  frozen expectation, so naming/unit conventions don't drift.
- **Adapter isolation.** A CI step checking that no public signature in
  `harw-observe`, `harw-dod` or `harw` names a type from the OpenTelemetry
  family — trivially true today since no such dependency exists at all.

---

## 8. Open items

1. **Exporter choice at high event rates.** If sensors like `procmon`/
   `flow` produce more than a push exporter can carry, the answer is
   pre-aggregation, not a transport change. The aggregation threshold is
   still undefined.
2. **File-sink retention.** Rotation by size is set; retention duration is
   not, and depends on how far back forensic lookback needs to reach.
3. **Histogram buckets** for turn latency and context sizes need real
   measurements before sensible bounds can be set.
4. **Trace bridge decision.** Whether to hand-build OTLP/JSON trace export
   (consistent with the metrics sink) or accept the OTel-family dependency
   via `tracing-opentelemetry` for traces only.
