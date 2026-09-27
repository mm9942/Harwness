# Harwness DoD × Context: Integration Plan and Dependency Doctrine

> Status: partially implemented · Last reviewed: 2026-09-24

**Bundles:** the security invariants behind `harw-dod-*` (see
`harw-security-observability-plan.md`) and the context invariants behind
`harw-context` (see `docs/design/runtime-contracts.md` for the current
context/ceiling contract).
**Complements:** `harw-dod-charter.md` (name, facade, intent).

---

## 0. Why these two areas share one plan

Security and context are not adjacent concerns, they interlock. Four points
of contact are close enough that building them separately would duplicate
mechanism:

1. **Disjointness between network and security context is not enforceable
   without a `ContextCeiling`.** A ceiling over permissions says what an
   agent may *do*. Disjointness requires a statement about what it may
   *see* — that is what `ContextCeiling` (`harw-context/src/ceiling.rs`)
   provides.
2. **The injection boundary for security findings and the trust classes of
   the context plan are the same mechanism.** Built twice, it would be two
   render paths and two fixture sets.
3. **`harw-observe` belongs to both.** Context metrics live in it, and its
   calibration loop feeds the security system's feedback channel.
4. **Security findings are the hardest context case there is.** Large
   evidence, high sensitivity, local inference. Reference-mode context
   loading exists for them, not incidentally.

Hence one vocabulary set and one fixture corpus, described together.

---

## 1. The four seams, in detail

### 1.1 Network/security disjointness as a set statement

```toml
# harwness.ceiling.security@1
[context]
universe  = ["goal.*", "plan.*", "security.*", "knowledge.palace",
             "memory.*", "project.root", "history.tail"]
forbidden = ["net.*", "transcript.sibling.*", "secrets.*"]
budget_max = { tokens_total = 8000 }

# harwness.ceiling.intel-scout@1
[context]
universe  = ["net.advisory", "project.deps", "history.tail"]
forbidden = ["security.*", "goal.*", "plan.*", "memory.*", "secrets.*"]
budget_max = { tokens_total = 4000 }
```

`ContextCeiling::intersect` is monotonic; `admits` checks at resolution
time. Disjointness is therefore not a prompting convention or a review
rule, it's a property the resolver rejects violations of. Implemented:
`harw-context/src/ceiling.rs`.

### 1.2 One injection boundary for both areas

`TrustClass` in `harw-context` is the carrier. Security event data is
`Data`, digest-secured evidence is `Evidence`, instructions come
exclusively from definitions. The two-block renderer is the only place that
decides what may enter the instruction part.

### 1.3 One digest type

`ContentDigest` (blake3) lives in `harw-types` and is used consistently for
fragment cache stability, tamper detection on security evidence, and plan
evidence references — one type, several call sites, no conversion.

### 1.4 Reference mode as a security mechanism

Freezing a sample ring yields evidence with a digest, not raw data. In
context it appears as a reference; reloading goes through capped
`context.load`. High-resolution evidence stays available without leaving
disk unasked, and a triage turn stays within a few thousand tokens, which
is what makes local inference feasible at all.

---

## 2. Merged rollout sequence

The phases below were the original build sequence for this combined work.
They are recorded here for the rationale they carry (why context primitives
had to land before security agents, for example), not as a live tracker.

| Phase | Theme | Contains |
|---|---|---|
| 0 | Vocabulary | `harw-observe`, `harw-context`, `harw-dod-signals`, IDs in `harw-types`, metrics/field macros, redaction, fragment conversion |
| 1 | Self-observation and assembly | trace context on stored jobs/leases, typestate assembly, context IR enforcement (`must_include` fail-closed), plan-loop metrics |
| 2 | Programs and observers | context programs as definitions, `ContextCeiling` in the resolver and handoff, sysfs/procfs/journald sensors, workspace drift sensor |
| 3 | Sets | `EgressSet` in `harw-sandbox`, `harw-dod-netpolicy` as a pure plan, source bindings (plan/memory/knowledge providers) |
| 4 | Boundary and rules | two-block rendering, trust classes, `harw-dod-rules` with contract rules, file-watch sensor with loginuid, baselines, evidence-ref digests |
| 5 | Enforcement and references | `harw-dod-warden-proto`, `harw-dod-warden`, `harw-dod-escalate`, freeze store, reference-mode context loading, history as a section |
| 6 | Agents | security agent family, triage specializations, program library, plan attachment |
| 7 | Operations | eBPF backends, systemd units, off-host mirroring, out-of-band alerting |

