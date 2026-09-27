# Harw workspace inventory (Migration Phase 0/1)

> **Status:** descriptive snapshot, R11 (branch `claude/r11-harw-ecosystem`), derived from
> every `Cargo.toml` in the root workspace (`Cargo.toml` members) and the nested DoD
> workspace (`dod/Cargo.toml` members). Plan reference: Eco-Doc §43–§47, §67.
>
> **Enforced source of truth:** `xtask/arch-policy.toml` (checked by
> `cargo run -p xtask -- gates`). This document is descriptive only. If the two
> disagree, `arch-policy.toml` wins and this file is stale.

## Ring legend

| Code | Ring | May depend on (target rule, Eco §6/§56) |
|---|---|---|
| F | foundation | F |
| I | shared infrastructure | F, I |
| C | compiler domain | F, I, C |
| J | job infrastructure | F, I, J (no Harwness/DoD semantics, Eco §14) |
| D | DoD domain | F, I, D |
| A | Harwness application/composition (incl. Harwness domain crates: tools, channels, knowledge, plan) | anything |
| T | privileged TCB (flag, combined as `D+T`) | explicit allowlist (`gate_privileges.rs`) |

## Summary

| Ring | Count | Members |
|---|---|---|
| F | 5 | digest, macros, types, protocol, lens-types |
| I | 36 | authority, sandbox, fsutil, observe(+file/prom/otlp), extension-api, operations, egress, secrets, oauth, home, config, completions, killer, code-graph, explorer, context, tools, catalog, research, browser(+thirtyfour), lens-*, provider, model-catalog, mcp-client |
| C | 4 | agent-dsl, agent-artifact, agent-compiler, agent-runner |
| J | 1 (+1 in flight) | job-runtime; `harw-job-core` is being added in R11 W1 (not yet a member in the snapshot) |
| D / D+T | 22 / 10 | all `dod/crates/*` |
| A | 36 | core, runtime, cli, tui, `harw`, harwness-sdk, ops, web, tool-*, channel-*, knowledge, memory, plan, plan-bridge, core-bridge, session-store, mcp-server, registry-defaults, install, provider-http, … |

Total: 114 packages (81 root incl. `xtask`, 33 DoD).

## Column notes

- **WS:** workspace membership *before* the DoD merge (PL-60, see
  `dod-workspace-merge-plan.md`). After the merge every package is a root member. The column
  then only records origin (`dod/crates/*`).

- **Internal deps:** normal `[dependencies]` (incl. target-specific) on workspace packages,
  `harw-` prefix stripped. Dev/build deps are excluded (e.g. `harw-macros` has dev-dep cycles
  for trybuild tests; `harw-dod-rules` dev-depends on `harw-knowledge`).
- **Rev:** number of packages (both workspaces) with a normal dependency on this package.
  Cross-workspace path deps are counted (root crates already depend on
  `dod/crates/harw-dod-{signals,rules,escalate}`).
