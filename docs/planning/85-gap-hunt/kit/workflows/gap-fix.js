export const meta = {
  name: 'gap-fix',
  description: 'Fix verified single-file findings: one fixer per file (Opus for critical/high), adversarial review with lessons checklist, one repair pass, final cross-file ripple check; no builds',
  whenToUse: 'After gap-hunt-area runs were consolidated. Pass findings for a DISJOINT file set; run several gap-fix workflows in parallel only if their file sets do not overlap. Multi-file findings go to a contract wave instead.',
  phases: [
    { title: 'Fix', detail: 'one agent per file, edits only that file' },
    { title: 'Review', detail: 'adversarial review, one repair pass, re-review of the repair' },
    { title: 'Ripple', detail: 'Opus cross-file consistency check over the diff (read-only)' },
  ],
}

// Args:
//   findings  array of verified findings (file, line, severity, title, description, evidence, fix, test_idea)
//   key       label for this batch
//   catalog   pattern catalog path (default docs/planning/85-gap-hunt/patterns.md)
//   rules     binding code rules (default: the workspace rules below)
//   ripple    true (default) to run the final cross-file check
//   reviewOnly  repo-relative files whose fixes are already in the working
//               tree (e.g. a run died at a session limit after its fixer
//               finished): skip the fixer, review and repair only
//   root      absolute path of the checkout to work in (required, catalog
//             P14). One workflow, one branch: cut a git worktree per wave and
//             pass its path here, so every wave commits on its own branch and
//             the main tree stays clean (catalog P11).
//   partial     repo-relative files a dead fixer may have left half-edited:
//               the new fixer inspects `git diff -- <file>` first and
//               completes or corrects that edit instead of starting over
// After a session limit, resume with the UNCHANGED script (Workflow
// resumeFromRunId): completed agents replay from the journal and failed ones
// run again. A resume replays only the longest unchanged prefix of agent calls;
// after the first edited or new call everything runs live, fixers included, so
// never resume a writing wave onto changed logic (catalog P12). Run a missing
// step as a separate agent instead. Use reviewOnly and partial when the
// original args are gone or the batch has to be recut.
const A = args || {}
const CATALOG = A.catalog || 'docs/planning/85-gap-hunt/patterns.md'
const RULES = A.rules || 'no let-chains (`if let … && …`, MSRV 1.85), forbid(unsafe), no unwrap/expect/panic! in library code OR tests/doctests (tests return TestResult and use the crate helpers), no third-party types in public APIs, hand-written error types, match the file\'s comment language and density, no book titles/authors/quotes anywhere'
const BUILD_RULE = 'Subagents and parallel agents must **never** run `cargo` or `rustc` in any form: no `check`, `build`, `test`, `nextest`, `clippy`, `fmt`, `run`, `doc`, `deny`, and no `make` target that calls them. They only read and edit code. At the end they report which tests they added and which commands the central build must run.'
// root is required (catalog P14): the session's working directory follows a
// `cd` of the main session, so "the current directory" is not a safe default.
if (!A.root) throw new Error('root is required: pass the absolute path of the checkout (catalog P14)')
const ROOT = A.root
const WHERE = ROOT
  ? `Repository root: ${ROOT} (a git worktree on its own branch). Every path below is relative to that root: read and edit files only under it, and run git as \`git -C ${ROOT} …\`. Never touch the same path in any other checkout.`
  : 'Repository: the current working directory (Rust workspace).'
const GIT = ROOT ? `git -C ${ROOT}` : 'git'
const CONTEXT = `${WHERE} Binding code rules: ${RULES}. Pattern catalog: ${CATALOG}. ${BUILD_RULE}`

// Lessons from earlier hunts (catalog P1/P2): fixes caused follow-up findings.
const FIX_RULES = `
Binding lessons (catalog P1/P2):
- Update every doc/comment in this file that describes the behaviour you change.
- New tests and doctests: no unwrap/expect/panic!; use TestResult and the crate's helpers.
- Do not add pub/pub(crate) items without a production caller; no infrastructure that nothing calls. If the proper fix needs wiring in another file, fix only what is safe here and put the rest in skipped with the exact missing call site.
- No performance regressions (no whole-tree re-walks, no locks across await).
- Keep the diff small: if the fix needs more than ~150 changed lines, skip it and explain.`
const REVIEW_RULES = `
Also check: docs/comments in the file still match the new behaviour; new tests/doctests have no unwrap/expect/panic!; no new pub item without a production caller (grep); no dead or unwired code; no performance regression; diff size proportionate to the finding.`

