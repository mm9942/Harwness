export const meta = {
  name: 'gap-verify',
  description: 'Verify already-found gap findings (tiered lenses, catalog P9/P10), optionally after a completeness critic; read-only, no builds',
  whenToUse: 'When findings exist but their verification is missing: verifiers died and the original run cannot be resumed, a critic did not run, or findings came from outside a gap-hunt-area run (field reports, reviews). A plain session-limit abort is better handled by resuming the original run.',
  phases: [
    { title: 'Critic', detail: 'optional Opus completeness critic for one area' },
    { title: 'Verify', detail: 'tiered: M3 intent-only, critical/high intent+scope(+exploit for M1), rest intent+reproduce; tie-breaker on split (sonnet)' },
  ],
}

// Args:
//   key       short id (labels, logs)
//   findings  findings to verify: {file, line, severity, category, pattern, title, description, evidence, fix, single_file, other_files?, test_idea?}
//   critic    optional {area, crates, exclude, known}: run a completeness critic for that area first;
//             `known` lists "file:line title" strings already reported, so the critic skips them
//   verify    'tiered' (default) or 'classic' (reproduce+intent for every finding)
//   catalog   pattern catalog path (default docs/planning/85-gap-hunt/patterns.md)
//   rules     binding code rules (default: the workspace rules below)
const A = args || {}
const CATALOG = A.catalog || 'docs/planning/85-gap-hunt/patterns.md'
const RULES = A.rules || 'no let-chains (`if let … && …`, MSRV 1.85), forbid(unsafe), no unwrap/expect/panic! in library code OR tests/doctests (tests return TestResult), no third-party types in public APIs, hand-written error types, match the file\'s comment language, no book titles/authors/quotes anywhere'
const BUILD_RULE = 'Subagents and parallel agents must **never** run `cargo` or `rustc` in any form: no `check`, `build`, `test`, `nextest`, `clippy`, `fmt`, `run`, `doc`, `deny`, and no `make` target that calls them. They only read and edit code. At the end they report which tests they added and which commands the central build must run.'

const C = A.critic || null
const CONTEXT = `Repository: the current working directory (Rust workspace). Read the current working tree; other processes may be editing some files concurrently, so re-read before concluding.
${C ? `Area: ${C.area}. Crates/paths: ${(C.crates || []).join(', ')}. Out of scope: ${(C.exclude || []).join(', ') || 'none'}.` : ''}
Binding code rules: ${RULES}. Design decisions: docs/planning/70-decisions; guides: docs/guides; ledger: docs/planning/90-migration-ledger.
Pattern catalog: ${CATALOG}. Tag every finding with a catalog id (M1…, P5, P8, …) or NEW:<short-name>.
READ-ONLY: do not edit, create or delete any file. ${BUILD_RULE}`

const FINDINGS = {
  type: 'object',
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          file: { type: 'string', description: 'repo-relative path' },
          line: { type: 'integer' },
          severity: { type: 'string', enum: ['critical', 'high', 'medium', 'low'] },
          category: { type: 'string' },
          pattern: { type: 'string', description: 'catalog id or NEW:<name>' },
          title: { type: 'string' },
          description: { type: 'string' },
          evidence: { type: 'string' },
          fix: { type: 'string' },
          single_file: { type: 'boolean' },
          other_files: { type: 'array', items: { type: 'string' } },
          test_idea: { type: 'string' },
        },
        required: ['file', 'line', 'severity', 'category', 'pattern', 'title', 'description', 'evidence', 'fix', 'single_file'],
      },
    },
  },
  required: ['findings'],
}
const VERDICT = {
  type: 'object',
  properties: { real: { type: 'boolean' }, fixable_in_one_file: { type: 'boolean' }, reason: { type: 'string' } },
  required: ['real', 'fixable_in_one_file', 'reason'],
}