**The critical ordering:** context-IR enforcement had to land before
anything else, because as long as `must_include` could silently be dropped,
any security guarantee about context contents was worthless. And the
two-block boundary had to land before the triage agents, because a triage
agent without it would be an injection target.

Phases 0–6 are implemented in the current codebase (`harw-context`,
`harw-observe*`, the DoD sensor/rules/escalate/warden (`dod/crates/`)
crates). Phase 7 operational items (off-host mirroring, out-of-band
alerting) are Open — see `docs/design/harw-security-observability-plan.md`
for current operational status.

---

## 3. Dependency doctrine

### 3.1 The rules

**D1. Pure Rust only on the privileged path.** The warden and everything it
links contains no C code, no `*-sys` crate, no `bindgen`, no `pkg-config`. A
C library in the warden would be exactly the memory-safety hole the whole
system is built against. **Implemented** — verified: `dod/crates/harw-warden/Cargo.toml`
depends only on Rust crates (`rustix`, `landlock`, `sd-listen-fds`, `serde`,
`clap`, `tracing`) plus in-house crates; network enforcement shells out to
the `nft` binary as a subprocess rather than linking a C netfilter library
(see §3.3).

**D2. No `build.rs` that invokes a C compiler**, not in the warden tree, not
in its dependencies.

**D3. Syscall floor is `rustix` with `linux_raw`.** `libc` only where
`rustix` doesn't suffice, isolated in its own module. **Implemented** in
`harw-warden`, `harw-sentinel`, `harw-probe-fs`, `harw-probe-bpf`.

**D4. Every external dependency sits behind its own trait.** A sensor
backend, a network backend, a telemetry sink are swappable without a facade
type changing.

**D5. Pre-1.0 dependencies never appear in a public type.** They may live
behind an adapter, but no facade `pub fn` returns or accepts them directly.

**D6. Build it yourself when the syscall surface is small and the crate is
weak.** Rule of thumb: under roughly 500 lines of binding code and a
low-traffic or unmaintained crate means: build it on `rustix`.

**D7. The warden's dependency count is a number with a ceiling**, documented
and checked in CI. A enforcer that can be fully read should have a
dependency tree that can be fully read too. **Status: Open as an enforced
CI check** — `dod/crates/harw-warden/Cargo.toml` currently lists roughly a
dozen direct dependencies, consistent with the doctrine's intent, but no CI
step currently counts and gates the transitive total.

**D8. Supply-chain gates in CI.** `cargo deny` for advisories, sources,
licenses and bans as the baseline gate — **implemented**
(`deny.toml`, `.github/workflows/ci.yml` `cargo-deny` job). `cargo vet`,
scoped to the `dod/` subtree only — **Open**, not yet wired into CI (no
`vet` config or audit store in the repository as of this review). `cargo
auditable` for shipped binaries — **Open**, not found in the build/release
tooling.

