# Crypto / infrastructure drift report (Masterplan H0)

> **Status:** H0 deliverable ("reconcile and freeze contracts"). Documentation only, **no functional change**.
> No existing DoD privilege boundary is changed by R11.
> Plan references: Crypto Masterplan v2 §1, §17, §22, §34 (H0), §39. CryptGuard service plan
> (`docs/planning/40-cryptguard-service/`).

## 1. Recorded versions

| Item | Value |
|---|---|
| Harwness planning baseline (`main`) | `d3e0b965696e7a631687a73618c934cda7ad3d2c` |
| R11 branch base (`claude/r11-harw-ecosystem`) | `339cbd14de0cc9e9bd2438e5f21f6362c03faffc` (adds the planning tree) |
| `harw-secrets` pin | `crypt_guard = "=3.0.2"` (`harw-secrets/Cargo.toml:16`). `Cargo.lock`: 3.0.2 from crates.io. |
| CryptGuard 3.1.0 | **Only on git**: `https://github.com/mm9942/crypt_guard`, branch `main`, head `2571edfc13e014cd60a6d87682b49c3805655f09` (2026-09-27). Not on crates.io. |
| 3.1.0 workspace crates | `crypt_guard` (root), `crypt_guard_core` (`crates/core`), `crypt_guard_service` (`crates/service`), `crypt_guard_hyper` (`crates/hyper`), `crypt_guard_proc`. **Underscore package names.** The plans write `crypt-guard-service` / `crypt-guard-hyper` in places; the real names use underscores. |
| libcrux on 3.1.0 | `crypt_guard_core` pins `libcrux-ml-kem = "=0.0.10"`. Its comment says it pulls `libcrux-sha3 >= 0.0.10` and `libcrux-secrets >= 0.0.6` and fixes the advisories below. |
| `harw-secrets` KATs | Re-run in the central R11 build (the `harw-secrets` tests). Not run by this docs agent (no builds in subagents). |

## 2. Stale `crypt_guard 3.0.1` references (§17)

| File:line | Stale statement | Correct statement |
|---|---|---|
| `harw-secrets/src/error.rs:46` | "`crypt_guard` 3.0.1 derives deterministic recipient keys only for hybrid KEMs" | Verified: 3.0.2 `derive_recipient_key_pair(kem, seed)` (`src/hpke_pq/mod.rs:2677`) also covers `MlKem512/768/1024`, after a SHAKE256 domain-separated seed expansion. `UnsupportedLegacyKem` is a **Harwness policy** (legacy pure ML-KEM records are read-only), not a library limit. |
| `harw-secrets/src/policy.rs:7` | module doc: 3.0.1 derives seeds "nur noch" for hybrid KEMs | same correction (hybrid-only claim) |
| `harw-secrets/src/policy.rs:76` | 3.0.1 "keine deterministische Seed-Ableitung mehr" for pure ML-KEM | same correction |
| `harw-secrets/src/envelope.rs:106` | legacy pure ML-KEM envelopes "unreadable with `crypt_guard` 3.0.1" | "rejected by `harw-secrets` policy" |
| `harw-secrets/src/envelope.rs:775` | test comment: envelope "mit crypt_guard 3.0.1 erzeugt" | historical fact, OK if it says "fixture produced with 3.0.1" |
| `harw-secrets/src/kek.rs:31` | cites `crypt_guard-3.0.1/src/hpke_pq/mod.rs:1698` | re-point to the 3.0.2 source line |
| `harw-secrets/src/kek.rs:318`, `:701` | "without this step crypt_guard 3.0.1 would expand the same root seed" | verify against 3.0.2, then update the version |
| `deny.toml:28`, `:32`–`:34` | "blockiert durch `crypt_guard = "=3.0.1"` (auch 3.0.2)" and reason strings "über crypt_guard =3.0.1" | pin is `=3.0.2`. The reason strings must name 3.0.2. |
| `docs/setup/build-prerequisites.md:33`–`36` | heading and text: "`crypt_guard` `3.0.1` (crates.io, hybrid KEM)" | 3.0.2 |

Rule (§17): update comments and docs only. Do **not** reinterpret persisted records. Keep the exact pin until the move is re-blessed.

## 3. `deny.toml` advisories waiting on the upgrade

| Advisory | Crate | Path | Cleared by |
|---|---|---|---|
| RUSTSEC-2026-0207 | `libcrux-sha3 0.0.9` | `crypt_guard =3.0.2` → `libcrux-ml-kem =0.0.9` | CryptGuard 3.1.0 (`libcrux-ml-kem =0.0.10`) |
| RUSTSEC-2026-0208 | `libcrux-sha3 0.0.9` | same | same |
| RUSTSEC-2026-0212 | `libcrux-secrets 0.0.5` | same | same |
| RUSTSEC-2026-0173 | `proc-macro-error2` (via `hax-lib-macros`, libcrux chain, compile time) | same chain | re-check after the upgrade |

## 4. Name clashes between plan, CryptGuard and Harwness