const FIXED = {
  type: 'object',
  properties: {
    file: { type: 'string' },
    fixed: { type: 'array', items: { type: 'string' } },
    skipped: { type: 'array', items: { type: 'object', properties: { title: { type: 'string' }, reason: { type: 'string' } }, required: ['title', 'reason'] } },
    tests_added: { type: 'array', items: { type: 'string' } },
    notes: { type: 'string' },
  },
  required: ['file', 'fixed', 'skipped', 'tests_added'],
}
const REVIEW = {
  type: 'object',
  properties: { ok: { type: 'boolean' }, problems: { type: 'array', items: { type: 'string' } } },
  required: ['ok', 'problems'],
}
const RIPPLE = {
  type: 'object',
  properties: {
    items: { type: 'array', items: { type: 'object', properties: { file: { type: 'string' }, line: { type: 'integer' }, problem: { type: 'string' }, fix: { type: 'string' } }, required: ['file', 'problem'] } },
  },
  required: ['items'],
}

// Canonical repo-relative paths (catalog P7): one group per file, never two fixers on one file.
const norm = p => String(p || '').replace(/^\/.*?\/(?=[^/]+\/src\/|docs\/|dod\/|deploy\/|xtask\/|Cargo\.toml)/, '').replace(/^\.\//, '')
const byFile = {}
for (const f of A.findings || []) {
  const file = norm(f.file)
  ;(byFile[file] = byFile[file] || []).push({ ...f, file })
}
const groups = Object.entries(byFile)
const REVIEW_ONLY = new Set((A.reviewOnly || []).map(norm))
const PARTIAL = new Set((A.partial || []).map(norm))
log(`${A.key || 'batch'}: ${groups.length} files, ${(A.findings || []).length} findings`)

const fixReports = []
const reviewReports = []
const results = await pipeline(groups,
  ([file, fs]) => REVIEW_ONLY.has(file) ? { file, fixed: [], skipped: [], notes: 'fix already applied in the working tree (reviewOnly)' } : agent(`${CONTEXT}

You are a fixer. Edit ONLY this one file: ${file}.${PARTIAL.has(file) ? ` An earlier fixer for this file died mid-run and may have left a partial edit: read \`${GIT} diff -- ${file}\` first, then complete or correct that edit rather than starting over.` : ''} Do not create or edit any other file. Use the Edit tool (no temp files). Never commit.
Fix these verified findings (skip one only if the fix would be riskier than the defect, and say why):
${fs.map((f, i) => `${i + 1}. [${f.severity}] ${f.title} (line ${f.line})\n   ${f.description}\n   Evidence: ${f.evidence}\n   Fix: ${f.fix}\n   Test idea: ${f.test_idea || '-'}`).join('\n')}
Add or adjust tests in this file's test module where sensible. Keep public signatures stable unless a finding requires otherwise; callers in other files must still compile.${FIX_RULES}`,
    { label: `fix:${file}`, phase: 'Fix', schema: FIXED, model: fs.some(f => f.severity === 'critical' || f.severity === 'high') ? 'opus' : 'sonnet', agentType: 'focused-coder' }),
  (fixRes, [file, fs]) => {
    if (fixRes) fixReports.push(fixRes)
    const review = agent(`${CONTEXT}

You are an adversarial reviewer of a just-made fix. File: ${file}. Inspect \`${GIT} diff -- ${file}\` (it may contain earlier unrelated edits; focus on these findings):
${fs.map(f => `- ${f.title}: ${f.fix}`).join('\n')}
Fixer report: ${JSON.stringify(fixRes || {})}
Check by reading: the defect is really fixed; no new bug; binding rules hold; cfgs compile on all target platforms; no dead code that clippy -D warnings rejects; callers in other files still compile (grep); comment language matches. Do not edit. ok=false only with concrete problems.${REVIEW_RULES}`,
      { label: `review:${file}`, phase: 'Review', schema: REVIEW, model: 'sonnet', agentType: 'focused-explorer' })
    return review.then(rev => ({ rev, fixRes }))
  },
  // A file is done only with a fixer report AND a successful review. A missing
  // agent (session limit, crash) never counts as success: it stays
  // `unverified`; a repair is re-reviewed before it counts as `repaired`.
  async ({ rev, fixRes }, [file, fs]) => {
    if (rev) reviewReports.push({ file, ...rev })
    if (!fixRes) return { file, status: 'unverified', reason: 'no fixer report' }
    if (!rev) return { file, status: 'unverified', reason: 'no review' }
    if (rev.ok) return { file, status: 'ok' }
    if (!(rev.problems || []).length) return { file, status: 'unverified', reason: 'review returned ok=false without naming a problem' }
    const repair = await agent(`${CONTEXT}

You are a repair fixer. Edit ONLY this one file: ${file} (Edit tool, no temp files, never commit). A reviewer found these problems in the latest changes to it:
- ${rev.problems.join('\n- ')}
Fix them minimally. Do not widen scope: if a problem needs another file, report it instead.${FIX_RULES}`,
      { label: `repair:${file}`, phase: 'Review', schema: FIXED, model: 'sonnet', agentType: 'focused-coder' })
    if (!repair) return { file, status: 'unresolved', reason: 'repair agent did not answer', problems: rev.problems }
    fixReports.push({ ...repair, repair: true })
    const recheck = await agent(`${CONTEXT}

You re-review a repaired fix. File: ${file}. Inspect \`${GIT} diff -- ${file}\`. The first review found:
- ${rev.problems.join('\n- ')}
Repair report: ${JSON.stringify(repair)}
Findings the file must fix:
${fs.map(f => `- ${f.title}: ${f.fix}`).join('\n')}
Check that every problem above is resolved and that the repair introduced no new one. Do not edit. ok=false only with concrete problems.${REVIEW_RULES}`,
      { label: `rereview:${file}`, phase: 'Review', schema: REVIEW, model: 'sonnet', agentType: 'focused-explorer' })
    if (!recheck) return { file, status: 'unresolved', reason: 'no re-review after repair', problems: rev.problems }
    reviewReports.push({ file, rereview: true, ...recheck })
    return recheck.ok ? { file, status: 'repaired' } : { file, status: 'unresolved', reason: 're-review still finds problems', problems: recheck.problems }
  },
)

