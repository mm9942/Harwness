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
//   root      absolute path of the checkout (a git worktree on its own branch); required (catalog P14)
//   clusters  [{id, files: [repo-relative paths], findings: [verified findings]}], file-disjoint
//   catalog   pattern catalog path (default docs/planning/85-gap-hunt/patterns.md)
//   rules     binding code rules (default: the workspace rules below)
const A = args || {}
const CATALOG = A.catalog || 'docs/planning/85-gap-hunt/patterns.md'
const RULES = A.rules || 'no let-chains (`if let … && …`, MSRV 1.85), forbid(unsafe), no unwrap/expect/panic! in library code OR tests/doctests (tests return TestResult and use the crate helpers), no third-party types in public APIs, hand-written error types, match the file\'s comment language and density, no book titles/authors/quotes anywhere; ring rules in xtask/arch-policy.toml (a crate may depend only on the rings its ring allows)'
const BUILD_RULE = 'Subagents and parallel agents must **never** run `cargo` or `rustc` in any form: no `check`, `build`, `test`, `nextest`, `clippy`, `fmt`, `run`, `doc`, `deny`, and no `make` target that calls them. They only read and edit code. At the end they report which tests they added and which commands the central build must run.'
// root is required (catalog P14): the session's working directory follows a
// `cd` of the main session, so "the current directory" is not a safe default.
// It also ends up in shell snippets inside prompts, so it must be a plain
// absolute path (letters, digits, `._/-`, no `..` segment), and every snippet
// single-quotes it.
function checkRoot(root) {
  const r = String(root || '').replace(/\/+$/, '')
  if (!r) throw new Error('root is required: pass the absolute path of the checkout (catalog P14)')
  if (!/^\/[A-Za-z0-9._\/-]+$/.test(r) || r.split('/').includes('..')) {
    throw new Error('root must be a plain absolute path of [A-Za-z0-9._/-] without ".." segments')
  }
  return r
}
const ROOT = checkRoot(A.root)
const GIT = `git -C '${ROOT}'`
const WHERE = `Repository root: ${ROOT} (a git checkout on its own branch). Every path below is relative to that root: read and edit files only under it, and run git as \`${GIT} …\`. Never touch the same path in any other checkout.`
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
          change: { type: 'boolean', description: 'false if this declared file needs no edit; instructions then say why' },
          instructions: { type: 'string', description: 'exact edits for this file: signatures, behaviour, doc updates' },
          tests: { type: 'array', items: { type: 'string' } },
        },
        required: ['file', 'change', 'instructions', 'tests'],
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
    problems: { type: 'array', description: 'blocking problems only', items: { type: 'object', properties: { file: { type: 'string' }, problem: { type: 'string' } }, required: ['file', 'problem'] } },
    notes: { type: 'string', description: 'non-blocking observations; never put them into problems' },
  },
  required: ['ok', 'problems'],
}