| Concept | Masterplan | CryptGuard 3.1.0 (`crypt_guard_service`) | Harwness today | Decision / action |
|---|---|---|---|---|
| Key generation counter | `KeyGeneration` (§12 `expected_generation`, §34 H1 "if not imported from CryptGuard service") | `KeyVersion(NonZeroU32)` (`crates/service/src/key.rs:70`) | `harw_secrets::id::KeyVersion(pub u32)` (`harw-secrets/src/id.rs:76`), persisted in `record.rs` | Import CryptGuard's `KeyVersion` in H1. Do not introduce `KeyGeneration`. `harw-secrets::KeyVersion` stays as the on-disk type, with an explicit conversion (0 is invalid in CG). |
| Principal | uses `harw_types::Principal` (§13) | `Principal(Box<str>)` (`crates/service/src/op.rs:21`) | `harw_types::Principal { kind, id, surface, tier }` (`harw-types/src/principal.rs:111`) | Never glob-import `crypt_guard_service::*` (the plan's `pub use crypt_guard_service::*` sketch collides). Alias it as `CgPrincipal` and map explicitly at the AuthHub client boundary. |

## 5. systemd drift: two sources of truth (§22)

| Aspect | `deploy/systemd/*` | `dod/packaging/systemd/*` |
|---|---|---|
| Files | `harw-sentinel.service`, `harw-probe-fs.service`, `harw-probe-bpf.service`, `harw-warden.service`, `harw-warden.socket` | `harw-dod-sentinel.service`, `harw-dod-bpf.service`, `harw-dod-warden.service`, `harw-dod-warden.socket`, `harw-dod.target` |
| Consumer | `harw-install/src/dod_units.rs`: `UNIT_CLASSES` names these files. `include_str!` of all five is **test-only** (`#[cfg(test)]`, lines 292–296). | `dod/Makefile` + `dod/scripts/install.sh` via `dod/packaging/manifest` (`@LIBEXECDIR@`/`@RUNTIMEDIR@` templating) |
| Runtime dir | `/run/harw` (`RuntimeDirectory=harw` on sentinel). Sockets `/run/harw/sentinel.sock` and `/run/harw/warden.sock`. | `@RUNTIMEDIR@` = `$(RUNSTATEDIR)/harw-dod` → `/run/harw-dod/{sentinel,warden}.sock` (matches §22.3) |
| Users | `harw-sentinel`, `harw-probe-fs`, `harw-probe-bpf`, `harw-warden`; group `harw-ipc` | `harw-dod`, `harw-dod-bpf`; groups `harw-dod-ipc`, `harw-dod-config` (`sysusers.d`) |
| Warden | `User=harw-warden` + `CAP_SYS_ADMIN` | **`User=root`** |
| BPF probe caps | `CAP_BPF` | `CAP_BPF CAP_PERFMON` |
| probe-fs | has a unit | **no unit** (the binary is still installed per `manifest`) |
| Ordering | independent, `WantedBy=multi-user.target` | `harw-dod.target`, `PartOf=`, bpf `Requires=` sentinel |
| Binary path | `/usr/local/bin/…` | `@LIBEXECDIR@/…` |

Consequence: `harw-install`'s unit tests check files that the DoD installer does not ship.
The DoD installer ships a warden running as root and a BPF probe with an extra capability,
and neither is covered by the `dod_units.rs` checks. **H0 changes nothing here.** The merge to one
canonical, embedded source (§22.1) is DoD-integration work (`50-dod-integration`, R12+).

## 6. `/run/harw` is already taken

| User | Path | Namespace |
|---|---|---|
| `harw-sandbox/src/bwrap.rs:62,65,68` | `/run/harw/netns-relay`, `/run/harw/egress.sock` (`SANDBOX_RUN_DIR = /run/harw`) | inside the bwrap sandbox (tmpfs root) |
| `harw-sandbox/src/tmux.rs:17` | `/run/harw/tmux.sock` | inside the sandbox |
| `harw-egress/src/{proxy,relay}.rs` (docs), `harw-browser-thirtyfour/src/launcher.rs:660` | `/run/harw/egress.sock`, `/run/harw/harw-netns-relay` | sandbox side |
| `harw-web` (docs/tests) | `/run/harw/web.sock` | host (illustrative) |
| `deploy/systemd/*`, `harw-sentinel`/probe CLIs | `/run/harw/sentinel.sock`, `/run/harw/warden.sock` | host (DoD) |
| Masterplan §22.3/§39 | `/run/harw/{control,network,secure}.sock` | host (infra) |

## 7. Decisions

| # | Decision | Deviation from plan |
|---|---|---|
| D1 | Split the runtime namespace: **`/run/harw/sandbox/`** for the fixed in-sandbox paths (relay, egress, tmux) and **`/run/harw/infra/`** for host infrastructure sockets (`control.sock`, `network.sock`, `secure.sock`, `security.sock`). DoD moves to `/run/harw-dod/` as in §22.3. | §22.3/§39 put infra sockets directly under `/run/harw/`. |
| D2 | **SecurityHub gets its own `security.sock`.** | §39 example points `[infrastructure.security]` at `control.sock`. |
| D3 | In **R12** the workspace moves from crates.io `crypt_guard =3.0.2` to **CryptGuard git** (`https://github.com/mm9942/crypt_guard`, `branch = "main"`). `Cargo.lock` pins the exact rev. Use the underscore package names (`crypt_guard`, `crypt_guard_core`, `crypt_guard_service`, `crypt_guard_hyper`). Re-bless the `harw-secrets` KATs in the same change, then drop the three libcrux `deny.toml` ignores and allow the git source in `deny.toml` `[sources]`. | Plans assume a released crate with hyphenated names. |
| D4 | `KeyVersion` (CryptGuard) is the canonical generation type. The plan's `KeyGeneration` is not introduced. | §34 H1 lists `KeyGeneration`. |
| D5 | CryptGuard `Principal` is never glob-imported. It is mapped to or from `harw_types::Principal` at one boundary. | Service plan sketch uses `pub use crypt_guard_service::*`. |
| D6 | H0 exit: no functional change. The stale-comment fixes (section 2) are allowed as doc-only edits and change no persisted format. | — |
