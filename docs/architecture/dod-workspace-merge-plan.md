# DoD workspace merge plan (PL-60)

> Status: implemented in the working tree (PL-60); lockfile regeneration and
> TCB verification are done by the central build (see §7). Last reviewed:
> 2026-09-27

**Purpose of this document:** record what the merge of the nested DoD Cargo
workspace (`dod/`) into the root Cargo workspace changed, what it deliberately
did not change, which dependency resolutions move, and which Trusted
Computing Base (TCB) invariants the central build must re-verify before the
merge counts as done.

**Related:** `docs/planning/10-ecosystem-workspace/HARW_ECOSYSTEM_WORKSPACE_ARCHITECTURE.md`
§19–§23 and §49–§51, `docs/planning/50-dod-integration/README.md`,
`xtask/src/gates.rs`, `xtask/src/gate_warden.rs`, `dod/Makefile`.

The rule behind the merge (architecture §21): *same lockfile does not mean
same TCB*. Workspace membership is build governance, not dependency
permission. The Warden's dependency boundary is enforced by the package
graph and the `xtask` gates, not by a second `Cargo.lock`.

---

## 1. Before / after

| Aspect | Before PL-60 | After PL-60 |
|---|---|---|
| Cargo workspaces | 2: root (82 members, `exclude = ["dod"]`) and `dod/` (32 members, `dod/crates/*`) | 1: root, 82 + 32 members; DoD block listed explicitly (no glob, §19) |
| Lockfiles | `Cargo.lock` + `dod/Cargo.lock` | `Cargo.lock` only |
| `[workspace.package]` | duplicated in both (identical values) | root only |
| `[workspace.dependencies]` | duplicated; `semver` only in `dod/` | root only; `semver` added to root |
| `[workspace.lints.rust]` | `unsafe_code = "forbid"` in both | root only (same rule) |
| Profiles | `release`, `dev` in both; `release-runner` root only | root only (unchanged values) |
| Build of DoD binaries | `cd dod && cargo …` / `--manifest-path dod/Cargo.toml` | root manifest with `-p` selections (`dod/Makefile`, CI job `dod`) |
| `xtask gates` graph | `WorkspaceGraph::load_many(&[".", "dod"])` | `WorkspaceGraph::load(".")` (same function `load_all_workspaces`) |
| Warden budget lockfile | `dod/Cargo.lock` | `Cargo.lock` |
| Dependabot | two `cargo` entries (`/`, `/dod`) | one (`/`) |

The 32 DoD packages keep their directories under `dod/crates/`; path
dependencies DoD → product (`../../../harw-types` etc.) and product → DoD
(`harw-knowledge`, `harw-plan-bridge`, `harw-core-bridge`, `harw-macros`
[dev]) are unchanged and remain valid.

## 2. Removed DoD workspace settings

`dod/Cargo.toml` consisted only of workspace-level sections. Every section was
removed; with nothing left the file was deleted, together with
`dod/Cargo.lock`.

| Removed section | Content | Where it lives now |
|---|---|---|
| `[workspace]` | `resolver = "2"`, 32 `members` | root `[workspace]` (`resolver = "2"`, DoD block in `members`) |
| `[workspace.package]` | `version = "0.3.0"`, `edition = "2024"`, `rust-version = "1.85"`, `license = "MIT OR Apache-2.0"`, `repository` | identical keys in the root (the root adds `publish = false`; every DoD crate already sets `publish = false` itself) |
| `[workspace.dependencies]` | `jiff`, `serde`, `serde_json`, `toml`, `blake3`, `ipnet`, `fs4`, `tempfile`, `tracing`, `rustix`, `semver` | root table; all identical except `semver`, which was DoD-only and was added to the root as `semver = { version = "1.0.28", features = ["serde"] }` |
| `[workspace.lints.rust]` | `unsafe_code = "forbid"` | root, identical |
| `[profile.release]` | `lto = "thin"`, `codegen-units = 16` | root, identical |
| `[profile.dev]` | `opt-level = 0` | root, identical |

DoD crates use exactly these inherited keys: `version`, `edition`, `license`,
`repository`, `rust-version` (`.workspace = true`, 32× each), `[lints]
workspace = true` (32×) and dependencies `jiff`, `serde`, `serde_json`,
`toml`, `blake3`, `ipnet`, `tempfile`, `tracing`, `rustix` (features `fs`,
`net`, `event`, `time` per crate) and `semver`. All of them resolve against
the root now; `semver` was the only missing key.

