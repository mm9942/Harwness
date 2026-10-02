# Consolidation branch (`consolidate/main`)

Fixed integration branch for the coding work. Every worker branch is merged here
(no-ff, one merge commit per branch) by the orchestrator, never by workers. `dev` is
not touched without explicit per-PR approval.

## Step contract (runs after every worker wave)

1. **Collect**: fetch worker branches, verify pushed head SHA and clean worktree.
2. **Merge** in dependency order (M1 → M2…M9; skeleton → S01…S10 per DAG).
3. **Synthesis**: resolve overlaps (shared `harw-macros/src/lib.rs`, registration files),
   remove now-redundant duplicates, align naming, update this log.
4. **Gates on a frozen SHA** (any later commit invalidates): `cargo fmt --check`,
   `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
   xtask gates, `cargo deny`, `make -C dod clippy test`, actionlint. Record results below;
   gates that cannot run in the sandbox are listed as NOT RUN, never as passed.
5. **Publish**: push `consolidate/main`; draft PR only on request.

## Log

| Branch | Merged | Head SHA | Notes | Gates |
|--------|--------|----------|-------|-------|
| macro/m6-kebab-snake | yes | c2899e3 | KebabEnum snake_case; WaveJoin::parse now lenient (open question); harw-tui/harw-ops/harw-plan not yet built | NOT RUN (workspace) |
| ws/s01-contract | yes (UNBUILT merged) | c6e5ec5 | W00 §3.5/3.6 contract delta, PlacementGeneration, golden fixtures; fmt+tests passed on branch, clippy unverified; H0–H9 labels do not exist in hub docs (hub uses RS0–RS9) — drop/replace H column in W00-ws-integration-map.md | NOT RUN |
| ws/s07-reconnect-alias | yes (UNBUILT) | see merge commit | backoff, resume cursors, AliasStore; never compiled; expected_node charset unchecked vs NodeId | NOT RUN |
| ws/s02-node-upgrade | yes (UNBUILT merged) | see merge commit | upgrade fns implemented; clippy passed before last small edits, tests never ran; tests use raw I/O not harw-session-ws (needs dev-deps harw-session-ws + futures-util added by orchestrator) | NOT RUN |
| ws/s05-daemon | NOT merged | de8e789 | based on #91 (coop/hub-daemon); needs workspace member + arch-policy via stacked skeleton; fmt+clippy clean, tests never ran | – |
| macro/m1-test-support | pending | – | – | – |
| macro/m2-harwerror | pending | – | – | – |
| macro/m3-tool-provider | yes (UNBUILT) | see merge commit | conflicts in harw-tool-plan provider.rs and skill_tools.rs resolved by hand (M3 macro form + M4 helpers); needs `cargo check -p harw-tool-plan -p harw-registry-defaults --all-targets` (unused imports likely: JsonSchema, Permission); 11 providers migrated with golden tests; same tunnel.status test failure | NOT RUN (disk full) |
| macro/m4-schema-helpers | yes | see merge commit | schema_helpers + parse_args; net ~47 lines; harw-registry-defaults test `auto_approved_tools_are_a_subset_of_the_read_only_surface` (tunnel.status) fails, cause unverified on clean dev (tunnel code is already on dev) | NOT RUN (workspace) |
| macro/m789-ids-limits-registry | yes (UNBUILT merged) | see merge commit | HarwId validate=, limits_struct!, default_true, diagnostic_code!, register_ops!, catalog parity test; skipped id_newtype!/string_id!/hand-rolled ids; harw-netsec/Cargo.toml conflict resolved (both harw-macros comments); agent claims crate tests passed on branch; Cargo.lock auto-merged | NOT RUN |
| ws/skeleton-dev | yes (UNBUILT) | 61ddf16 | 3 stub crates + upgrade stubs + `harw attach`; Cargo.lock auto-merged with M6 deps, must be re-resolved by a build; `durable_replay.rs` named so because .gitignore has `**/*transcript*`; skeleton-branch checks: fmt, clippy (6 crates), tests (6 crates), xtask arch/edges/privileges/warden-deps/warden-cbuild passed; harw-tui/harw-cli tests not completed (disk) | NOT RUN on merged SHA |
| ws/skeleton-stack | not started | – | – | – |

## Known synthesis hotspots

- `harw-macros/src/lib.rs`: touched by M2, M6, M7 (expect textual conflicts).
- Workspace `Cargo.toml` members and arch-policy: touched by the skeleton only.