// Paths inside ROOT become repo-relative; an absolute path into any other
// checkout stays absolute, so a write in the wrong tree shows up (catalog P14).
const norm = p => {
  const s = String(p || '').trim().replace(/^\.\//, '')
  return s.startsWith(ROOT + '/') ? s.slice(ROOT.length + 1) : s
}
const describe = fs => fs.map((f, i) => `${i + 1}. [${f.severity}, ${f.pattern}] ${f.title}
   Primary: ${f.file}:${f.line}; other files: ${(f.other_files || []).join(', ') || 'none'}
   ${f.description}
   Evidence: ${f.evidence}
   Proposed fix: ${f.fix}
   Test idea: ${f.test_idea || '-'}`).join('\n')

const reports = []

// The declared cluster (c.files) is the trusted input; the contract's file list
// is model output. Both are normalized; the contract must list every declared
// file exactly once and nothing else before any coder starts. Files that the
// findings name but the cluster does not declare are reported and fenced off.
const uniq = xs => [...new Set(xs)]
const declaredOf = c => uniq((c.files || []).map(norm))
const uncoveredOf = c => {
  const declared = declaredOf(c)
  return uniq((c.findings || []).flatMap(f => [f.file, ...(f.other_files || [])]).map(norm)).filter(f => f && !declared.includes(f))
}

await pipeline(A.clusters || [],
  c => agent(`${CONTEXT}

You write the implementation contract for one cluster of verified findings. Read every file involved and the callers you need; do not edit anything.
Cluster ${c.id}. Declared files (fixed set): ${declaredOf(c).join(', ')}
Findings:
${describe(c.findings)}

The file set is fixed: list every declared file exactly once in files, and no other file. For a declared file that needs no edit, set change=false and say why in its instructions.${uncoveredOf(c).length ? ` The findings also name files outside the cluster: ${uncoveredOf(c).join(', ')}. Plan no edit there; if a finding cannot be fixed inside the declared files, set feasible=false and say which file it needs.` : ''}
Write a contract that one coder per file can follow without talking to the others:
- For each file that must change: the exact edits (new/changed signatures verbatim, behaviour, error variants, doc comments to update) and the tests to add in that file (TestResult, no unwrap/expect/panic).
- Keep public APIs stable unless a finding requires otherwise; if a signature changes, list every caller file in this cluster and its edit. A caller outside the cluster must not break: if one would, set feasible=false and name it.
- Fail closed, keep diffs small, respect the ring rules in xtask/arch-policy.toml, no new dependencies unless unavoidable (then say so under decisions_needed).
- If a finding needs a product decision (behaviour change users would notice, a new config key, a security trade-off), set feasible=false and list it.`,
    { label: `contract:${c.id}`, phase: 'Contract', schema: CONTRACT, model: 'opus', agentType: 'focused-explorer' }),
  async (contract, c) => {
    if (!contract) { reports.push({ id: c.id, status: 'no-contract' }); return null }
    if (!contract.feasible) { reports.push({ id: c.id, status: 'needs-decision', decisions: contract.decisions_needed, summary: contract.summary, uncovered: uncoveredOf(c) }); return null }
    const declared = declaredOf(c)
    const contracted = (contract.files || []).map(f => norm(f.file))
    const duplicates = uniq(contracted.filter((f, i) => contracted.indexOf(f) !== i))
    const extra = uniq(contracted.filter(f => !declared.includes(f)))
    const omitted = declared.filter(f => !contracted.includes(f))
    if (duplicates.length || extra.length || omitted.length) {
      reports.push({ id: c.id, status: 'contract-mismatch', declared, duplicates, extra, omitted, summary: contract.summary })
      return null
    }
    const toCode = contract.files.filter(fp => fp.change !== false)
    const codedAll = await parallel(toCode.map(fp => () => agent(`${CONTEXT}

You are a focused coder. Edit ONLY this one file: ${norm(fp.file)}. Use the Edit tool (no temp files). Never commit. Other coders edit the other files of this contract at the same time; do not touch them.
Contract for cluster ${c.id}: ${contract.summary}
Your part:
${fp.instructions}
Tests to add in this file: ${fp.tests.join('; ') || 'none'}
Other files in this contract and what they do: ${contract.files.filter(o => o.file !== fp.file).map(o => `${norm(o.file)}: ${o.instructions.slice(0, 300)}`).join(' | ') || 'none'}
Update every doc/comment in this file that describes the changed behaviour. Code and docs never refer to your brief, the contract, a report, a wave or an agent (catalog P15). If the contract cannot be followed exactly, do the closest safe thing and list it under deviations.`,
      { label: `code:${norm(fp.file)}`, phase: 'Code', schema: CODED, agentType: 'focused-coder' })))
    const coded = codedAll.filter(Boolean)
    // One coder result per contracted file, or the cluster is not done.
    const missingCoders = toCode.filter((fp, i) => !codedAll[i]).map(fp => norm(fp.file))
    const review = await agent(`${CONTEXT}

You review one contract wave cluster. Read \`${GIT} diff -- ${declared.join(' ')}\` and the contract below. Do not edit.
Contract ${c.id}: ${contract.summary}
${contract.files.map(f => `- ${norm(f.file)}${f.change === false ? ' (no change)' : ''}: ${f.instructions}`).join('\n')}
Coder reports: ${JSON.stringify(coded)}
Findings the contract must fix:
${describe(c.findings)}
Check: every finding is really fixed; the files agree with each other (signatures, callers, error variants); callers outside the cluster still compile (grep); binding rules hold (no let-chains, no unwrap/expect/panic in tests, TestResult); ring rules hold; docs match; no dead code clippy -D warnings would reject; no new pub item without a production caller. ok=false only with concrete blocking problems, each tied to one file; non-blocking observations go into notes, never into problems.`,
      { label: `review:${c.id}`, phase: 'Review', schema: REVIEW, model: 'opus', agentType: 'focused-explorer' })
    const problems = (review && review.problems) || []
    // Only declared files are ever repaired (catalog P14): a problem in any
    // other file cannot be fixed inside this cluster, so it blocks the cluster
    // and is handed on as a follow-up instead of being edited here.
    const inside = problems.filter(p => declared.includes(norm(p.file)))
    const outside = problems.filter(p => !declared.includes(norm(p.file))).map(p => ({ file: norm(p.file), problem: p.problem }))
    const base = { id: c.id, summary: contract.summary, files: declared, unchanged: contract.files.filter(f => f.change === false).map(f => norm(f.file)), uncovered: uncoveredOf(c), coded, problems, outside, notes: review ? review.notes || '' : '' }
    // Fail closed: a missing coder, review or repair never reads as done, and a
    // repaired cluster counts only after a second review of the whole diff.
    // No repair runs after ok=true, whatever the review lists.
    if (missingCoders.length) { reports.push({ ...base, status: 'unresolved', reason: `no coder result for ${missingCoders.join(', ')}` }); return null }
    if (!review) { reports.push({ ...base, status: 'unverified', reason: 'no cluster review' }); return null }
    if (review.ok) { reports.push({ ...base, status: 'ok' }); return null }
    if (!problems.length) { reports.push({ ...base, status: 'unverified', reason: 'review returned ok=false without naming a problem' }); return null }
    if (outside.length) { reports.push({ ...base, status: 'unresolved', reason: `needs files outside the cluster: ${[...new Set(outside.map(o => o.file))].join(', ')}` }); return null }
    const byFile = {}
    for (const p of inside) (byFile[norm(p.file)] = byFile[norm(p.file)] || []).push(p.problem)
    const repairTargets = Object.entries(byFile)
    const repairsAll = await parallel(repairTargets.map(([file, ps]) => () => agent(`${CONTEXT}

You are a repair coder. Edit ONLY this one file: ${file} (Edit tool, never commit). The cluster reviewer found:
- ${ps.join('\n- ')}
Contract ${c.id}: ${contract.summary}
Fix these minimally. If a problem needs another file, report it under deviations instead.`,
      { label: `repair:${file}`, phase: 'Review', schema: CODED, agentType: 'focused-coder' })))
    const repairs = repairsAll.filter(Boolean)
    const missingRepairs = repairTargets.filter((rt, i) => !repairsAll[i]).map(([file]) => file)
    if (missingRepairs.length) { reports.push({ ...base, repairs, status: 'unresolved', reason: `no repair result for ${missingRepairs.join(', ')}` }); return null }
    const recheck = await agent(`${CONTEXT}

You re-review one contract wave cluster after its repair pass. Read \`${GIT} diff -- ${declared.join(' ')}\`. Do not edit.
Contract ${c.id}: ${contract.summary}
${contract.files.map(f => `- ${norm(f.file)}${f.change === false ? ' (no change)' : ''}: ${f.instructions}`).join('\n')}
The first review found:
${problems.map(p => `- ${norm(p.file)}: ${p.problem}`).join('\n')}
Repair reports: ${JSON.stringify(repairs)}
Check that every problem is resolved, that the files still agree with each other and with the contract, and that the repairs introduced nothing new. ok=false only with concrete problems, each tied to one file.`,
      { label: `rereview:${c.id}`, phase: 'Review', schema: REVIEW, model: 'opus', agentType: 'focused-explorer' })
    if (!recheck) { reports.push({ ...base, repairs, status: 'unresolved', reason: 'no re-review after repair' }); return null }
    reports.push({ ...base, repairs, status: recheck.ok ? 'repaired' : 'unresolved', rereview: recheck, reason: recheck.ok ? undefined : 're-review still finds problems' })
    return null
  },
)

const done = reports.filter(r => r.status === 'ok' || r.status === 'repaired')
const clusterIds = (A.clusters || []).map(c => c.id)
const missing = clusterIds.filter(id => !reports.some(r => r.id === id))
const complete = missing.length === 0 && done.length === clusterIds.length
log(`${A.key}: ${complete ? 'complete' : 'NOT complete'}; ${done.length}/${clusterIds.length} clusters done, ${reports.filter(r => r.status === 'needs-decision').length} need a decision${missing.length ? `, no report for ${missing.join(', ')}` : ''}`)
return { key: A.key, complete, missing, reports }