No `dod/deny.toml` existed; `cargo deny` previously ran only on the root
workspace, so the DoD-only external crates were never license-checked. They
are now; their licenses are all on the allow list (see §3).

## 3. Version and lockfile differences

### 3.1 Manifest-level

Shared `[workspace.dependencies]` versions were already identical. Direct,
non-inherited DoD dependencies and their root counterparts:

| Crate | DoD requirement | Root |
|---|---|---|
| `clap` | `4.6.1` + `derive` (4 binaries) | `4.6.1`/`4.6.7` + `derive`; locked 4.6.7 in both |
| `tracing-subscriber` | `0.3.23` + `env-filter` | same + `json`/`fmt`/`ansi`/`registry` in product crates |
| `rayon` | `1.12.0` (`harw-dod-rules`) | locked 1.12.0 |
| `nix` | `0.31`, no default features, `fanotify` (`harw-probe-fs`) | 0.29 transitively via `zbus` |
| `landlock` | `0.4.7` (4 binaries) | — (DoD-only) |
| `aya` | `0.14.0` (`harw-dod-bpf`) | — (DoD-only) |
| `sd-listen-fds` | `0.2.0` (`harw-warden`) | — (DoD-only) |

### 3.2 Expected lockfile changes (root `Cargo.lock`)

Packages that were only in `dod/Cargo.lock` enter the root lock:
`aya` 0.14.0, `aya-obj` 0.3.0, `object` 0.39.1, `landlock` 0.4.7,
`sd-listen-fds` 0.2.0, `assert_matches` 1.5.0 (all `MIT OR Apache-2.0` or
`MIT/Apache-2.0`), plus `nix` 0.31.3 (`MIT`).

Expected coexistence (two majors, no common choice possible):

- `nix` 0.29.0 (`zbus`, OS keyring in `harw-secrets`) **and** `nix` 0.31.x
  (`harw-probe-fs`). `deny.toml` keeps `multiple-versions = "warn"`; a comment
  records this pair. No `skip` entry is needed.

Expected unification to the root's already-locked patch versions (Cargo keeps
existing lock entries when new members arrive; all changes are
semver-compatible):

- `rustix` 1.1.4 (DoD) → **1.1.5** (root). `harw-warden`, `harw-sentinel`,
  `harw-probe-fs` and `harw-dod-netlink` cite checks against
  `rustix-1.1.4/src/net/`; re-read the 1.1.5 diff for the `net`/`event`
  modules they use.
- DoD had newer patch versions of about 60 shared transitive crates than the
  root (e.g. `libc` 0.2.189 vs 0.2.186, `thiserror` 2.0.20 vs 2.0.18,
  `bitflags` 2.13.2 vs 2.13.0, `indexmap` 2.14.2 vs 2.14.0, `memchr`
  2.8.3 vs 2.8.2, `syn` 2.0.119 vs 2.0.117). Without a `cargo update` the
  DoD crates move to the root's (older) patch versions. That is acceptable
  unless an entering crate (`aya`, `landlock`, `object`, `nix` 0.31) requires
  a newer minimum, in which case lock regeneration bumps that entry for the
  whole workspace. Review the lock diff for such bumps.
- Versions that matter for the Warden closure are already identical in both
  lockfiles: `jiff` 0.2.37, `tokio` 1.53.1, `tokio-util` 0.7.19, `clap`
  4.6.7, `clap_complete` 4.6.11, `serde` 1.0.229, `serde_json` 1.0.151,
  `tracing` 0.1.44, `tracing-subscriber` 0.3.23, `blake3` 1.8.7. Only
  `rustix` (1.1.4 → 1.1.5) and `libc` (0.2.189 → 0.2.186) change there.

## 4. Feature-unification risk

