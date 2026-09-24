# version-bump — detect the right semver step and apply it

Goal: look at what actually changed, decide the **smallest correct** semver bump, explain why, and apply it to the one source of truth. Default to small, continuous steps — never let the version drift into a big undocumented jump.

## 1. Find the version source(s)

Detect the ecosystem and its single source of truth (grep, don't guess):

| Ecosystem | Source of truth | Notes |
|---|---|---|
| Rust | `Cargo.toml` → `[workspace.package].version` (workspaces) or `[package].version` | Members should use `version.workspace = true`. If a `bump-version.sh` / `make bump-*` exists, prefer it. |
| Node | `package.json` → `.version` | `npm version <level>` is the canonical bumper (also tags). Monorepo: the root or each package per its policy. |
| Python | `pyproject.toml` → `[project].version` or `[tool.poetry].version`; `__version__`; `setup.cfg` | Watch for dynamic version (`hatch`, `setuptools-scm` → driven by git tags, no file to edit). |
| Go | git tags (`vX.Y.Z`) | No version file; bump = create the next tag. |
| Generic | a `VERSION` file, or a `version =` line | |

If there are **multiple** version strings that should be one (e.g. a workspace root + a member with a divergent literal), flag it and unify on the single source (`version.workspace = true` / shared field) as part of the bump.

## 2. Gather the change set

Determine what changed since the last version:

```sh
# last release tag (fallback: last commit that touched the version source)
last=$(git describe --tags --abbrev=0 2>/dev/null || echo "")
git diff ${last:+$last..}HEAD --stat        # or working tree if uncommitted
git log ${last:+$last..}HEAD --oneline      # commit subjects often state intent (feat/fix/!)
```

Also read the actual diff of public surfaces (exported items, CLI flags, HTTP routes, config keys, DB migrations, schemas), not just the stat.

## 3. Classify — the decision table

Pick the **highest** level any change triggers:

| Level | Trigger (any of) |
|---|---|
| **major** | Breaking change: removed/renamed/retyped public API, removed or renamed CLI flag / subcommand / HTTP endpoint, incompatible config or schema or non-backward-compatible migration, MSRV/runtime-version raise, default behavior change that breaks callers, Conventional-Commits `feat!`/`BREAKING CHANGE`. |
| **minor** | Backward-compatible new functionality: new public API/type, new endpoint/subcommand/flag, new optional config, new feature behind a default-off flag, `feat:` commits. |
| **patch** | Backward-compatible fixes & internal change: bug fix, perf, docs, refactor, dependency bumps within range, test/CI, `fix:`/`chore:`/`refactor:` commits. |

**Pre-1.0 (`0.y.z`) policy** (state it, then apply): under SemVer 0.x, breaking changes bump the **minor** (`0.Y+1.0`) and additive/fix changes bump the **patch** (`0.y.Z+1`). So a "major"-class change pre-1.0 becomes a minor bump, a "minor"-class change becomes a patch. Respect any project-stated policy that differs (check CONTRIBUTING / a versioning note / memory).

**Cadence bias:** prefer the smallest level the changes justify, and bump *often* (per wave/PR) rather than batching — that's what keeps steps small.

## 4. Recommend, then apply

1. State the recommended bump **and the evidence** ("minor: new `GET /admin/analytics` endpoint + new `ws_mode` config field; no breaking change"). One line of reasoning per driver.
2. Apply to the single source, preferring the project's own bumper:
   - Rust: `make bump-patch|bump-minor|bump-major` or `scripts/bump-version.sh <level>` if present; else edit `[workspace.package].version`. Then `cargo metadata --no-deps` to confirm it resolves (catches a member with a stale literal version).
   - Node: `npm version <level> --no-git-tag-version` (or with tag if the user wants the tag).
   - Python: edit `pyproject.toml`; if version is dynamic (setuptools-scm/hatch-vcs), do NOT edit a file — create the git tag instead.
   - Go / tag-driven: create the next `vX.Y.Z` tag.
3. Offer the matching **git tag** (`vX.Y.Z`) and a one-line changelog entry, but only tag/commit if the user asked.

## 5. Guardrails

- **Don't bump while a version file is being edited by another process/agent** (concurrent edit race) — apply when it's free.
- Verify the bump resolves (`cargo metadata` / `npm pkg get version` / build) before reporting done.
- Never invent a release type the changes don't support; if the diff is empty or trivial, say "no bump needed".
- If the user has a stated small-step cadence (e.g. patch-per-wave), honor it over a larger "technically minor" jump unless a real breaking change forces a higher level.