- **Privileged:** `xtask/src/gate_privileges.rs`. `CRATE_PRIVILEGE` class, or the
  `MONITORED_BINARIES` budget for the five monitored binaries. `—` = not listed (only
  allowed inside `harw-agent-runner`'s closure, which uses `DefaultUnprivileged`).
- **Platform:** heuristic from `cfg(...)` usage, target-specific deps and hard-coded
  `/proc`, `/sys`, bwrap or netlink use. "linux-only" = no useful behaviour elsewhere.
  "unix cfg" = compiles everywhere, with unix-only code paths.
- **Unsafe:** every package has `[lints] workspace = true`. Both workspaces set
  `unsafe_code = "forbid"`. "+ attr" = also `#![forbid(unsafe_code)]` in
  `lib.rs`/`main.rs`. No `allow(unsafe_code)` exists anywhere.
- **Native deps:** C/asm code that reaches the build, resolved through `Cargo.lock`/
  `dod/Cargo.lock`: `aws-lc-sys` and `ring` via `reqwest`/rustls, `libdbus-sys` via
  `keyring` (`sync-secret-service`), `tikv-jemalloc-sys`, and `cc` via `blake3`,
  `crypt_guard` and `oxidize-pdf`. `rustix`, `landlock`, `nix` and `aya` are pure Rust
  (raw syscalls), so they are not native. They make a crate platform-specific instead.

## Inventory

| Package | Path | WS | Ring | Internal deps (normal) | Rev | Privileged | Platform | Unsafe | Native deps |
|---|---|---|---|---|---|---|---|---|---|
| `harw-digest` | `harw-digest` | root | **F** | — | 2 | Unpriv (listed) | portable | forbid (ws) | blake3 (cc) |
| `harw-lens-types` | `harw-lens-types` | root | **F** | digest, macros | 20 | — | portable | forbid (ws + attr) | — |
| `harw-macros` | `harw-macros` | root | **F** | — | 69 | Unpriv (listed) | portable | forbid (ws) | blake3 (cc) |
| `harw-protocol` | `harw-protocol` | root | **F** | types | 6 | — | portable | forbid (ws + attr) | — |
| `harw-types` | `harw-types` | root | **F** | digest | 71 | Unpriv (listed) | portable | forbid (ws + attr) | — |
| `harw-authority` | `harw-authority` | root | **I** | types | 37 | Unpriv (listed) | unix cfg | forbid (ws + attr) | blake3 (cc) |
| `harw-browser` | `harw-browser` | root | **I** | — | 3 | — | portable | forbid (ws) | — |
| `harw-browser-thirtyfour` | `harw-browser-thirtyfour` | root | **I** | browser | 1 | — | unix cfg | forbid (ws) | aws-lc-sys via thirtyfour/reqwest |
| `harw-catalog` | `harw-catalog` | root | **I** | config, home | 6 | — | portable | forbid (ws + attr) | — |
| `harw-code-graph` | `harw-code-graph` | root | **I** | macros, types | 6 | Unpriv (listed) | portable | forbid (ws + attr) | — |
| `harw-completions` | `harw-completions` | root | **I** | macros | 5 | Unpriv (listed) | portable | forbid (ws) | — |
| `harw-config` | `harw-config` | root | **I** | agent-dsl, fsutil, macros, types | 13 | — | unix cfg | forbid (ws + attr) | — |
| `harw-context` | `harw-context` | root | **I** | authority, lens-types, macros, types | 12 | — | portable | forbid (ws) | — |
| `harw-egress` | `harw-egress` | root | **I** | authority, sandbox | 3 | — | linux (netns relay, bwrap) | forbid (ws + attr) | aws-lc-sys via reqwest; blake3 (cc) |
| `harw-explorer` | `harw-explorer` | root | **I** | — | 4 | — | unix cfg | forbid (ws) | — |
| `harw-extension-api` | `harw-extension-api` | root | **I** | authority, catalog, context, lens-types, macros, sandbox, tools, types | 26 | — | portable | forbid (ws + attr) | — |
| `harw-fsutil` | `harw-fsutil` | root | **I** | — | 12 | Unpriv (listed) | portable (linux fast path) | forbid (ws + attr) | — |
| `harw-home` | `harw-home` | root | **I** | fsutil | 19 | Unpriv (listed) | unix cfg | forbid (ws + attr) | blake3 (cc) |
| `harw-killer` | `harw-killer` | root | **I** | — | 2 | — | linux (pidfd,/proc; non-linux stub) | forbid (ws + attr) | — |
| `harw-lens` | `harw-lens` | root | **I** | lens-embed, lens-query, lens-source, lens-types, macros | 2 | — | portable | forbid (ws) | — |
| `harw-lens-chunk` | `harw-lens-chunk` | root | **I** | lens-types, macros, types | 1 | — | portable | forbid (ws + attr) | — |
| `harw-lens-embed` | `harw-lens-embed` | root | **I** | lens-types, macros, types | 4 | — | portable | forbid (ws + attr) | aws-lc-sys via reqwest; blake3 (cc) |
| `harw-lens-federation` | `harw-lens-federation` | root | **I** | lens-embed, lens-index, lens-query, lens-rank, lens-types, macros | 1 | — | portable | forbid (ws) | — |
| `harw-lens-index` | `harw-lens-index` | root | **I** | lens-rank, lens-store, lens-types, macros, types | 3 | — | portable | forbid (ws) | — |
| `harw-lens-query` | `harw-lens-query` | root | **I** | home, lens-embed, lens-index, lens-rank, lens-store, lens-types, macros | 2 | — | portable | forbid (ws) | — |
| `harw-lens-rank` | `harw-lens-rank` | root | **I** | lens-types | 3 | — | portable | forbid (ws) | — |
| `harw-lens-source` | `harw-lens-source` | root | **I** | home, knowledge, lens-chunk, lens-embed, lens-index, lens-store, lens-types, macros, observe, types | 1 | — | unix cfg | forbid (ws) | — |
| `harw-lens-store` | `harw-lens-store` | root | **I** | home, lens-types, macros, types | 3 | — | portable | forbid (ws) | — |
| `harw-mcp-client` | `harw-mcp-client` | root | **I** | extension-api, tools | 2 | — | portable | forbid (ws) | aws-lc-sys via reqwest |
| `harw-model-catalog` | `harw-model-catalog` | root | **I** | config, context, lens-types, types | 6 | — | unix cfg | forbid (ws + attr) | aws-lc-sys via reqwest |
| `harw-oauth` | `harw-oauth` | root | **I** | config | 2 | — | unix cfg | forbid (ws + attr) | aws-lc-sys via reqwest |
| `harw-observe` | `harw-observe` | root | **I** | authority, macros | 16 | Unpriv (listed) | portable | forbid (ws) | — |
| `harw-observe-file` | `harw-observe-file` | root | **I** | macros, observe | 2 | Unpriv (listed) | portable | forbid (ws) | blake3 (cc) |
| `harw-observe-otlp` | `harw-observe-otlp` | root | **I** | macros, observe | 1 | Unpriv (listed) | portable | forbid (ws) | — |
| `harw-observe-prom` | `harw-observe-prom` | root | **I** | macros, observe | 1 | Unpriv (listed) | portable | forbid (ws) | — |
| `harw-operations` | `harw-operations` | root | **I** | authority, extension-api, sandbox, tools, types | 8 | — | portable | forbid (ws + attr) | — |
| `harw-provider` | `harw-provider` | root | **I** | macros, types | 3 | — | portable | forbid (ws + attr) | — |
| `harw-research` | `harw-research` | root | **I** | macros, types | 5 | Unpriv (listed) | portable | forbid (ws + attr) | — |
| `harw-sandbox` | `harw-sandbox` | root | **I** | authority, types | 24 | Unpriv (listed) | linux (bwrap; unix cfg) | forbid (ws + attr) | — |
| `harw-secrets` | `harw-secrets` | root | **I** | fsutil, macros, observe | 4 | — | unix cfg | forbid (ws) | cc via crypt_guard; libdbus-sys via keyring (feature `keyring`) |
| `harw-tools` | `harw-tools` | root | **I** | authority, context, lens-types, macros, sandbox, types | 20 | — | portable | forbid (ws + attr) | — |
| `harw-agent-artifact` | `harw-agent-artifact` | root | **C** | — | 5 | — | unix cfg | forbid (ws + attr) | blake3 (cc) |
| `harw-agent-compiler` | `harw-agent-compiler` | root | **C** | agent-artifact, agent-dsl, catalog, home, registry-defaults | 2 | — | unix cfg | forbid (ws + attr) | — |
| `harw-agent-dsl` | `harw-agent-dsl` | root | **C** | context | 13 | — | portable | forbid (ws + attr) | blake3 (cc) |
| `harw-agent-runner` | `harw-agent-runner` | root | **C** | agent-artifact, agent-dsl, core, home, registry-defaults, runtime, tool-job, tui, types, harwness-sdk | 0 | bin budget Unprivileged (unlisted=default-unpriv) | unix (rustix) | forbid (ws + attr) | — |
| `harw-job-runtime` | `harw-job-runtime` | root | **J** | macros, observe, types | 8 | — | portable | forbid (ws + attr) | — |
| `harw-dod` | `dod/crates/harw-dod` | dod | **D** | authority, dod-authlog, dod-blockio, dod-bpf, dod-cap, dod-cpu, dod-flow, dod-fsmon, dod-gpu, dod-listener, dod-memory, dod-netcounters, dod-procmon, dod-rules, dod-scanreport, dod-sentinel, dod-signals, dod-thermal, dod-workspace | 0 | — | linux (facade over sensors) | forbid (ws) | — |
| `harw-dod-authlog` | `dod/crates/harw-dod-authlog` | dod | **D+T** | dod-cap, dod-netlink, dod-signals, macros, types | 1 | Netlink | linux-only (netlink) | forbid (ws) | — |
| `harw-dod-blockio` | `dod/crates/harw-dod-blockio` | dod | **D** | dod-cap, dod-readfs, dod-signals, macros, types | 2 | Unpriv (listed) | linux-only (procfs/sysfs) | forbid (ws) | — |
| `harw-dod-bpf` | `dod/crates/harw-dod-bpf` | dod | **D+T** | dod-cap, macros, types | 4 | Bpf | linux-only (aya) | forbid (ws) | none in cargo; loads C BPF objects built by clang (dod/bpf) |
| `harw-dod-cap` | `dod/crates/harw-dod-cap` | dod | **D** | authority, macros, types | 25 | Unpriv (listed) | unix cfg | forbid (ws) | — |
| `harw-dod-cgroup` | `dod/crates/harw-dod-cgroup` | dod | **D** | dod-cap, dod-readfs, dod-signals, macros, types | 1 | Unpriv (listed) | linux-only (cgroup v2) | forbid (ws) | — |
| `harw-dod-config` | `dod/crates/harw-dod-config` | dod | **D** | — | 2 | Unpriv (listed) | unix (rustix) | forbid (ws + attr) | blake3 (cc) |
| `harw-dod-cpu` | `dod/crates/harw-dod-cpu` | dod | **D** | dod-cap, dod-readfs, dod-signals, macros, types | 2 | Unpriv (listed) | linux-only (procfs) | forbid (ws) | — |
| `harw-dod-escalate` | `dod/crates/harw-dod-escalate` | dod | **D** | authority, dod-rules, dod-signals, dod-warden-proto, macros, session-store, types | 1 | Unpriv (listed) | portable | forbid (ws + attr) | — |
| `harw-dod-fixtures` | `dod/crates/harw-dod-fixtures` | dod | **D** | dod-cap, dod-readfs, dod-signals, macros, types | 0 | — | linux (fixture fs trees) | forbid (ws) | — |
| `harw-dod-flow` | `dod/crates/harw-dod-flow` | dod | **D+T** | authority, dod-bpf, dod-cap, dod-signals, macros, sandbox, types | 2 | Bpf | linux-only (bpf) | forbid (ws) | — |
| `harw-dod-fsmon` | `dod/crates/harw-dod-fsmon` | dod | **D+T** | dod-cap, dod-readfs, dod-signals, macros, types | 2 | FileWatch | linux-only (fanotify) | forbid (ws) | — |
| `harw-dod-gpu` | `dod/crates/harw-dod-gpu` | dod | **D** | dod-cap, dod-readfs, dod-signals, macros, types | 2 | Unpriv (listed) | linux-only (sysfs) | forbid (ws) | — |
| `harw-dod-listener` | `dod/crates/harw-dod-listener` | dod | **D** | dod-cap, dod-readfs, dod-signals, macros, types | 2 | Unpriv (listed) | linux-only (procfs; unix cfg) | forbid (ws) | — |
| `harw-dod-memory` | `dod/crates/harw-dod-memory` | dod | **D** | dod-cap, dod-readfs, dod-signals, macros, types | 2 | Unpriv (listed) | linux-only (procfs) | forbid (ws) | — |
| `harw-dod-netcounters` | `dod/crates/harw-dod-netcounters` | dod | **D** | dod-cap, dod-readfs, dod-signals, macros, types | 2 | Unpriv (listed) | linux-only (procfs) | forbid (ws) | — |
| `harw-dod-netlink` | `dod/crates/harw-dod-netlink` | dod | **D** | dod-cap, macros | 1 | Unpriv (listed) | linux-only | forbid (ws) | — |
| `harw-dod-netpolicy` | `dod/crates/harw-dod-netpolicy` | dod | **D** | authority, dod-cap, macros, sandbox | 0 | Unpriv (listed) | portable | forbid (ws + attr) | — |
| `harw-dod-procmon` | `dod/crates/harw-dod-procmon` | dod | **D+T** | dod-bpf, dod-cap, dod-signals, macros, types | 2 | Bpf | linux-only (bpf) | forbid (ws) | — |
| `harw-dod-readfs` | `dod/crates/harw-dod-readfs` | dod | **D** | dod-cap, macros | 11 | Unpriv (listed) | linux (procfs/sysfs reader; unix cfg) | forbid (ws) | — |
| `harw-dod-rules` | `dod/crates/harw-dod-rules` | dod | **D** | authority, code-graph, dod-signals, research, types | 5 | Unpriv (listed) | portable | forbid (ws + attr) | — |
| `harw-dod-scanreport` | `dod/crates/harw-dod-scanreport` | dod | **D** | dod-cap, dod-readfs, dod-signals, home, macros, types | 2 | Unpriv (listed) | portable | forbid (ws) | — |
| `harw-dod-sentinel` | `dod/crates/harw-dod-sentinel` | dod | **D** | authority, dod-cap, dod-rules, dod-signals, fsutil, macros, observe, types | 2 | Unpriv (listed) | linux (procfs) | forbid (ws) | — |
| `harw-dod-signals` | `dod/crates/harw-dod-signals` | dod | **D** | dod-cap, macros, observe, types | 25 | Unpriv (listed) | portable | forbid (ws) | — |
| `harw-dod-thermal` | `dod/crates/harw-dod-thermal` | dod | **D** | dod-cap, dod-readfs, dod-signals, macros, types | 2 | Unpriv (listed) | linux-only (sysfs) | forbid (ws) | — |
| `harw-dod-warden` | `dod/crates/harw-dod-warden` | dod | **D+T** | dod-warden-proto, macros, types | 1 | Unpriv (listed) | linux-only (cgroup v2) | forbid (ws + attr) | — |
| `harw-dod-warden-proto` | `dod/crates/harw-dod-warden-proto` | dod | **D+T** | macros, types | 3 | Unpriv (listed) | portable | forbid (ws) | blake3 (cc) |
| `harw-dod-workspace` | `dod/crates/harw-dod-workspace` | dod | **D** | code-graph, dod-cap, dod-signals, macros, types | 2 | Unpriv (listed) | portable | forbid (ws + attr) | — |
| `harw-probe-bpf` | `dod/crates/harw-probe-bpf` | dod | **D+T** | authority, completions, dod-bpf, dod-cap, dod-config, dod-flow, dod-procmon, dod-signals, sandbox, types | 0 | bin budget CapBpf | linux-only (aya, landlock) | forbid (ws + attr) | — |
| `harw-probe-fs` | `dod/crates/harw-probe-fs` | dod | **D+T** | completions, dod-cap, dod-fsmon, dod-signals, types | 0 | bin budget CapSysAdmin | linux-only (fanotify, landlock) | forbid (ws + attr) | — |
| `harw-sentinel` | `dod/crates/harw-sentinel` | dod | **D** | authority, completions, dod-blockio, dod-cap, dod-cgroup, dod-config, dod-cpu, dod-gpu, dod-listener, dod-memory, dod-netcounters, dod-rules, dod-scanreport, dod-sentinel, dod-signals, dod-thermal, dod-workspace, home, macros, observe, observe-file, sandbox, types | 0 | bin budget Unprivileged | linux (landlock; non-linux stub) | forbid (ws + attr) | — |
| `harw-warden` | `dod/crates/harw-warden` | dod | **D+T** | completions, dod-warden, dod-warden-proto, macros, types | 0 | bin budget SystemdSocketNoNet | linux (landlock, socket act.; non-linux stub) | forbid (ws + attr) | — |
| `harw` | `harw` | root | **A** | agent-dsl, authority, code-graph, core, extension-api, model-catalog, operations, ops, plan, plan-bridge, provider, registry-defaults, research, sandbox, types | 0 | — | portable | forbid (ws) | — |
| `harw-channel` | `harw-channel` | root | **A** | macros, secrets, session-store, types | 3 | — | portable | forbid (ws + attr) | — |
| `harw-channel-telegram` | `harw-channel-telegram` | root | **A** | authority, channel, sandbox, session-store, types | 2 | — | unix cfg | forbid (ws + attr) | — |
| `harw-channel-telegram-transport` | `harw-channel-telegram-transport` | root | **A** | channel, channel-telegram, config, secrets, types | 1 | — | portable | forbid (ws + attr) | aws-lc-sys via reqwest |
| `harw-cli` | `harw-cli` | root | **A** | agent-artifact, agent-compiler, agent-dsl, authority, channel, channel-telegram, channel-telegram-transport, completions, config, context, core, extension-api, home, install, job-runtime, killer, knowledge, lens, lens-types, mcp-client, mcp-server, memory, model-catalog, oauth, observe, observe-file, observe-otlp, observe-prom, operations, ops, plan, plan-bridge, protocol, provider-http, registry-defaults, runtime, sandbox, secrets, session-store, tool-doc, tool-lens, tui, types, web | 0 | — | portable (unix/windows/linux cfg) | forbid (ws + attr) | tikv-jemalloc-sys (cc); aws-lc-sys via reqwest; libdbus-sys via keyring |
| `harw-core` | `harw-core` | root | **A** | agent-dsl, authority, catalog, config, context, extension-api, instructions, job-runtime, lens-types, macros, observe, protocol, sandbox, session-store, tools, types | 10 | — | portable | forbid (ws + attr) | — |
| `harw-core-bridge` | `harw-core-bridge` | root | **A** | agent-dsl, authority, core, dod-signals, extension-api, operations, research, sandbox, types | 2 | — | portable | forbid (ws) | — |
| `harw-install` | `harw-install` | root | **A** | config, home | 1 | — | portable (systemd/launchd/schtasks backends) | forbid (ws + attr) | — |
| `harw-instructions` | `harw-instructions` | root | **A** | extension-api, macros, types | 2 | — | portable | forbid (ws + attr) | — |
| `harw-knowledge` | `harw-knowledge` | root | **A** | agent-dsl, context, dod-rules, dod-signals, job-runtime, lens-types, macros, model-catalog, observe, plan, session-store, types | 6 | — | unix cfg | forbid (ws + attr) | — |
| `harw-matrix-game` | `harw-matrix-game` | root | **A** | — | 1 | — | portable | forbid (ws) | — |
| `harw-mcp-server` | `harw-mcp-server` | root | **A** | job-runtime, session-store, types | 1 | — | portable | forbid (ws + attr) | — |
| `harw-memory` | `harw-memory` | root | **A** | context, extension-api, fsutil, lens-types, types | 4 | — | unix cfg | forbid (ws) | — |
| `harw-ops` | `harw-ops` | root | **A** | agent-compiler, agent-dsl, authority, code-graph, config, core, core-bridge, explorer, extension-api, home, job-runtime, knowledge, macros, matrix-game, memory, model-catalog, operations, plan, plan-bridge, provider, provider-http, registry-defaults, research, sandbox, session-store, tool-job, tool-plan, tool-shell, tools, types, web | 4 | — | unix cfg | forbid (ws + attr) | — |
| `harw-plan` | `harw-plan` | root | **A** | macros, types | 7 | — | portable | forbid (ws + attr) | — |
| `harw-plan-bridge` | `harw-plan-bridge` | root | **A** | agent-dsl, authority, context, core, dod-escalate, dod-signals, extension-api, home, job-runtime, knowledge, lens-types, macros, observe, operations, plan, research, session-store, types | 4 | — | portable | forbid (ws + attr) | — |
| `harw-project-discovery` | `harw-project-discovery` | root | **A** | extension-api, macros, types | 2 | — | portable (linux cfg) | forbid (ws + attr) | — |
| `harw-provider-http` | `harw-provider-http` | root | **A** | config, core, fsutil, oauth, protocol, provider, sandbox, tools, types | 4 | — | portable | forbid (ws + attr) | aws-lc-sys via reqwest; libdbus-sys via keyring |
| `harw-registry-defaults` | `harw-registry-defaults` | root | **A** | agent-dsl, authority, browser, browser-thirtyfour, catalog, config, egress, extension-api, home, instructions, knowledge, project-discovery, sandbox, tool-browser, tool-deps, tool-doc, tool-explorer, tool-fs, tool-job, tool-lens, tool-plan, tool-process, tool-shell, tool-web, tools | 7 | — | unix cfg | forbid (ws + attr) | — |
| `harw-runtime` | `harw-runtime` | root | **A** | agent-artifact, agent-dsl, authority, catalog, config, context, core, core-bridge, explorer, extension-api, fsutil, home, job-runtime, knowledge, lens-types, macros, mcp-client, memory, model-catalog, observe, operations, ops, plan, plan-bridge, project-discovery, protocol, provider-http, registry-defaults, sandbox, secrets, session-store, tool-job, tool-plan, tool-shell, types | 4 | — | unix cfg | forbid (ws + attr) | — |
| `harw-session-store` | `harw-session-store` | root | **A** | fsutil, job-runtime, macros, observe, types | 13 | — | unix cfg (perms/locks) | forbid (ws + attr) | — |
| `harw-tool-browser` | `harw-tool-browser` | root | **A** | authority, browser, extension-api, sandbox, tools | 1 | — | portable | forbid (ws) | — |
| `harw-tool-deps` | `harw-tool-deps` | root | **A** | authority, code-graph, extension-api, macros, sandbox, tools | 1 | — | unix cfg | forbid (ws + attr) | — |
| `harw-tool-doc` | `harw-tool-doc` | root | **A** | authority, egress, extension-api, fsutil, macros, tools | 3 | — | portable | forbid (ws + attr) | aws-lc-sys via reqwest; cc via oxidize-pdf |
| `harw-tool-explorer` | `harw-tool-explorer` | root | **A** | authority, explorer, extension-api, macros, sandbox, tools | 1 | — | portable | forbid (ws) | — |
| `harw-tool-fs` | `harw-tool-fs` | root | **A** | authority, extension-api, fsutil, home, macros, sandbox, tools, types | 1 | — | unix cfg | forbid (ws + attr) | — |
| `harw-tool-job` | `harw-tool-job` | root | **A (J split)** | authority, extension-api, tool-shell, tools, types | 5 | — | linux-first (/proc stat, rustix pgrp; unix compile) | forbid (ws + attr) | — |
| `harw-tool-lens` | `harw-tool-lens` | root | **A** | authority, extension-api, home, lens, lens-federation, macros, tools | 2 | — | portable | forbid (ws + attr) | — |
| `harw-tool-plan` | `harw-tool-plan` | root | **A** | authority, extension-api, home, tools, types | 4 | — | unix cfg | forbid (ws + attr) | — |
| `harw-tool-process` | `harw-tool-process` | root | **A** | authority, extension-api, killer, macros, tools | 1 | — | linux (target dep) | forbid (ws) | — |
| `harw-tool-shell` | `harw-tool-shell` | root | **A** | authority, extension-api, macros, observe, sandbox, tools, types | 5 | — | portable | forbid (ws + attr) | blake3 (cc) |
| `harw-tool-web` | `harw-tool-web` | root | **A** | authority, egress, extension-api, fsutil, macros, sandbox, tool-doc, tools | 1 | — | portable | forbid (ws + attr) | aws-lc-sys via reqwest; blake3 (cc) |
| `harw-tui` | `harw-tui` | root | **A** | agent-artifact, agent-dsl, authority, catalog, config, context, core, explorer, extension-api, fsutil, home, lens-types, memory, model-catalog, operations, ops, plan, protocol, registry-defaults, runtime, sandbox, session-store, tool-job, tool-plan, tool-shell, tools, types | 2 | — | unix cfg | forbid (ws + attr) | — |
| `harw-web` | `harw-web` | root | **A** | context, macros, operations, session-store, types | 2 | — | unix (UDS, SO_PEERCRED) | forbid (ws + attr) | — |
| `harwness-sdk` | `harwness-sdk` | root | **A** | config, core, extension-api, home, protocol, provider-http, runtime, session-store, tools, types | 1 | — | portable | forbid (ws + attr) | — |
| `xtask` | `xtask` | root | **A (tooling)** | code-graph | 0 | — | unix cfg | forbid (ws) | — |
Counter({'I': 36, 'A': 36, 'D': 22, 'D+T': 10, 'F': 5, 'C': 4, 'J': 1})

## Classification notes

| Package | Note |
|---|---|
| `harw-types` | Classified F, but it carries `tokio-util` (`CancelToken`) and Harwness identity vocabulary (`TenantId`, `WorkspaceId`, `ApprovalActor`, `Principal`). Split candidate (Eco §10): value types vs runtime/Harwness types. |
| `harw-protocol` | F (wire envelope, no runtime deps). Only depends on `harw-types`. |
| `harw-lens-types` | F: pure retrieval vocabulary (digest + macros). |
| `harw-observe` | I. `TraceContext` is foundation-grade vocabulary (Eco §32/§58). The declared dependency on `harw-authority` is unused in code (only a doc comment in `routing.rs`), see inversions doc. |
| `harw-context`, `harw-tools`, `harw-research` | I as shared vocabularies. They carry Harwness-agent semantics, so review before any F promotion. |
| `harw-lens-source` | I, but depends on `harw-knowledge` (A). Recorded as an inversion. |
| `harw-agent-runner` | C per Eco §43, but it is really a composition binary (depends on core/runtime/tui/registry-defaults/tool-job/sdk). See inversions doc. |
| `harw-tool-job` | A/J split: tool schema, ownership and notifier stay A. Process supervision (procfs, pgrp kill, `meta.json`) moves to J crates. See `job-extraction-map.md`. |
| `harw-session-store` | A (session persistence). `job_store.rs` is J material and moves to `harw-job-store` (Eco §33). |
| `harw-dod-warden`, `harw-dod-warden-proto`, `harw-warden` | D+T: warden TCB (Eco §22). |
| `harw-probe-fs`, `harw-probe-bpf` | D+T: privileged binaries (CapSysAdmin / CapBpf budgets). |
| `harw-dod-{bpf,procmon,flow,authlog,fsmon}` | D+T: elevated `CRATE_PRIVILEGE` class (Bpf / Netlink / FileWatch). |
| `harw-sentinel` | D (not T): monitored binary with an Unprivileged budget. |
| `xtask` | A (tooling). Depends only on `harw-code-graph`. |

## Divergence from `xtask/arch-policy.toml` (as written in parallel in R11)

`arch-policy.toml` is authoritative. Where this descriptive ring differs, read the policy value:

| Package(s) | This doc | arch-policy | Comment |
|---|---|---|---|
| `harw-session-store` | A | I | Policy treats it as infrastructure with the `→ harw-job-runtime` edge as a W2 exception. Consequence: R4 (`harw-dod-escalate` → session-store) is D→I, which is legal in the policy. |
| `harw-agent-runner` | C | A | Policy accepts the runner as composition, so R2/R3 are not exceptions there. The split-crate proposal still stands. |
| `harw-lens*`, `harw-browser*`, `harw-explorer`, `harw-mcp-client`, `harw-model-catalog`, `harw-oauth`, `harw-provider` | I | A | Policy is stricter here. Consequence: R6 (`harw-lens-source` → `harw-knowledge`) is A→A, so it is legal in the policy. |
| T flag | privileged binaries + elevated `CRATE_PRIVILEGE` crates (probes, bpf/procmon/flow/authlog/fsmon, warden family) | `[tcb.*]` allowlists for the warden family + `harw-dod-readfs` + `harw-dod-signals` | Different meaning: the policy's T = "has a dependency allowlist", this doc's T = "holds privilege". Both views are needed. The privilege view stays enforced by `gate_privileges.rs`. |
| `harw-job-*` (planned crates) | not listed (not yet members) | J | Pre-declared in the policy. |