// Same lenses and tiering as gap-hunt-area (catalog P9/P10).
const LENSES_VERIFY = [
  { key: 'reproduce', text: 'Correctness / does-it-reproduce: read the cited code yourself on the current working tree. Default to real=false if the evidence does not hold.' },
  { key: 'intent', text: 'Intent: is this deliberate/documented (decisions, guides, comments) or handled elsewhere (grep callers and other layers)? If so real=false. For doc drift (M3) confirm cheaply by grepping the cited text and the code it describes.' },
  { key: 'scope', text: 'Fix scope and risk: genuine defect worth fixing now (not style noise), and fixable by editing ONLY the primary file without breaking callers elsewhere? fixable_in_one_file=false if other files must change.' },
  { key: 'exploit', text: 'Exploitability (adversarial): name a concrete input, caller or config that an untrusted party controls and that reaches the flaw. If no untrusted party can reach it, real=false and say "defence in depth" in the reason.' },
]
const lens = k => LENSES_VERIFY.find(l => l.key === k)
const plan = f => {
  if (A.verify === 'classic') return [['reproduce', 'intent'], 'scope']
  if (String(f.pattern || '').startsWith('M3')) return [['intent'], 'reproduce']
  if (f.severity === 'critical' || f.severity === 'high') {
    return [String(f.pattern || '').startsWith('M1') ? ['intent', 'scope', 'exploit'] : ['intent', 'scope'], 'reproduce']
  }
  return [['intent', 'reproduce'], 'scope']
}

const norm = p => String(p || '').replace(/^\/.*?\/(?=[^/]+\/src\/|docs\/|dod\/|deploy\/|xtask\/|Cargo\.toml)/, '').replace(/^\.\//, '')
const confirmed = []
const rejected = []
const unverified = []

const verifyOne = (f, l) => agent(`${CONTEXT}

You are an adversarial verifier. Try to REFUTE this finding. Lens: ${l.text}

Finding (${f.severity}, ${f.pattern}, ${f.category}): ${f.title}
Primary file: ${f.file}:${f.line}
Description: ${f.description}
Evidence: ${f.evidence}
Proposed fix: ${f.fix}
single_file: ${f.single_file}; other files: ${(f.other_files || []).join(', ') || 'none'}`,
  { label: `verify:${l.key}:${f.file.split('/').pop()}:${f.line}`, phase: 'Verify', schema: VERDICT, model: 'sonnet', agentType: 'focused-explorer' })

const judge = async f => {
  const [firstKeys, tieKey] = plan(f)
  const run = k => verifyOne(f, lens(k)).then(v => (v ? { ...v, lens: k } : null))
  const first = (await parallel(firstKeys.map(k => () => run(k)))).filter(Boolean)
  let votes = first
  const yes = key => first.filter(v => v[key]).length
  const tied = key => 2 * yes(key) === first.length
  if (first.length < firstKeys.length || tied('real') || (2 * yes('real') > first.length && tied('fixable_in_one_file'))) {
    const tie = await run(tieKey)
    votes = [...first, tie].filter(Boolean)
  }
  // Fixed quorum: as many answered lenses as were planned (the tie-breaker may
  // stand in for a lens that died). Fewer answers never decide a finding in
  // either direction; it stays unverified and can be retried.
  if (votes.length < firstKeys.length) {
    unverified.push({ ...f, votes: votes.map(v => ({ lens: v.lens, real: v.real })), reason: `${votes.length} of ${firstKeys.length} planned verdicts` })
    return
  }
  const need = Math.floor(votes.length / 2) + 1
  const real = votes.filter(v => v.real).length >= need
  const oneFile = Boolean(f.single_file) && votes.filter(v => v.fixable_in_one_file).length >= need
  const tally = votes.map(v => ({ lens: v.lens, real: v.real }))
  if (real) confirmed.push({ ...f, oneFile, votes: tally, reasons: votes.map(v => v.reason) })
  else rejected.push({ file: f.file, line: f.line, title: f.title, votes: tally, reasons: votes.map(v => v.reason) })
}

const input = (A.findings || []).map(f => ({ ...f, file: norm(f.file) }))

if (C) {
  phase('Critic')
  const known = [...(C.known || []), ...input.map(f => `${f.file}:${f.line} ${f.title}`)]
  const critic = await agent(`${CONTEXT}

You are the completeness critic for this area. Already reported (do not repeat):
- ${known.join('\n- ') || '(nothing)'}
Ask: which crates/files/modules were NOT examined, which risk classes were not checked (concurrency, error paths, input limits, auth, persistence/recovery, docs drift), and look there. Report only real, evidenced new defects.`,
    { label: `critic:${A.key}`, phase: 'Critic', schema: FINDINGS, model: 'opus', agentType: 'focused-explorer' })
  if (critic) for (const f of critic.findings || []) input.push({ ...f, file: norm(f.file), fromCritic: true })
  else log(`${A.key}: critic did not answer`)
}

phase('Verify')
await parallel(input.map(f => () => judge(f)))

log(`${A.key}: ${confirmed.length} confirmed (${confirmed.filter(c => c.oneFile).length} single-file), ${rejected.length} rejected, ${unverified.length} unverified`)
return { key: A.key, confirmed, rejected, unverified }
