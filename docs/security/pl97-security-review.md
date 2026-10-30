# PL-97 wave 1 — independent security review

**Scope.** The twelve commits of branch `ccr-54aa3dfd-o46he2` that are not on
`origin/dev` (head `47a0720e`, base `fcb12b41`), i.e. the first implementation
wave of PL-97 (`docs/planning/97-harw-dod-mission-control-chain/README.md`):
C1, D1, D2, M1, C4a, B4, E1a, B1 and M2. The intended security properties are
the ones stated in PL-97 §0.2, §3 and §8.

**Method.** Full read of `git diff origin/dev...HEAD` split into four areas
(E1a sandbox chain; D1/D2 exports and client; B1 injection and config scope;
B4/M2/C4a/M1), with the pre-existing code each change relies on (tool crates,
`harw-fsutil`, `harw-authority`, `harw-catalog` trigger engine, context
rendering). Every candidate finding was checked against the code with the aim
of refuting it; only the survivors are listed. Targeted test runs:
`cargo test -p harw-job-exec -p harw-job-linux --features linux-sandbox`
(26 + 11 + 82 pass), `cargo test -p harw-activity --lib` (51 pass),
`cargo fmt --all -- --check` (clean). Nothing was changed during the review
itself; the fixes live in the pull request that carries this document.

**Result of this pass.** Within the questions above this pass found no
critical and no high finding: one medium finding (a test gate that is red on
a clean checkout), six low findings, two of them rules the next work package
(E1b) must follow rather than defects in the landed code.

**Scope limit — this is a partial review.** The repository owner's review
threads on PR #154 cover the same commit range and hold reproducible
file:line findings this pass did not triage, so "every survivor is listed"
holds only for the questions above, not for the range as a whole. Those
threads remain authoritative for their files and are listed in §1.1 with the
state known at the time of writing; none of them is closed by this document.

## 1. Ranked findings

| # | Severity | Area | Location | Status |
|---|----------|------|----------|--------|
| 1 | Medium | M1 gate | `harw-activity/tests/replay.rs:29-35`, `.gitignore:27` | fixed on the parent branch (`a7111029`) |
| 2 | Low | D2 | `harw-dod-client/src/paths.rs:85-99` | fixed in this PR |
| 3 | Low | B1 | `harw-core/src/skill_injection.rs:451-454` | default fixed in this PR; importer wiring stays with PL-95 S8 |
| 4 | Low | E1a → E1b requirement | `harw-job-exec/src/trampoline.rs:94-105`, `harw-job-runtime/src/coordinator/linux.rs:602-612` | requirement for E1b (in progress) |
| 5 | Low | E1a → E1b requirement | `harw-job-exec/src/plan.rs:73-76, 378` | requirement for E1b (in progress) |
| 6 | Low | B4 | `harw-tool-gitread/src/index.rs:238`, `odb.rs:37`, `repo.rs:135-148` | fixed in this PR |
| 7 | Low (informational) | C4a, M1 | `harw-tool-job/src/model.rs`, `harw-activity/src/reducer.rs:149-161` | documented; reducer eviction goes to M3 |

Line numbers refer to the reviewed head `47a0720e`.

### 1.1 Findings from the PR #154 review threads, not triaged here

Reproducible findings the repository owner posted on PR #154 against the
same range (`fcb12b41..47a0720e`). This pass neither verified nor refuted
them; they are listed so this report does not read as exhaustive. Status is
the thread state at the time of writing; the threads, not this list, are
authoritative.

