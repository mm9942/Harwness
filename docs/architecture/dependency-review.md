# Dependency review: supply-chain tiers

> **Status:** descriptive review, R12. Closes the "supply-chain tier document" item of
> ledger entry MIG-006 (`docs/planning/90-migration-ledger/MIGRATION_LEDGER.md`) and
> implements Job-Runtime-Doc §19/§20
> (`docs/planning/20-job-runtime/HARW_LINUX_JOB_RUNTIME_FOUNDATION.md`).
>
> **Derived from:** `Cargo.lock` (one lockfile since PL-60), the workspace members'
> `Cargo.toml` files, `[workspace.dependencies]` in the root `Cargo.toml`,
> `xtask/arch-policy.toml` (`[rules]`, `[sys_crates]`, `tcb = true` packages) and
> `deny.toml` (`[bans]`, `[sources]`).
>
> **Enforced source of truth:** `xtask/arch-policy.toml` (gate `arch`, run by
> `cargo run -q -p xtask -- gates`) and `deny.toml` (`cargo deny check`). This document
> explains and records decisions. It does not enforce anything by itself. If it disagrees
> with the policy files, the policy files win and this file is stale.

## Scope and rules

The review covers third-party crates that do one of these things:

- wrap unsafe Linux/Unix ABI behaviour (syscalls, file descriptors, Landlock, procfs,
  cgroups, systemd socket activation, BPF),
- carry a network or crypto trust boundary (HTTP, TLS, CryptGuard),
- build C, C++ or assembly code (`*-sys` crates, `cc`, `cmake`, `bindgen`).

Pure-Rust utility crates (`serde`, `jiff`, `tracing`, `bytes`, …) are out of scope. They
are pinned once in `[workspace.dependencies]` and checked by `cargo deny`.

First-party rule (Job-Runtime-Doc §19): every workspace crate inherits
`unsafe_code = "forbid"` from `[workspace.lints.rust]`, and every `harw-job-*` crate also
carries `#![forbid(unsafe_code)]` in `lib.rs`. All unsafe code in the job stack
therefore lives in the Tier A crates below, which is why they need this review.

Machine rules that back the tiers (from `xtask/arch-policy.toml`):

| Rule | Effect |
|---|---|
| `J.forbid_sys_crates = true` | No package in ring J may reach a `*-sys` crate in its transitive closure. |
| `[sys_crates] forbid_for_tcb = true` | The same holds for every `tcb = true` package: `harw-warden`, `harw-dod-warden`, `harw-dod-warden-proto`, `harw-dod-readfs`, `harw-dod-signals`. |
| `[[sys_crates.allow]]` | Only `linux-raw-sys` (pure Rust syscall constants for `rustix`), `windows-sys` (Windows targets only) and `js-sys` (wasm32 only) are exempt. |
| `[tcb."…"].allowed_internal` | TCB packages may only depend directly on the listed internal crates. |
| `deny.toml [sources]` | `unknown-git = "deny"` and `unknown-registry = "deny"`. Only the crates.io index is allowed (`allow-registry`). No git source is allowed. |

The closure is computed from `Cargo.lock` edges, which carry no platform or feature
filter. The closure is therefore an **over-approximation**. A crate reachable in the
lockfile closure may never be compiled on Linux (for example `crabgrind`, see Tier C).
The gate still counts it, so the conservative reading applies.

Ring codes as in `harw-workspace-inventory.md`: F foundation, I shared infrastructure,
C compiler, J job infrastructure, D DoD domain, A application/composition, T = the
`tcb = true` packages of ring D.

## Tier A: approved foundations

Approved for the listed rings without further review. A new use in a listed ring needs
no sign-off. A new use in an unlisted ring does.