**D9. The doctrine is itself a checked rule.** `harw-code-graph` reads
`Cargo.lock`, the workspace-drift sensor (`harw-dod-workspace`) produces
structure/dependency drift findings, and `harw-dod-rules` checks new
dependencies against a curated inventory. **Implemented** as a mechanism
(`harw-dod-workspace`, `harw-dod-rules`'s `StructureDriftRule`); the
curated-inventory content itself should be reviewed periodically as
dependencies change.

### 3.2 Evaluated choices

**Tier A, pure Rust, in use.**

| Purpose | Crate | Status |
|---|---|---|
| Syscalls | `rustix` (`linux_raw`) | Implemented, used throughout the privileged binaries |
| eBPF | `aya` | Implemented — `dod/crates/harw-dod-bpf` |
| Path confinement | `landlock` | Implemented — `harw-sentinel`, `harw-probe-fs`, `harw-warden` all apply a landlock rule |
| Netlink transport | `netlink-*` family | Implemented — `harw-dod-netlink`, `harw-dod-authlog` |
| procfs | in-house parsing | Implemented per-sensor (see `harw-dod-crate-decomposition.md` §4/§5), rather than an external `procfs` crate |
| Time, hash, serde | `jiff`, `blake3`, `serde` | Implemented, workspace-pinned |

**Tier B/candidates, reconsidered against the actual implementation.**

| Purpose | Original candidate | Current status |
|---|---|---|
| Netfilter | `rustables` (pure-Rust netlink) | **Not used.** `dod/crates/harw-warden/src/isolation.rs` instead shells out to the `nft` CLI binary as a child process, with a fixed argument grammar and no shell interpolation. This is a deliberate deviation from the original doctrine's D1 preference for pure Rust over an external binary; it avoids depending on a less-mature netlink-nftables crate at the cost of a subprocess dependency. Worth revisiting once/if the nftables Rust ecosystem matures. |
| seccomp | `seccompiler` | **Not implemented.** No seccomp filtering was found in the codebase; process confinement currently relies on landlock plus capability dropping, not syscall filtering. Open. |
| GPU | `nvml-wrapper` | Behind a feature/trait as planned; AMD/Intel go through sysfs with no extra dependency (`harw-dod-gpu`). |

**Tier D, deliberately excluded — still holds.**

`opentelemetry`/`opentelemetry-otlp` are not a core dependency; confirmed —
no OTel dependency appears anywhere in the workspace. The telemetry sink
trait (`harw-observe`) keeps any future OTel binding behind an optional
adapter crate (`harw-observe-otlp`), never in the core. The same applies to
Prometheus (`harw-observe-prom`).

Also excluded, as designed: any crate that would start a scanner process on
our behalf. Scanners run on a systemd timer with fixed arguments; this
system only reads their reports.

### 3.3 The warden as the hard case

The warden is where the doctrine applies most strictly. Confirmed in
`dod/crates/harw-warden/Cargo.toml`: `rustix` for syscalls, no async
runtime (no tokio — a blocking socket loop is sufficient), a
systemd-activated `SOCK_SEQPACKET` socket via `sd-listen-fds` (0 further
dependencies of its own), `landlock` for path confinement, and network
enforcement via the `nft` CLI rather than a linked netfilter library (see
§3.2 above — a deviation from the original all-Rust vision, kept narrow: a
fixed table/chain name, argument-list invocation, no shell). A
systemd-activated socket also means the warden isn't running at all until
something needs it — the smallest attack surface is a process that doesn't
exist.

---

## 4. Facade and boundary

`harw-dod` is the security subsystem's facade (see `harw-dod-charter.md`).
For integration, what matters most is what is **not** under it:

`harw-context` is **not** part of `harw-dod`. It is general infrastructure
every agent uses. Putting it under the security facade would make every
context change look like a security change and vice versa. `harw-observe`
sits beside the subsystem in dependency terms for the same reason —
telemetry is for everyone.

The boundary in one sentence: **`harw-dod` re-exports what concerns
defense; `harw-context` and `harw-observe` are infrastructure standing
beside it, not under it.**

---

## 5. Checks for this integration plan

- **Disjointness test.** A test loads the security ceiling and the scout
  ceiling and checks that the intersection of their `universe` sets is
  empty.
- **Shared fixtures.** Injection fixtures live in one directory and are
  read by both the context-rendering test suite and the security-triage
  test suite.
- **Golden renders for security programs.** A frozen render of the
  security-triage prompt is part of regression testing, so a prompt change
  is a visible diff, not a silent drift.
- **Doctrine gate (Open).** A CI step that counts the warden binary's
  transitive dependencies and fails above a ceiling, and a second step that
  checks the warden subtree contains no crate with a `links` attribute or a
  `build.rs` invoking a C compiler — neither is wired into CI yet; `cargo
  deny` covers advisories/licenses/bans but not this specific doctrine
  check.

---

## 6. Open items

1. **`nft`-subprocess vs. a pure-Rust netlink-nftables crate.** The current
   implementation shells out to `nft`. Revisit if the ecosystem's pure-Rust
   options mature, per D1's original preference.
2. **seccomp filtering.** Not implemented; process confinement currently
   relies on landlock and capability separation. Whether syscall filtering
   is added later, or the doctrine is revised to state landlock+caps as
   sufficient, is open.
3. **Ceiling on warden dependencies.** Not yet a CI-enforced number.
4. **`cargo vet` scope and `cargo auditable`.** Neither is wired into CI
   yet; both remain part of the doctrine's intent (D8) but are Open.
5. **Fallback without eBPF.** On kernels without `CAP_BPF` or without BTF,
   the sentinel must stay operational with fewer sensors. This is already
   expressible via the sensor typestate (an unbound handle simply never
   binds); it should be documented as a supported operating mode rather
   than left implicit.