| Thread | Severity | Finding (owner's wording, condensed) |
|--------|----------|--------------------------------------|
| `harw-dod-client/src/findings.rs:210` | P1 | `reaches_window_start` treats the minimum `observed_at` in the read tail as proof that unread older records are outside the window; a truncated or unordered export can be marked complete. |
| `harw-dod-client/src/status.rs:328` | P1 | Warden can be reported `Enabled` with an inactive socket and an unreadable guard file (`flag_absent` is false for every error except `ENOENT`). |
| `harw-dod-client/Cargo.toml:21` | P2 | `time` 0.3.53 requires Rust 1.88, above the crate's inherited `rust-version`. |
| `dod/crates/harw-sentinel/src/main.rs:936` | P1 | Export destinations are compared lexically; `.`/`..` components or symlinked parents let findings and health aliases collide. |
| `harw-home/src/bundle.rs:1001` | P1 | `include_str!` paths escape the publishable `harw-home` package into sibling crates. |
| `harw-tool-job/src/model.rs:617` | P2 | A `harw-job-*` leaf name is not proof of a dedicated job cgroup without a delegated root. |
| `harw-activity/src/reducer.rs:619` | P2 | A cancelled `Call → Job` rebase binds the job and injects a running constituent. |
| `docs/setup/dod.md:270` | P2 | Documents a default hub findings source that `HubConfig` does not have. |
| `docs/planning/97-…/README.md:1462` | P1 | The injector's active/dedupe state is not persisted across restart; the B1 status text claims otherwise. |
| `harw-core/src/skill_injection.rs:459` | P2 | Imported provenance has no production producer (same gap as finding 3 here). |
| `harw-core/src/skill_injection.rs:960` | P2 | The tool-result scan limit is enforced after collecting every leaf, not while traversing. |
| `harw-core/src/turn_loop.rs:6128` | P2 | An injection delivery is acknowledged before its event is durable. |
| `harw-tools/src/provider_macro.rs:511` | P1 | `TOOL_ACTIVITIES` is not reachable after provider registration (type erasure). |
| `docs/design/dod-system-operations-authority.md:413` | P2 | `permission_request!` has no productive consumer yet; status should stay partial. |
| `dod/crates/harw-probe-bpf/src/scope_rescan.rs:231` | P1 | Partial per-handle scope updates are declared recovered (E3b, outside this pass's range). |

**Updates after the report (PR #154 branch).**

- `harw-dod-client/src/status.rs:328` is fixed in `64d3831c`.
- `harw-activity/src/reducer.rs:619` is fixed in `ff7ca74a`.
- `dod/crates/harw-probe-bpf/src/scope_rescan.rs:231` is fixed in the E3c
  commit. Every handle's map converges before recovery is reported. The
  regression tests are
  `partial_insert_converges_when_the_descendant_vanishes_before_the_retry`
  and `partial_removal_converges_when_the_descendant_is_observed_again`.
- PR #159 assigns ten of the remaining rows to the Codex helper. Its
  handoff file `docs/planning/97-harw-dod-mission-control-chain/HANDOFF-review-fixes.md`
  lists each row with its owner.

Where the two lists overlap (`harw-activity` fixtures, imported provenance,
reducer behaviour, job cgroup leaf), the owner's thread carries the
stricter reading.

### 1. Medium — `harw-activity` replay fixtures are not in git; the workspace test gate is red

- **Where.** `harw-activity/tests/replay.rs:29-35` `include_str!`s seven
  `tests/fixtures/*.jsonl` files. `.gitignore:27` ignores `*.jsonl`, so the
  files were never committed (`git ls-files harw-activity/tests` lists only
  `replay.rs`; the directory does not exist on a clean clone).
- **Scenario.** On any checkout other than the author's,
  `cargo test -p harw-activity` fails to compile the integration test target
  (seven "couldn't read … fixtures/….jsonl" errors), which turns
  `cargo test --workspace` / `cargo nextest run --workspace` red. The library
  tests pass (51/51), so the reducer itself is not at fault.
- **Reachability.** Every CI run and every reviewer.
- **Fix.** The seven fixtures were force-added on the parent branch
  (`a7111029`, merged into this PR's branch). Regression guard: the test
  target itself; it cannot compile without the files.

### 2. Low — the export resolver silently downgrades to a forgeable source

- **Where.** `harw-dod-client/src/paths.rs:85-99`: `is_listable_dir` returns
  `false` for every failure, `EACCES` included, so `resolve_export_paths`
  falls through to `<HARW home>/dod/export` (`ExportSource::Home`).
- **Scenario.** With the shipped layout (`/var/lib/harw-dod` is `0710
  harw-sentinel:harw-dod-readers`, `deploy/tmpfiles.d/harw.conf`; the group is
  empty by default, `deploy/sysusers.d/harw.conf`) every operator who has not
  been added to `harw-dod-readers` reads the home directory instead. That
  directory is owned by the harw user and reachable by anything that writes
  below HARW home. `status()` (`status.rs:208-267`) then reports
  `findings_export: Readable`, counts planted `critical` lines and
  `attention_count()` is `Some(n)` as soon as the unit files exist. Only
  `export_paths.source` carries the fact; nothing marks `Home` as unverified.
- **Reachability.** Not on this branch: no consumer of `harw-dod-client` is
  wired yet (`grep harw_dod_client` hits only `Cargo.toml` and the two xtask
  policies). It becomes reachable with D3 (`harw dod status`, `dod.*` tools)
  and D5 (TUI badge).
- **Fix (this PR, additive).** `resolve_export_paths` distinguishes "system
  directory exists but cannot be listed" from "absent": the former yields the
  new `ExportSource::SystemUnreadable` with the *system* paths, so every read
  reports `Unreadable` and `status()` appends the operator hint ("add the
  user to the harw-dod-readers group") instead of counting home findings.
  `ExportSource::is_verified()` is `true` for `System` only: `Override` is a
  path a trusted *config* layer named, but `[dod] export_dir` is only
  required to be absolute and nothing checks who can write that directory
  (`/tmp/dod-export` or a directory below the harw home is accepted), so
  config provenance must not be read as file integrity (owner's review of
  PR #156). `hint()` and `DodStatus::export_verified()` let D3/D5 label every
  unverified source; `attention_count()` is unchanged. Migration for D3: a `match` on `ExportSource` gains one arm;
  `DodStatus` has no new field. Regression tests:
  `unreadable_system_dir_is_reported_instead_of_the_home_fallback`,
  `unreadable_system_export_is_not_replaced_by_planted_home_findings`
  (both skip the unreadable branch when running as root),
  `only_the_system_directory_counts_as_verified` and
  `writable_absolute_override_is_not_verified`.

### 3. Low — `provenance()` fails toward the most trusted class

- **Where.** `harw-core/src/skill_injection.rs:451-454`:
  `Some(SkillSource::Layer(_)) | None => SkillProvenance::TrustedLayer`, i.e.
  a skill name that is not in the index gets `TrustClass::Instruction`.
- **Scenario.** Unreachable today: `harw_catalog::skill_select::select`
  (`skill_select.rs:266-277`) only yields indexed skills and `provenance` is
  only called on its result. The default direction is nevertheless wrong for
  a security switch.
- **Related.** `[skills] inject_imported` (marked `security_critical`) guards
  nothing yet: `with_imported_skills` (`skill_injection.rs:423`) is the only
  way to populate the `imported` set and has no production caller. The
  PL-95 S8 importer must call it before any imported skill can enter the
  index; otherwise an imported skill would land as `Layer(_)` → Instruction.
- **Fix (this PR).** `None => SkillProvenance::Imported` (Evidence) with the
  regression test `unknown_skill_names_fail_closed_to_imported`. The importer
  wiring stays with S8.

### 4. Low — E1b requirement: the trampoline report must not be writable by the job

- **Where.** `harw-job-exec/src/trampoline.rs:94-105` creates `report.json`
  `O_EXCL | 0600` inside the attempt directory before the restrictions and
  writes it through the still-open fd after `apply_controls` (Landlock checks
  at `open`, so the write needs no rule). `harw-job-runtime/src/coordinator/linux.rs:602-612`
  (`late_report`) and `harw-job-exec/src/report.rs:77-85` (`merge_reports`)
  take `filesystem`, `network`, `no_new_privs` and `capabilities` from that
  file as the stronger layer.
- **Scenario.** PL-97 §3.4 plans to bind the attempt directory read-write at
  `/run/harw-job/attempt` "rw for the report" and the `bwrap_like_map` test
  fixture mirrors that. If E1b also puts that destination into the Landlock
  `read_write` list, the confined job (same uid) can truncate and rewrite
  `report.json` after the trampoline and claim `Enforced` on every dimension;
  `late_report` / `merge_reports` would persist the forged "honest status".
- **Reachability.** Not on this branch: `/run` is in `SYSTEM_READ` only and
  the fixture withholds it, so Landlock denies the write (fail-closed). No
  production caller of `PathMap`, `plan_landlock_mapped` or `merge_reports`
  exists yet.
- **Fix.** In E1b never add the attempt destination to the Landlock
  read-write policy (the fd write already works), or hand the report over an
  inherited pipe fd (`--report-fd`) instead of a file inside the job's view.

### 5. Low — E1b requirement: `CgroupJoin::Verify` and bwrap `--unshare-all`

- **Where.** `harw-job-exec/src/plan.rs:73-76` documents it, the §3.4 diagram
  still shows `bwrap --unshare-all`.
- **Scenario.** Inside a fresh cgroup namespace `/proc/self/cgroup` reads
  `0::/`; `validate_verify` rejects `/` as an expected path (`plan.rs:378`)
  and `check` never matches it, so every BwrapTrampoline job would be refused
  with exit 126.
- **Reachability.** Correct (fail-closed) direction; purely a launcher rule.
- **Fix.** The E1b launcher uses the explicit `--unshare-*` list without
  `--unshare-cgroup`; fix the diagram.

### 6. Low — auto-approved `git.*` reads very large `.git` files into memory

- **Where.** `harw-tool-gitread/src/index.rs:238` `MAX_INDEX_BYTES = 512 MiB`,
  `odb.rs:37` `MAX_IDX_BYTES = 128 MiB`, `repo.rs:135-148` `read_git_file`
  allocates up to cap + 1 bytes per call.
- **Scenario.** `git.status` / `git.log` are now auto-approved in `Full` and
  `ReadOnlyExplore`. A workspace with a crafted or huge `.git/index` (or one
  the model writes with `fs.write` in `Full`) makes every status call
  allocate ~512 MiB without a prompt.
- **Reachability.** Any workspace the agent is pointed at. No authority
  impact; memory pressure only.
- **Fix (this PR).** `MAX_INDEX_BYTES`, `MAX_IDX_BYTES` and
  `MAX_GIT_FILE_BYTES` are 64 MiB, and `Repo::read_git_file` refuses an
  oversized file from its `fstat` size before allocating (the `take` guard
  stays for a file that grows after the check). Regression tests:
  `oversized_index_is_refused_before_it_is_read`,
  `oversized_git_file_is_refused_by_size_before_reading` and a compile-time
  pin of every per-file cap at or below 64 MiB (`const _` assertions in the
  `repo` test module). Stream-parsing the index and the
  pack indices remains the better long-term shape.

### 7. Low (informational)

- `ManagedJobOrigin` is trusted display data read from `meta.json`
  (`harw-tool-job/src/model.rs`, `deserialize_origin_lenient`). Every consumer
  (`manager.rs:815-1055`, `event.rs:51`, `tools.rs:926`, TUI `jobs_glue.rs:204-224`,
  `JobRow`) is a persistence, notice or row field; none feeds `Caller`
  checks, approval, sandbox policy or stop permission, and the TUI strips
  control characters. Provenance-only as required. Future code must not treat
  `Operator` as trust.
- The M1 reducer never evicts (`harw-activity/src/reducer.rs:149-161`,
  documented); memory is O(events) per session. Deterministic, no panics, one
  entry per event at most. The M3 TUI adapter must snapshot and trim.

## 2. Checked and found sound

**Authority (question 1).** `fsread.*` (16) and `git.*` (6) are
`ReadWorkspace`, `AUTO_APPROVED_TOOLS`, in `Full` and `ReadOnlyExplore`
(`analyst`, `explorer`, `researcher-deps` admit them). `sys.*` (12, `sys.env`
included) is `ExecuteProcess`, `Full` only, in neither approval list, so it
asks like `process.list`. `cargo.*` is unchanged (only `cargo.test_one`,
`Full`). Path confinement: the root comes only from the sandbox's canonical
workspace root; every open goes through `harw_fsutil::open_beneath`
(`openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS)`
with an equally strict per-component fallback); `RelPath` rejects `..` and
absolute paths outside the root lexically; `-L` resolution re-checks every hop
(40-hop cap). `/proc`, `/dev`, `~/.ssh`, `/etc` are unreachable; `fsread.*`
additionally refuses secret names and extensions (`.env*`, `.ssh`, `*.pem`,
AWS credentials). `gitread` is a pure-Rust reader of `<workspace>/.git`: no
`git` binary, no `-c`, hooks, pager or fsmonitor surface, no `GIT_*`
environment, a `gitdir:` pointing outside the workspace is refused, remote
URLs are never emitted. Output caps: 64 KiB / 5 000 items per response,
48 KiB text blocks, walks 100 000 entries / depth 32 / 5 s; gitread diff 1 MiB
per side, blame 20 000 lines, 200 000 commits. `sys.env` masks by name and by
value heuristics with no reveal path. `permission_request!` expands only to
`harw_authority::PermissionRequest::from_permissions` / `::empty`; the type is
consumed solely by `restrict()` and `PolicyBootstrap`, and there is no
conversion into `PermissionSet` or `AuthorityContext`, so it cannot construct
or widen a grant. `activity(...)` / `skill` constants have no non-test consumer
in the registry or authority path and never enter `ToolSpec`. A tool crate
still cannot self-register: approval requires the `AUTO_APPROVED_TOOLS` row.

**Sandbox chain (question 2).** `unified_cgroup_path` (`plan.rs:177-191`)
accepts only `0::<absolute>` lines, ignores v1/hybrid lines, returns `None`
on zero or two unified lines, relative paths, an empty file or garbage, and
takes everything after the second colon as the path. Read errors map to
`CgroupMismatch` (`trampoline.rs:272-285`) with a 64 KiB bounded read. The
comparison is component-wise `Path` equality: trailing slash and `//` are
tolerated; parent, sibling (`harw-job-10`) and child paths all mismatch;
`..` stays a component. The expected path is validated on raw bytes
(relative, `/`, NUL, newline, `.`/`..` rejected). Order: verify → rlimits →
sandbox → report → `refusal_first` → `check_requirement` → `exec`; the refusal
is returned before `exec` on every branch with exit 126, and a later rlimit
error cannot mask it. `PathMap`: absolute, `..`-free policy paths and binds
are enforced in the builder and in serde `try_from` (`deny_unknown_fields`);
matching is component-wise `strip_prefix` (`/work/repo2` ≠ `/work/repo`),
longest host prefix wins, policy paths no bind covers are dropped (empty map ⇒
empty ruleset), under read-only binds `read_write` → `read_only` and `exec` →
`read_only` unless the destination is in `SYSTEM_EXEC` (`/usr`, `/bin`,
`/sbin`, `/lib`, `/lib64`, `/lib32`, component-wise), which is never looser
than the ro-bind mount itself; `shadowed_read_only_binds` detects
same-destination and nested read-only binds; non-existent destinations are
skipped by `build_ruleset`, so no rule ⇒ denied. `merge_reports` is a pure
per-dimension maximum with `resource_limits` from the trampoline only
(exhaustive table test).

**File I/O (question 3).** Health export (`health_export.rs:284-320`): temp
sibling in the same directory, stale temp removed then `create_new`
(`O_EXCL`), `sync_data`, `rename`, temp removed on failure, target refused if
not a regular file, `0640` under `UMask=0007`; health path equal to the
findings path refused; both export directories are added as Landlock
read-write roots before `restrict_self`; `--config`, `--findings-export`,
`--health-export` require absolute paths; the install parity test pins empty
`CapabilityBoundingSet` / `AmbientCapabilities`; the health document carries
no paths, hostnames or evidence. Findings export: `symlink_metadata` refusal,
`0640`, 16 MiB cap with rotation (`keep ≥ 1`), `sanitize_scope` omits rather
than rewrites, `sanitize_summary` plus 512-byte cap, one `\n` per line. Client:
one `open(O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_NOCTTY | O_CLOEXEC)` plus
`fstat` per file, FIFO / socket / device refused (`fsio.rs:34-78`); tail
window clamped to 256 KiB, never past the size seen at open, `.1`
continuation with inode check, lines over 16 KiB, non-UTF-8, future-dated or
schema-violating lines skipped, allocation bounded by `take`; identifiers
validated, summaries sanitised (controls, bidi, zero-width) and capped at
512 bytes, `scope` dropped unless absolute, ≤ 4 KiB and clean; the health
reader checks the size via `fstat`, reads `take(MAX + 1)`, keeps ≤ 64 sensors.
`systemctl show` (`units.rs:126-172`): executable from absolute `PATH` entries
only, fixed argv (`show -p ActiveState,SubState,UnitFileState` plus six
constant unit names), stdin and stderr null, 3 s deadline with kill and reap,
stdout ≤ 16 KiB, values kept only as ≤ 64-byte ASCII tokens; non-zero exit or
timeout → `Unknown`, absence → `NotAvailable`. The crate writes nothing and
is `forbid(unsafe_code)`. Correlation (`correlate.rs:68-132`): component-wise,
`..` never matches, empty and root job components are filtered out, `/a/b`
vs `/a/bc` and `harw-job-1` vs `harw-job-10` are tested.

**Injection (question 4).** Index layers come only from
`ConfigTrustReport::layers` (root space, active profile, approved repo
`.harw`, project settings); an unapproved repo `.harw` is never a layer, so
its skills are never indexed or selectable. Trigger feed: user text →
keywords and error codes; tool output → error codes only (the four bundled
`triggers.toml` files declare only `tools` and `commands`, so this surface is
empty for bundled skills; a trusted-layer skill with `error_codes` could be
selected by repository text, which spends budget on operator-trusted text
only); `fs.*` / `doc.*` path arguments → globs; `shell.*` / `process.*` /
`job.start` → first-word rules. `inject_imported` is honoured on the single
injection path (`select_into`); the synthetic first-use trigger uses the same
`select`. Budgets are enforced in `select` before return, cuts are UTF-8 and
line-boundary safe with a marker, the injector re-caps and evicts oldest, the
session cap is ≤ 4 × per-turn, `released_bytes` only refunds bytes that never
reached the model; the compaction reset is explicit with a
`HistoryCursor::still_valid` guard, and each compaction re-opens one 16 KiB
window (documented, bounded by the context size); arithmetic is saturating or
checked throughout. Rendering: Instruction excerpts come last in
`instruction_fragments` (after the cache-stable prefix); Evidence goes into
the data block as a user text block wrapped in `BEGIN/END DATA BLOCK`, every
line prefixed with the content-line guard, control, bidi and zero-width
characters escaped; nothing enters the system prompt; delivery requires the
ceiling section and activation (fail-closed). The injection record carries
skill, section, kind, manifest key, bytes, turn, trust and SHA-256, never the
body or the touched path or command.

**Config scope (question 5).** `skills.triggered_injection` (default `true`),
`skills.inject_imported` (default `false`, security-critical),
`skills.per_turn_bytes`, `skills.per_session_bytes` are `Profile` /
`ProfileReplaces` (`scope.rs:485-492`); an untrusted project layer is ignored
with a diagnostic (`merge.rs:312-329`). `dod.export_dir` and
`dod.findings_window_hours` are parsed outside `HarnessConfig` and never
copied by `merge_restricted_dod` (`discovery.rs:985-994`: only `auto_freeze`
raise-only and `allowed_cgroup_prefixes` narrowing); `export_dir` must be
absolute and `validate()` runs via serde `try_from` in every layer; all of it
pinned by `config_scope_exhaustive.rs:1037-1143`. No `jobs.cgroup_root` key is
in this diff. An invalid value in an untrusted repo config fails discovery
rather than being ignored (repo-caused refusal to start, consistent with the
existing `[jobs]` / `[dod]` behaviour; not a widening).

**Repo rules (question 6).** Zero added `unwrap()`, `expect(`, `panic!`,
`unreachable!` or `unsafe` in the whole diff, tests included; the only
`allow` attributes are `clippy::too_many_arguments` (`export.rs`) and
`dead_code` / `unused_imports` in macro-generated code. `harw-dod-client` is
layer A in `xtask/arch-policy.toml` with a `forbidden_crates` rule against
the Warden, escalation and daemon crates; the `xtask/spawn-policy.toml` entry
for `units.rs` is lint-only, the runtime constraint is the fixed argv. Every
newly registered tool has a catalog row, a `tool_permission()` branch and
exactly one approval-list membership (covered by the new parity tests).