| Crate | Locked version | Why | Where used today (direct dependents) | Rings that may use it |
|---|---|---|---|---|
| `rustix` | 1.1.5 (workspace pin `1.1.4`); 0.38.44 only transitively via `crossterm` 0.28 | Safe wrapper over raw Linux syscalls (pidfd, `waitid`, rlimits, `NO_NEW_PRIVS`, fanotify, sockets) without linking libc on Linux (`linux_raw` backend, `linux-raw-sys`). Removes the need for first-party `unsafe`. | `harw-job-linux`, `harw-job-exec`, `harw-job-darwin`, `harw-killer`, `harw-fsutil`, `harw-authority` (unix only, `geteuid`), `harw-tool-job`, `harw-cli`, `harw-web`, `harw-dod-bpf`, `harw-dod-config`, `harw-dod-netlink`, `harw-probe-bpf`, `harw-probe-fs`, `harw-sentinel`, `harw-warden` | I, J, D, T, A. Not F (F stays OS-free). |
| `linux-raw-sys` | 0.12.1 (0.4.15 via `rustix` 0.38) | Syscall ABI constants for `rustix`. Pure Rust, no C build. Allowlisted in `[[sys_crates.allow]]`. | only through `rustix` | wherever `rustix` is allowed |
| `tokio` | 1.53.1 | Async runtime. `harw-job-tokio` uses `AsyncFd` over pidfds for exit readiness and one supervisor event loop. | ~40 workspace crates, including `harw-job-runtime`, `harw-job-tokio`, the three hub daemons, `harw-node-transport`, `harw-infra-client`. | I, J, D, A. F and T only through `harw-types` → `tokio-util` (`CancellationToken`), a known split candidate (Eco §10). No direct `tokio` dependency in F or T. |
| `procfs` | 0.18.0 (`default-features = false`) | Typed `/proc` parsing for process recovery identity (start time, ppid, pgid) without hand-written parsers. Built on `rustix`. | `harw-job-linux` (feature `linux-basic`, default) | J, D, A. Must stay `default-features = false` (no `flate2`/`chrono` pulls). |
| `cap-std` | 3.4.6 | Capability-based filesystem access: all job-record I/O stays inside one directory handle, and cgroup v2 files are opened relative to a delegated cgroup directory. | `harw-job-store`, `harw-job-linux` (features `linux-sandbox`, `linux-cgroup-v2`) | I, J, D, A |
| `landlock` | 0.4.7 | Unprivileged filesystem/network sandboxing via Landlock LSM. Uses `libc` for the syscall, no C build. | `harw-job-linux` (feature `linux-sandbox`), `harw-warden`, `harw-sentinel`, `harw-probe-fs`, `harw-probe-bpf` | J, D, T, A |
| `fs4` | 1.1.0 | Advisory file locks (per-record locks in the job store, session locks). Built on `rustix`. | `harw-job-store`, `harw-netsec`, several I/A crates via `[workspace.dependencies]` | I, J, D, A |
| `hyper` / `hyper-util` | 1.11.1 / 0.1.20 | HTTP/1 server and client for the local AF_UNIX daemons and the node transport. Memory-safe HTTP parsing instead of a hand-rolled protocol. | `harw-auth-hub`, `harw-security-hub`, `harw-netsec`, `harw-infra-client`, `harw-node-transport`, `harw-web`, `harw-mcp-server`, `harw-observe-otlp`, `harw-agent-runner` (optional); also via `crypt_guard_hyper`, `reqwest`, `axum` | I, A. Not J, not T (no HTTP in the job stack or warden TCB). D only for a future read-only uplink, after review. |
| `http`, `http-body`, `http-body-util`, `bytes` | 1.4.2, 1.1.0, 0.1.5, 1.12.1 | Type vocabulary shared with `hyper`. | same crates as `hyper` | same as `hyper`; `bytes` also J (`harw-job-tokio` output buffers) |
| `tower` / `tower-service` | 0.5.3 / 0.3.3 | Service abstraction. `harw-node-transport` exposes a `tower_service::Service`. `crypt_guard_service` is built on `tower`. | `harw-node-transport` (direct `tower-service`); `tower` only transitively via `crypt_guard_service`, `reqwest`, `axum` | I, D (through `crypt_guard_service` in `harw-dod-encrypt`), A |
| `rustls` + `aws-lc-rs` | rustls 0.23.45, `aws-lc-rs` 1.18.1 (builds `aws-lc-sys` 0.45.0) | TLS 1.3 with the hybrid `X25519MLKEM768` key exchange for node channels. `aws-lc-rs` is the provider the workspace already locks (via `reqwest` 0.13 / `rustls` defaults), so no second crypto backend is added. **Note:** `aws-lc-sys` is a C build (`cc`, `cmake`, prebuilt NASM). Tier A applies to ring A only. | `harw-node-transport` (direct: `rustls`, `tokio-rustls` 0.26.4, `aws-lc-rs` with `prebuilt-nasm`); transitively `reqwest` (0.12 `rustls-tls` uses `ring`, 0.13 `rustls` uses `aws-lc-rs`), `thirtyfour`, `tokio-tungstenite` | A only. Forbidden in J and T by `forbid_sys_crates`. I and D need a review entry here first. |
| `crypt_guard`, `crypt_guard_service`, `crypt_guard_hyper` | 3.1.0 from crates.io (registry source, checksums in `Cargo.lock`) | First-party-maintained post-quantum crypto (ML-KEM / ML-DSA via `libcrux`, KMS service layer, Hyper binding). Published on crates.io since R15 (ledger MIG-015). The earlier git pin is gone. | `crypt_guard`: `harw-secrets`, `harw-infra-client`, `harw-auth-hub`. `crypt_guard_service`: `harw-auth-hub`, `harw-dod-encrypt`. `crypt_guard_hyper`: `harw-auth-hub`, `harw-infra-client`. | I, D (not T: the DoD crypto crate `harw-dod-encrypt` must stay unreachable from the facade, warden, probes and sensors, gate `edges`), A. Not J. See the `crabgrind` note in Tier C. |
| `blake3` | 1.8.7 | Content digests (`ContentDigest`, artifact hashes). **Note:** builds C/assembly SIMD kernels through `cc`. It is not a `*-sys` crate, so the arch gate does not flag it. It reaches every closure that includes `harw-types`/`harw-digest`, including J and T. | `harw-digest`, `harw-macros`, `harw-authority`, `harw-home`, `harw-agent-*`, `harw-egress`, `harw-dod-config`, `harw-dod-warden-proto`, and more | all rings (accepted C shim, single well-audited upstream). Switching to its `pure` feature for T is an open option, not a decision. |