Resolver 2 unifies features across **the packages selected for one build**.
Before the merge a `--workspace` build of `dod/` never saw product crates;
now `cargo build/clippy/test --workspace` at the root resolves DoD and
product crates together. `-p` selections are unaffected (only the selected
packages' features are unified). Shared dependencies with differing feature
sets:

| Shared dep | DoD side | Added by product crates in a `--workspace` build | Risk |
|---|---|---|---|
| `serde_json` | default | `preserve_order` (via `thirtyfour` in `harw-browser-thirtyfour`; `indexmap` appears under `serde_json` in the root lock, not in the DoD lock) | **Behavioral**: `serde_json::Map`/`Value` key order becomes insertion order instead of sorted. Struct serialization is unaffected; any DoD code that hashes, compares or golden-tests serialized `Value`s can pass in one configuration and fail in the other. Warden: `harw-warden`/`harw-dod-warden-proto` encode structs (unaffected) — verify no `Value`-based digest in the Warden path. |
| `tokio` | `rt`, `macros` (+ `tokio-util`) reached via `harw-types` | `rt-multi-thread`, `net`, `process`, `signal`, `io-std`, `io-util`, `fs`, … | **Compile scope**: more tokio code (and optional deps such as `mio`, `socket2`, `signal-hook-registry`) compiled into Warden artifacts of a `--workspace` build. Not visible in `cargo tree -p harw-warden`, not counted by `warden-deps`. |
| `rustix` 1.x | `fs`, `net`, `event`, `time` in the DoD manifests, plus `process` via `harw-authority` (unix normal dependency for `geteuid`), which `harw-dod-cap`/`-rules`/`-sentinel` pull into most DoD crates | whatever product-side transitive users enable | Compile scope: package-scoped DoD builds compile `process` too. Warden uses `net` only; the Warden chain `harw-warden` → `harw-dod-warden` → `harw-dod-warden-proto` does not reach `harw-authority`, so its package-scoped build and the `warden-deps` budget are unchanged. |
| `tracing-subscriber` | `env-filter` | `json`, `fmt`, `ansi`, `registry`, `std` | Compile scope; `json` pulls `tracing-serde`/`serde_json` into the subscriber. |
| `harw-completions` | `clap-args` (all 4 DoD binaries) | `clap-args` (`harw-cli`) | None: same feature on both sides. |
| `harw-authority` | none | `test-support` via `[dev-dependencies]` of `harw-tools`, `harw-core-bridge` | **Tests only**: `harw-authority` is a normal dependency of `harw-dod-cap` and therefore of nearly every DoD crate (not of the Warden chain `harw-warden` → `harw-dod-warden` → `harw-dod-warden-proto`). In `cargo test`/`clippy --tests --workspace` those DoD test builds now compile `harw-authority` with `test-support` (`SandboxSpec::from_resolved_for_test`). Not in any non-test build; the package-scoped CI job `dod` tests without it. |
| `harw-dod-rules` | `test-support` via dev-deps of `harw-dod`, `harw-dod-escalate` | also via dev-dep of `harw-plan-bridge` | None new: already unified inside the old DoD workspace test build. |

Mitigations in this change:

- The packaged binaries are built package-scoped: `dod/Makefile` `build`
  runs `cargo build -p harw-sentinel -p harw-probe-bpf -p harw-probe-fs
  -p harw-warden` against the root manifest and keeps its own target
  directory `dod/target`, so `install`/`install-existing` never pick up a
  binary produced by a `--workspace` build in the root `target/`.
- CI job `dod` builds the four binaries and tests all DoD crates with `-p`
  selections only (no product features unified in), in addition to the
  `--workspace` run of the `check` job. Both configurations must be green.
- The `warden-deps` gate computes the Warden closure from `harw-warden`
  alone (its own feature resolution), matching the package-scoped build.

Open risk (flag for review): nothing prevents someone from shipping a Warden
built by `cargo build --release --workspace`. That binary would contain the
unified `tokio`/`rustix`/`serde_json` feature set above. `release.yml` does
not build DoD binaries today; if it ever does, it must use `-p`.

## 5. TCB invariants (architecture §22, §23, §50, §51)

These must hold after the lock regeneration, measured by the central build:

1. **Warden runtime closure does not grow.** `MAX_WARDEN_RUNTIME_DEPS = 54`
   in `xtask/src/gate_warden.rs` is a ratchet and must not be raised for this
   merge. `cargo tree -p harw-warden -e normal --prefix none | sort -u | wc -l`
   must not exceed the value measured on the pre-merge tree (run the same
   command against the pre-merge commit with `--manifest-path dod/Cargo.toml`
   for the baseline).
2. **No C build in the Warden subtree.** `cargo tree -i libbpf-sys` and
   `cargo tree -i openssl-sys` (workspace-wide, then `-p harw-warden`) must
   not show a path from `harw-warden`/`harw-dod-warden`. `aya` is pure Rust
   and must stay confined to `harw-dod-bpf` → `harw-dod-procmon`/`-flow` →
   `harw-probe-bpf`/`harw-dod` (`privileged`).
3. **Critical graphs compared** (§50): `harw-dod-warden`, `harw-dod-sentinel`,
   `harw-warden`, `harw-sentinel`, `harw-probe-bpf`, `harw-probe-fs` —
   `cargo tree -p <crate> -e normal` before vs after; differences may only be
   patch-version moves from §3.2.
4. **No new edges into the Warden.** Being a workspace member does not permit
   `harw-dod-warden` → `harw-runtime`/providers/HTTP/browser/job runtime
   (§22). The `edges` and `arch` gates must stay green; the new
   `harw-job-core` crate must not appear in `cargo tree -i harw-job-core`
   under any Warden package.
5. **One workspace, all DoD crates in the graph.** New xtask test
   `gates::tests::test_dod_crates_are_root_workspace_members` fails if
   `dod/Cargo.toml` or `dod/Cargo.lock` reappears or a directory under
   `dod/crates/` is missing from the root `members`.

## 6. Files touched by PL-60

- `Cargo.toml` — removed `exclude = ["dod"]` and its comment, added the DoD
  members block, added `semver` to `[workspace.dependencies]`.
- `dod/Cargo.toml`, `dod/Cargo.lock` — deleted.
- `xtask/src/gates.rs` — `load_all_workspaces` loads the root only;
  `DOD_WORKSPACE` removed; new regression test.
- `xtask/src/gate_warden.rs` — Warden closure uses the root `Cargo.lock` and
  the root `[workspace.dependencies]`; docs updated.
- `xtask/src/gate_privileges.rs` — docs/message wording.
- `.github/workflows/ci.yml` — removed `cargo fetch --manifest-path
  dod/Cargo.toml`; job `dod` runs from the root with `-p` selections.
- `.github/dependabot.yml` — removed the `/dod` cargo entry.
- `dod/Makefile` — all Cargo calls via the root manifest with `-p`.
- `deny.toml` — comment on the expected `nix` 0.29/0.31 pair.
- Docs: `CLAUDE.md`, `CONTRIBUTING.md`, `CHANGELOG.md`, `Makefile` comment,
  `rust-toolchain.toml` comment, `docs/setup/{build-prerequisites,install,dod}.md`,
  `docs/design/{build-history,harw-dod-crate-decomposition,harw-dod-charter,harw-dod-integration-and-dependencies,harw-security-observability-plan,dod-system-operations-authority}.md`,
  `docs/planning/50-dod-integration/README.md`.

## 7. Central build checklist

Run once, after all parallel work has landed, with one build setting
(in the cloud container: `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0
CARGO_PROFILE_TEST_DEBUG=0`):

```bash
# Baseline from the pre-merge commit (separate worktree, own lockfile)
git worktree add /tmp/pre-pl60 HEAD
cargo tree --manifest-path /tmp/pre-pl60/dod/Cargo.toml -p harw-warden -e normal --prefix none | sort -u | wc -l
for c in harw-dod-warden harw-dod-sentinel harw-warden harw-sentinel harw-probe-bpf harw-probe-fs; do
  cargo tree --manifest-path /tmp/pre-pl60/dod/Cargo.toml -p "$c" -e normal --prefix none | sort -u > "/tmp/pre-$c.txt"
done

# Regenerate the root lock (no --locked on this first call)
cargo metadata --format-version 1 >/dev/null
git diff --stat Cargo.lock

cargo fmt --all
cargo clippy --workspace --tests -- -D warnings
cargo nextest run --workspace && cargo test --workspace --doc
cargo run -q -p xtask -- gates
cargo tree -p harw-warden -e normal --prefix none | sort -u | wc -l
for c in harw-dod-warden harw-dod-sentinel harw-warden harw-sentinel harw-probe-bpf harw-probe-fs; do
  cargo tree -p "$c" -e normal --prefix none | sort -u | diff "/tmp/pre-$c.txt" - ; done
cargo tree -i libbpf-sys ; cargo tree -i openssl-sys
cargo tree -p harw-warden -i openssl-sys ; cargo tree -p harw-warden -i libbpf-sys
cargo deny check
make -C dod clippy test          # package-scoped DoD run, --locked
cargo build --locked -p harw-sentinel -p harw-probe-bpf -p harw-probe-fs -p harw-warden
actionlint
```
