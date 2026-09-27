export const meta = {
  name: 'contract-wave',
  description: 'Fix multi-file findings cluster by cluster: Opus contract, one coder per file, Opus cross-file review, one repair pass; no builds',
  whenToUse: 'After consolidation, for findings whose fix needs more than one file (catalog P2). Pass file-disjoint clusters; run several contract-wave workflows in parallel only if their clusters share no file. Pass root to work in a per-wave git worktree (catalog P11).',
  phases: [
    { title: 'Contract', detail: 'Opus writes one contract per cluster: per-file edits, exact signatures, tests' },
    { title: 'Code', detail: 'one focused-coder per file, edits only that file' },
    { title: 'Review', detail: 'Opus checks the whole cluster diff against its contract; one repair pass per file, then an Opus re-review' },
  ],
}

// Args:
//   key       label for this wave
//   root      absolute path of the checkout (a git worktree on its own branch); default: cwd
//   clusters  [{id, files: [repo-relative paths], findings: [verified findings]}], file-disjoint
//   catalog   pattern catalog path (default docs/planning/85-gap-hunt/patterns.md)
//   rules     binding code rules (default: the workspace rules below)
const A = args || {}
const CATALOG = A.catalog || 'docs/planning/85-gap-hunt/patterns.md'
const RULES = A.rules || 'no let-chains (`if let … && …`, MSRV 1.85), forbid(unsafe), no unwrap/expect/panic! in library code OR tests/doctests (tests return TestResult and use the crate helpers), no third-party types in public APIs, hand-written error types, match the file\'s comment language and density, no book titles/authors/quotes anywhere; ring rules in xtask/arch-policy.toml (a crate may depend only on the rings its ring allows)'
const BUILD_RULE = 'Subagents and parallel agents must **never** run `cargo` or `rustc` in any form: no `check`, `build`, `test`, `nextest`, `clippy`, `fmt`, `run`, `doc`, `deny`, and no `make` target that calls them. They only read and edit code. At the end they report which tests they added and which commands the central build must run.'
const ROOT = A.root || ''
const WHERE = ROOT
  ? `Repository root: ${ROOT} (a git worktree on its own branch). Every path below is relative to that root: read and edit files only under it, and run git as \`git -C ${ROOT} …\`. Never touch the same path in any other checkout.`
  : 'Repository: the current working directory (Rust workspace).'
const GIT = ROOT ? `git -C ${ROOT}` : 'git'
const CONTEXT = `${WHERE} Binding code rules: ${RULES}. Pattern catalog: ${CATALOG}. ${BUILD_RULE}`

const CONTRACT = {
  type: 'object',
  properties: {
    feasible: { type: 'boolean', description: 'false if the cluster needs a product decision first' },
    decisions_needed: { type: 'array', items: { type: 'string' } },
    summary: { type: 'string' },
    files: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          file: { type: 'string' },
          instructions: { type: 'string', description: 'exact edits for this file: signatures, behaviour, doc updates' },
          tests: { type: 'array', items: { type: 'string' } },
        },
        required: ['file', 'instructions', 'tests'],
      },
    },
  },
  required: ['feasible', 'decisions_needed', 'summary', 'files'],
}
const CODED = {
  type: 'object',
  properties: {
    file: { type: 'string' },
    done: { type: 'array', items: { type: 'string' } },
    deviations: { type: 'array', items: { type: 'string' } },
    tests_added: { type: 'array', items: { type: 'string' } },
    notes: { type: 'string' },
  },
  required: ['file', 'done', 'deviations', 'tests_added'],
}
const REVIEW = {
  type: 'object',
  properties: {
    ok: { type: 'boolean' },
    problems: { type: 'array', items: { type: 'object', properties: { file: { type: 'string' }, problem: { type: 'string' } }, required: ['file', 'problem'] } },
  },
  required: ['ok', 'problems'],
}