## Tier B: restricted, review before enabling in a new crate

Allowed where listed. Any additional dependent needs a review entry in this table and,
for TCB packages, an update of `[tcb.*]` in `arch-policy.toml`.

| Crate | Locked version | Why restricted | Where used today | Rings that may use it |
|---|---|---|---|---|
| `sd-listen-fds` | 0.2.0 | systemd socket activation (`LISTEN_FDS`). Zero dependencies, but contains its own `unsafe` blocks (`OwnedFd::from_raw_fd`) and trusts the environment for fd numbers. Review the source on every version bump. | `harw-warden` (T), `harw-auth-hub`, `harw-security-hub`, `harw-netsec`, `harw-web` | A, and T only for `harw-warden` (existing, reviewed in `docs/design/harw-dod-integration-and-dependencies.md`). Not J. |
| `thirtyfour` | 0.37.5 | WebDriver client. Large closure: `reqwest` 0.13, `tokio-tungstenite`, `aws-lc-sys`, `ring`, and platform verifiers (`jni-sys`, `security-framework-sys`, `core-foundation-sys` on their targets). | `harw-browser-thirtyfour` only | A only (adapter crate behind `harw-browser`). Never I/J/D/T. |
| `reqwest` | 0.12.28 and 0.13.4 | General HTTP client with TLS. Two major versions in the tree (warned by `multiple-versions = "warn"`). 0.12 with `rustls-tls` pulls `ring` (C/asm build). | `harw-egress` (I), `harw-lens-embed`, `harw-mcp-client`, `harw-model-catalog`, `harw-oauth`, `harw-provider-http`, `harw-tool-doc`, `harw-tool-web`, `harw-channel-telegram-transport`, `harw-cli`, `harw-browser-thirtyfour` | I, A. Not J, D, T. Consolidating on 0.13 (aws-lc-rs only) is an open item. |
| `keyring` | 3.6.3 | OS keychain. On Linux `sync-secret-service` builds `libdbus-sys` (links libdbus via `pkg-config`) and pulls `zbus` → `nix` 0.29. | `harw-secrets` (feature `keyring`, optional), `harw-provider-http`, `harw-cli` | A; I only behind the optional `harw-secrets/keyring` feature. Never J, D, T. |
| `tikv-jemallocator` | 0.7.0 (builds `tikv-jemalloc-sys`) | Global allocator for the CLI binary. Compiles jemalloc from C. | `harw-cli` (optional) | A binaries only. |
| `nix` | 0.31.3 (direct), 0.29.0 (via `zbus`) | Broad libc wrapper. Prefer `rustix`. Kept only where `rustix` lacks the API. | `harw-probe-fs` (`fanotify` feature) | D/T probes only, as today. New uses should use `rustix`. |
| `aya` | 0.14.0 | Pure-Rust BPF loader (no `libbpf-sys`). Loads BPF objects built separately with clang (`dod/bpf`). Uses `libc` and privileged syscalls. | `harw-dod-bpf` | D only, under the `Bpf` privilege class of `gate_privileges.rs`. Never J. |
| `tokio-tungstenite` / `tungstenite` | 0.30.0 (`default-features = false`, `handshake`) | WebSocket framing for the session control plane (W00 D10). Parser of untrusted peer input; `handshake` pulls `sha1`, `base64`, `httparse`, `rand`. No TLS or `connect` features: TLS and identity come from the underlying transport (local UDS with `SO_PEERCRED`, or `harw-node-transport`). Message/frame caps are set explicitly (`WsLimits::tungstenite_config`). | `harw-session-ws` (direct, reviewed W00 R1); `harw-browser-thirtyfour` (transitive via `thirtyfour`) | A only. The arch gate enforces it: `[[forbidden_crates]]` keeps both names out of the F/I/C/J/D closures and every TCB package (ARC-02). |
| `oxidize-pdf` | 5.1.3 (`default-features = false`, `compression`) | PDF text extraction of untrusted input. Parser attack surface. | `harw-tool-doc` | A only. |
| `pathrs`, `libcgroups` | not in `Cargo.lock` | Listed by Job-Runtime-Doc §19 as candidates (`pathrs` Tier B, `libcgroups` Tier A with restricted features). Not adopted: `cap-std` covers path confinement and `harw-job-linux` drives cgroup v2 through files. | — | Review entry required before first use. |
| container runtime integration | — | Job-Runtime-Doc §19 Tier B. Not present. Bubblewrap stays an external executable (`harw-job-executor-bwrap`), no library binding. | — | Review entry required before first use. |