let ripple = null
if (A.ripple !== false && groups.length) {
  phase('Ripple')
  ripple = await agent(`${CONTEXT}

You are the cross-file consistency checker. Files just changed by one-file fixers: ${groups.map(g => g[0]).join(', ')}. Read \`${GIT} diff\` for them and look ONLY for cross-file ripple: callers/tests elsewhere that no longer match a changed signature or behaviour, docs/comments/ledger/guides elsewhere describing the old behaviour, pub helpers without any production caller, the same issue fixed twice in conflicting ways, anything that would fail to compile or fail clippy -D warnings across files. Do not edit. Report concrete items with file:line.`,
    { label: `ripple:${A.key || 'batch'}`, phase: 'Ripple', schema: RIPPLE, model: 'opus', agentType: 'focused-explorer' })
}

// Completion signal for committing/merging the wave: every file ok or
// repaired-and-re-reviewed, and the ripple check answered (when it ran).
const files = groups.map(([file], i) => results[i] || { file, status: 'unverified', reason: 'pipeline item dropped' })
const open = files.filter(f => f.status !== 'ok' && f.status !== 'repaired')
const rippleStatus = A.ripple === false || !groups.length ? 'skipped' : (ripple ? 'ok' : 'missing')
const complete = open.length === 0 && rippleStatus !== 'missing'
log(`${A.key || 'batch'}: ${complete ? 'complete' : 'NOT complete'}; ${files.length - open.length}/${files.length} files done, ripple ${rippleStatus}`)
return { key: A.key || 'batch', complete, open, files, fixReports, reviewReports, rippleStatus, ripple: ripple ? ripple.items : null }