const norm = p => String(p || '').replace(/^\/.*?\/(?=[^/]+\/src\/|docs\/|dod\/|deploy\/|xtask\/|Cargo\.toml)/, '').replace(/^\.\//, '')
const describe = fs => fs.map((f, i) => `${i + 1}. [${f.severity}, ${f.pattern}] ${f.title}
   Primary: ${f.file}:${f.line}; other files: ${(f.other_files || []).join(', ') || 'none'}
   ${f.description}
   Evidence: ${f.evidence}
   Proposed fix: ${f.fix}
   Test idea: ${f.test_idea || '-'}`).join('\n')

const reports = []

await pipeline(A.clusters || [],
  c => agent(`${CONTEXT}

You write the implementation contract for one cluster of verified findings. Read every file involved and the callers you need; do not edit anything.
Cluster ${c.id}. Files: ${c.files.map(norm).join(', ')}
Findings:
${describe(c.findings)}

Write a contract that one coder per file can follow without talking to the others:
- For each file that must change: the exact edits (new/changed signatures verbatim, behaviour, error variants, doc comments to update) and the tests to add in that file (TestResult, no unwrap/expect/panic).
- Keep public APIs stable unless a finding requires otherwise; if a signature changes, list every caller file in this cluster and its edit. A caller outside the cluster must not break: if one would, set feasible=false and name it.
- Fail closed, keep diffs small, respect the ring rules in xtask/arch-policy.toml, no new dependencies unless unavoidable (then say so under decisions_needed).
- If a finding needs a product decision (behaviour change users would notice, a new config key, a security trade-off), set feasible=false and list it.`,
    { label: `contract:${c.id}`, phase: 'Contract', schema: CONTRACT, model: 'opus', agentType: 'focused-explorer' }),
  async (contract, c) => {
    if (!contract) { reports.push({ id: c.id, status: 'no-contract' }); return null }
    if (!contract.feasible) { reports.push({ id: c.id, status: 'needs-decision', decisions: contract.decisions_needed, summary: contract.summary }); return null }
    const codedAll = await parallel(contract.files.map(fp => () => agent(`${CONTEXT}

You are a focused coder. Edit ONLY this one file: ${norm(fp.file)}. Use the Edit tool (no temp files). Never commit. Other coders edit the other files of this contract at the same time; do not touch them.
Contract for cluster ${c.id}: ${contract.summary}
Your part:
${fp.instructions}
Tests to add in this file: ${fp.tests.join('; ') || 'none'}
Other files in this contract and what they do: ${contract.files.filter(o => o.file !== fp.file).map(o => `${norm(o.file)}: ${o.instructions.slice(0, 300)}`).join(' | ') || 'none'}
Update every doc/comment in this file that describes the changed behaviour. If the contract cannot be followed exactly, do the closest safe thing and list it under deviations.`,
      { label: `code:${norm(fp.file)}`, phase: 'Code', schema: CODED, agentType: 'focused-coder' })))
    const coded = codedAll.filter(Boolean)
    // One coder result per contracted file, or the cluster is not done.
    const missingCoders = contract.files.filter((fp, i) => !codedAll[i]).map(fp => norm(fp.file))
    const review = await agent(`${CONTEXT}

You review one contract wave cluster. Read \`${GIT} diff -- ${contract.files.map(f => norm(f.file)).join(' ')}\` and the contract below. Do not edit.
Contract ${c.id}: ${contract.summary}
${contract.files.map(f => `- ${norm(f.file)}: ${f.instructions}`).join('\n')}
Coder reports: ${JSON.stringify(coded)}
Findings the contract must fix:
${describe(c.findings)}
Check: every finding is really fixed; the files agree with each other (signatures, callers, error variants); callers outside the cluster still compile (grep); binding rules hold (no let-chains, no unwrap/expect/panic in tests, TestResult); ring rules hold; docs match; no dead code clippy -D warnings would reject; no new pub item without a production caller. ok=false only with concrete problems, each tied to one file.`,
      { label: `review:${c.id}`, phase: 'Review', schema: REVIEW, model: 'opus', agentType: 'focused-explorer' })
    const problems = (review && review.problems) || []
    const byFile = {}
    for (const p of problems) (byFile[norm(p.file)] = byFile[norm(p.file)] || []).push(p.problem)
    const repairTargets = Object.entries(byFile)
    const repairsAll = await parallel(repairTargets.map(([file, ps]) => () => agent(`${CONTEXT}

You are a repair coder. Edit ONLY this one file: ${file} (Edit tool, never commit). The cluster reviewer found:
- ${ps.join('\n- ')}
Contract ${c.id}: ${contract.summary}
Fix these minimally. If a problem needs another file, report it under deviations instead.`,
      { label: `repair:${file}`, phase: 'Review', schema: CODED, agentType: 'focused-coder' })))
    const repairs = repairsAll.filter(Boolean)
    const missingRepairs = repairTargets.filter((rt, i) => !repairsAll[i]).map(([file]) => file)
    const base = { id: c.id, summary: contract.summary, files: contract.files.map(f => norm(f.file)), coded, problems, repairs }
    // Fail closed: a missing coder, review or repair never reads as done, and a
    // repaired cluster counts only after a second review of the whole diff.
    if (missingCoders.length) { reports.push({ ...base, status: 'unresolved', reason: `no coder result for ${missingCoders.join(', ')}` }); return null }
    if (!review) { reports.push({ ...base, status: 'unverified', reason: 'no cluster review' }); return null }
    if (review.ok) { reports.push({ ...base, status: 'ok' }); return null }
    if (!problems.length) { reports.push({ ...base, status: 'unverified', reason: 'review returned ok=false without naming a problem' }); return null }
    if (missingRepairs.length) { reports.push({ ...base, status: 'unresolved', reason: `no repair result for ${missingRepairs.join(', ')}` }); return null }
    const recheck = await agent(`${CONTEXT}

You re-review one contract wave cluster after its repair pass. Read \`${GIT} diff -- ${contract.files.map(f => norm(f.file)).join(' ')}\`. Do not edit.
Contract ${c.id}: ${contract.summary}
${contract.files.map(f => `- ${norm(f.file)}: ${f.instructions}`).join('\n')}
The first review found:
${problems.map(p => `- ${norm(p.file)}: ${p.problem}`).join('\n')}
Repair reports: ${JSON.stringify(repairs)}
Check that every problem is resolved, that the files still agree with each other and with the contract, and that the repairs introduced nothing new. ok=false only with concrete problems, each tied to one file.`,
      { label: `rereview:${c.id}`, phase: 'Review', schema: REVIEW, model: 'opus', agentType: 'focused-explorer' })
    if (!recheck) { reports.push({ ...base, status: 'unresolved', reason: 'no re-review after repair' }); return null }
    reports.push({ ...base, status: recheck.ok ? 'repaired' : 'unresolved', rereview: recheck, reason: recheck.ok ? undefined : 're-review still finds problems' })
    return null
  },
)

const done = reports.filter(r => r.status === 'ok' || r.status === 'repaired')
const clusterIds = (A.clusters || []).map(c => c.id)
const missing = clusterIds.filter(id => !reports.some(r => r.id === id))
const complete = missing.length === 0 && done.length === clusterIds.length
log(`${A.key}: ${complete ? 'complete' : 'NOT complete'}; ${done.length}/${clusterIds.length} clusters done, ${reports.filter(r => r.status === 'needs-decision').length} need a decision${missing.length ? `, no report for ${missing.join(', ')}` : ''}`)
return { key: A.key, complete, missing, reports }