## Tier C: forbidden in J and TCB

Forbidden in every ring-J package and every `tcb = true` package, directly or
transitively. For crates that are absent today, the ban is enforced two ways: the
`*-sys` names fail the arch gate, and a new dependency needs a review entry here first.

| Crate / class | Locked version | Status | Enforcement |
|---|---|---|---|
| `libbpf-sys` | absent | Forbidden in the default graph. BPF goes through `aya` (Tier B, D only). | arch gate (`*-sys`) for J/T; review for others |
| `libseccomp` / `libseccomp-sys` | absent | Forbidden. Seccomp, if needed, must use a pure-Rust filter builder after review. | arch gate (`-sys`) for J/T; review for `libseccomp` |
| `openssl-sys` / `openssl` / `native-tls` | absent | Forbidden workspace-wide. TLS is `rustls` only. `reqwest` is used with `default-features = false`. | arch gate (`-sys`) for J/T; review for others. A `deny.toml` `[bans] deny` entry would make it hard everywhere (open item). |
| Any `*-sys` crate with a C build | present: `aws-lc-sys` 0.45.0, `libdbus-sys` 0.2.7, `tikv-jemalloc-sys` 0.7.1, `clang-sys` 1.9.1; platform-only: `core-foundation-sys`, `security-framework-sys`, `jni-sys`, `web-sys` | Allowed only in A (and I through `reqwest`/`keyring`, see Tier B). Never in J or T. | arch gate `forbid_sys_crates` / `forbid_for_tcb` over the lockfile closure |
| C/asm build helpers without `-sys` name: `ring` 0.17.14, `cc` 1.2.67, `cmake` 0.1.58, `bindgen` 0.72.1 | present | `ring` via `rustls`/`reqwest` 0.12/`quinn` (A/I only). `cmake` via `aws-lc-sys`. `cc` is reachable from J/T through `blake3` (accepted, see Tier A). These names are **not** caught by the arch gate. Reviews must check them by hand (`cargo tree -i ring`, `cargo tree -i cc`). | review only |
| `crabgrind` 0.2.6 (+ `bindgen`, `clang-sys`) | present in `Cargo.lock` | Pulled by `libcrux-secrets` only under `cfg(valgrind_ct_test)`, so it is never compiled in a normal build. It still appears in the lockfile closure of every `crypt_guard*` consumer (`harw-auth-hub`, `harw-infra-client`, `harw-dod-encrypt`). One more reason why `crypt_guard*` must not enter J or T. | lockfile closure (over-approximation) |
| Arbitrary first-party C shims | — | Forbidden in the whole workspace. No `build.rs` compiling C in first-party crates. | review |

## Current closure check (from `Cargo.lock`)

Native build crates reachable in the lockfile closure (excluding the allowlisted
`linux-raw-sys`, `windows-sys`, `js-sys`):

| Package (ring) | Native crates in closure | Verdict |
|---|---|---|
| all `harw-job-*`, `harw-job` (J) | `cc` (via `blake3` through `harw-types`/`harw-macros`) | OK: no `*-sys`, `cc` accepted through `blake3` |
| `harw-warden`, `harw-dod-warden`, `harw-dod-warden-proto`, `harw-dod-readfs`, `harw-dod-signals` (T) | `cc` (via `blake3`) | OK, same reason |
| `harw-netsec`, `harw-security-hub` (A) | `cc` (via `blake3`) | OK |
| `harw-auth-hub` (A), `harw-dod-encrypt` (D) | `cc`, `bindgen`, `clang-sys`, `crabgrind`, `pkg-config`, `core-foundation-sys` | OK for A/D. `crabgrind` is cfg-gated (see Tier C). |
| `harw-infra-client` (I) | as `harw-auth-hub`, plus `libdbus-sys`, `security-framework-sys` via `harw-secrets` → `keyring` (optional feature) | OK for I (no `forbid_sys_crates` on I) |
| `harw-node-transport` (A) | `aws-lc-sys`, `ring`, `cc`, `cmake`, `pkg-config` | OK for A only |

## Checks for the central build

The arch gate already covers the `*-sys` rule. These commands give the manual view from
Job-Runtime-Doc §20. Run them only in the central build, never from a subagent:

```text
cargo run -q -p xtask -- gates        # arch gate: rings, TCB allowlists, *-sys closure
cargo deny check                       # sources, licenses, bans, advisories
cargo tree -e features -p harw-job     # feature unification in the job stack
cargo tree -i libbpf-sys               # must print nothing
cargo tree -i libseccomp               # must print nothing
cargo tree -i openssl-sys              # must print nothing
cargo tree -i aws-lc-sys               # only A crates as roots
cargo tree -i ring                     # only A/I crates as roots
```

Feature unification can change the effective graph without a manifest change in the
affected crate. Do not assume an optional dependency stays optional.

## Open items

- Add `openssl-sys`, `openssl`, `native-tls`, `libbpf-sys`, `libseccomp`,
  `libseccomp-sys` to a `deny.toml` `[bans] deny` list so the ban holds outside J/T too.
- Extend the arch gate to treat `ring`, `cmake` and `bindgen` like `*-sys` names for J/T.
- Decide whether T packages should build `blake3` with its `pure` feature (no `cc`).
- Consolidate `reqwest` on 0.13 so `ring` leaves the tree and `aws-lc-rs` is the only
  TLS crypto provider.
- Add `cargo audit`/RustSec and a `cargo geiger` report to CI (Job-Runtime-Doc §19).
