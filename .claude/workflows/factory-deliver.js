export const meta = {
  name: 'factory-deliver',
  description: 'Run one work item through spec, slice, build, review and approve',
  phases: [{ title: 'Spec' }, { title: 'Slice' }, { title: 'Build' }, { title: 'Review' }, { title: 'Approve' }],
}

// args: {item: {id, title, body, images?: [path]}, check, visual?, evidenceDir?, rounds, slice: bool, lenses: [..],
//        models: {spec, slice, build, review, approve}, efforts: {...},
//        holdout: {enabled: bool, threshold?: number, score?: number},
//        resume?: {from: 'build'|'approve', criteria: [..], decisions?: [..], round?, branch?, findings?: [..], notes?, paths?, evidence?, history?: [..]}}
// rounds is the total number of review rounds, so rounds: 2 means one build, one review,
// at most one rework, and a second review.
// Every return value has {stage, outcome, history}; generate.md lists the outcomes.
const VERDICT = (pair, extra, required = []) => ({
  type: 'object',
  properties: { verdict: { type: 'string', enum: pair }, ...extra },
  required: ['verdict', ...required],
})
const STRINGS = { type: 'array', items: { type: 'string' } }
// needs_human flags a finding only a person can resolve (a decision with no
// recommendable option, a conflicting requirement, missing access, or a policy
// change), so the orchestrator stops requeueing the item and asks a person.
const FINDINGS = { type: 'array', items: { type: 'object', properties: {
  file: { type: 'string' }, line: { type: 'number' }, severity: { type: 'string' }, summary: { type: 'string' },
  needs_human: { type: 'boolean' }, reason: { type: 'string' } },
  required: ['file', 'line', 'severity', 'summary'] } }
const OWNER = { type: 'array', items: { type: 'string', enum: ['engineer', 'design-lead', 'product-owner'] } }
// Open choices the spec writer settled by picking its recommendation. Every
// decision is listed in the change request body. needs_human (absent means
// false) marks a product, policy or security choice with no clearly
// recommendable option, with `reason` saying why; only those keep the change
// from auto-merge.
const DECISIONS = { type: 'array', items: { type: 'object', properties: {
  question: { type: 'string' }, chosen: { type: 'string' }, alternatives: STRINGS, why: { type: 'string' },
  needs_human: { type: 'boolean' }, reason: { type: 'string' } },
  required: ['question', 'chosen', 'alternatives', 'why'] } }
// A BLOCKED build says what blocks it, so the orchestrator can requeue work that
// waits on another item or a fixable environment problem and send only what a
// person must do to awaiting-human.
const BUILD = VERDICT(['DONE', 'BLOCKED'], { branch: { type: 'string' }, notes: { type: 'string' }, evidence: { type: 'string' },
  paths: STRINGS, ui: { type: 'boolean' },
  blocker: { type: 'string', enum: ['item', 'environment', 'human'] }, blocked_by: { type: 'string' },
  needs_human: { type: 'boolean' }, needs_human_reason: { type: 'string' },
  visual: { type: 'object', properties: { exit: { type: 'number' }, passed: { type: 'boolean' }, summary: { type: 'string' } },
    required: ['exit', 'passed'] },
  gate: { type: 'object', properties: { command: { type: 'string' }, passed: { type: 'boolean' }, summary: { type: 'string' } },
    required: ['command', 'passed'] } }, ['branch', 'gate', 'notes'])
// Large args can arrive as a JSON string, so accept both forms.
const a = typeof args === 'string' ? JSON.parse(args) : args
// Agents written during the current session are not registered until the next one, so the
// orchestrator passes inlineAgents: {agentType: agent body} on the first run after
// generation. Those stations run as general-purpose with the body prepended.
const station = (prompt, o) => a.inlineAgents?.[o.agentType]
  ? agent(`${a.inlineAgents[o.agentType]}\n\n---\n\n${prompt}`, { ...o, agentType: 'general-purpose' })
  : agent(prompt, o)
const opt = (station) => ({ phase: station[0].toUpperCase() + station.slice(1),
  model: a.models?.[station], effort: a.efforts?.[station] })
const itemText = `Item ${a.item.id}: ${a.item.title}\n\n${a.item.body}` +
  (a.item.images?.length ? `\n\nImages attached to the item (open them with Read):\n${a.item.images.map((i) => `- ${i}`).join('\n')}` : '')
const bullets = (xs) => `- ${xs.join('\n- ')}`
const itemBranch = `factory/${a.item.id}`
// Slice branches live outside the item branch's ref path, because git cannot
// hold `factory/<n>` and `factory/<n>/<slice>` at once (#190).
const sliceBranch = (id) => `factory-slice/${a.item.id}/${id}`
// Builders keep their memory out of item branches; a build that reports a path
// under it has not finished, whatever its gate says.
const memoryPaths = (b) => (b?.paths || []).filter((p) => p.startsWith('.claude/agent-memory/'))
// Each decision with everything the approver's brief repeats for a
// needs_human one: the chosen option, the alternatives, why, and the reason.
const decisionLines = (ds) => ds.map((d) =>
  `- ${d.question} -> ${d.chosen} (needs_human: ${d.needs_human === true}${d.reason ? `; ${d.reason}` : ''})\n` +
  `  alternatives: ${(d.alternatives || []).length ? d.alternatives.join('; ') : '(none)'}\n` +
  `  why: ${d.why || '(not given)'}`).join('\n')
// Every build starts from, or first merges, current origin/main, so a change
// is never verified against the main of an older day.
const freshBase = (branch, base) => base
  ? `After creating it, run \`git fetch origin\` and merge current origin/main into ${branch} with a merge commit before any other change.\n`
  : `Run \`git fetch origin\`. When ${branch} exists (locally or on origin), check it out, fetch it, and merge current origin/main ` +
    `into it with a merge commit before any other change. Otherwise create ${branch} from current origin/main, never from the local HEAD or an old main.\n`
// A visual gate that ran must pass too: new accessibility, token or console
// problems, or a renderer that broke, block the build like a red check. When
// the project has a visual gate and the builder says the change touches
// rendered UI, evidence is required, so a UI build cannot skip rendering.
// A build that does not say whether it touched UI counts as UI, so leaving
// out `ui` cannot skip the evidence. Slices of a multi-slice item are built
// without rendering (the integrate step renders the whole item), so they pass
// needVisual = false.
const built = (b, needVisual = true) => b && b.verdict === 'DONE' && b.gate?.passed && !memoryPaths(b).length
  && (!b.visual || b.visual.passed)
  && !(needVisual && a.visual && b.ui !== false && !b.visual)
// Continue the history of the run being resumed, so the item's metrics keep
// its build and review rounds after a holdout judge or a human decision.
const history = [...(a.resume?.history || [])]
let decisions = a.resume?.decisions || []
const done = (stage, outcome, extra = {}) => ({ stage, outcome, history, decisions, ...extra })
// What blocked a build, for the orchestrator's routing. A BLOCKED verdict that
// names no kind goes to a person, so an unexplained block is never retried
// silently. An item block that names no item becomes an environment block, so
// the orchestrator files the blocker as a new item and links it.
const blockerOf = (b) => {
  const kind = b.blocker ?? 'human'
  return kind === 'item' && !b.blocked_by ? { blocker: 'environment' } : { blocker: kind, blocked_by: b.blocked_by }
}
// A build that claimed DONE but left a gate red, a visual gate failed, or a UI
// change without evidence is unfinished work, not a blocker: the orchestrator
// requeues it at build with these findings, as it does an exhausted item.
const gateFindings = (b, needVisual) => [
  ...(memoryPaths(b).length ? [`agent memory on the branch: remove ${memoryPaths(b).join(', ')} from it`] : []),
  ...(!b.gate?.passed ? [`${b.gate?.command ?? 'check'}: ${b.gate?.summary ?? 'the gate did not pass'}`] : []),
  ...(b.visual && !b.visual.passed ? [`visual: ${b.visual.summary ?? `exit ${b.visual.exit}`}`] : []),
  ...(needVisual && a.visual && b.ui !== false && !b.visual ? ['visual: a UI change rendered no visual evidence'] : []),
].map((summary) => ({ file: '(gate)', line: 0, severity: 'blocking', summary }))
const unbuilt = (b, needVisual, extra) => b.verdict === 'BLOCKED'
  ? done('build', 'blocked', { ...extra, branch: b.branch, ...blockerOf(b) })
  : done('build', 'gate-failed', { ...extra, branch: b.branch, findings: gateFindings(b, needVisual),
    ...(b.needs_human ? { needs_human: true, needs_human_reason: b.needs_human_reason } : {}) })

const approveStation = async (branch, criteria, round, paths = [], evidence = null) => {
  phase('Approve')
  const score = a.holdout?.enabled ? ` Holdout satisfaction: ${a.holdout.score} (threshold ${a.holdout.threshold}).` : ''
  // ESCALATE means "human review required": the change request still opens, a person approves it, and reconcile then merges it.
  const approve = await station(`Decide whether the factory may auto-merge ${branch} for ${a.item.id} under factory/policy.md ` +
    `(APPROVE), or whether a human must review and approve it before the factory merges it (ESCALATE).\n` +
    `Criteria:\n${bullets(criteria)}\nReview rounds used: ${round} of ${a.rounds}.${score}\n` +
    `Spec decisions:\n${decisions.length ? decisionLines(decisions) : '- (none)'}\n` +
    `Changed paths:\n${paths.length ? bullets(paths) : '- (not reported)'}` +
    (evidence ? `\nVisual evidence: ${evidence}` : ''), {
    ...opt('approve'), agentType: 'factory-approver', label: 'approve',
    schema: VERDICT(['APPROVE', 'ESCALATE'], { reason: { type: 'string' }, brief: { type: 'string' }, owner: OWNER,
      classes: STRINGS,
      reviewers: { type: 'array', items: { type: 'object', properties: {
        role: { type: 'string' }, strength: { type: 'string', enum: ['required', 'recommended'] }, why: { type: 'string' } },
        required: ['role', 'strength', 'why'] } } }, ['reason', 'classes']),
  })
  if (!approve) return done('approve', 'agent-failed')
  // Fail closed: an approval that cannot say what changed, or which classes
  // that falls in, or that carries a decision only a person can make, needs
  // human review, so dark mode never merges it unclassified or with such a
  // choice unconfirmed. Other decisions go in the change request body and do
  // not block auto-merge.
  // A human-review verdict may truthfully have no classes, but it always needs
  // an owner to request review from, so one without falls to an engineer.
  const gap = approve.verdict !== 'APPROVE' ? null
    : !paths?.length ? 'changed paths not reported' : !approve.classes?.length ? 'no change class given'
    : decisions.some((d) => d.needs_human === true) ? 'decisions needing a person' : null
  const verdict = gap ? 'ESCALATE' : approve.verdict
  const decided = verdict === 'APPROVE' ? approve : { ...approve, verdict,
    reason: gap ? `${approve.reason} (human review required: ${gap})` : approve.reason,
    owner: approve.owner?.length ? approve.owner : ['engineer'] }
  return done('approve', decided.verdict === 'APPROVE' ? 'approved' : 'needs-review', {
    branch, criteria, approve: decided, owner: decided.owner, paths, evidence,
  })
}

// Resume at approve after the orchestrator has run the holdout judge.
if (a.resume?.from === 'approve') {
  return await approveStation(a.resume.branch, a.resume.criteria, a.resume.round ?? a.rounds, a.resume.paths, a.resume.evidence)
}

let criteria = a.resume?.criteria
if (!criteria) {
  phase('Spec')
  const spec = await station(`${itemText}\n\nWrite acceptance criteria.`, {
    ...opt('spec'), agentType: 'factory-spec-writer', label: 'spec',
    schema: VERDICT(['READY', 'ESCALATE'], { criteria: STRINGS, decisions: DECISIONS, brief: { type: 'string' }, owner: OWNER }),
  })
  if (!spec) return done('spec', 'agent-failed')
  // ESCALATE is reserved for a choice with no reasonable default; every other open choice arrives as decisions.
  if (spec.verdict !== 'READY' || !spec.criteria?.length) return done('spec', 'escalated', { brief: spec.brief, owner: spec.owner })
  criteria = spec.criteria
  decisions = spec.decisions || []
}

let slices = [{ id: 'all', criteria, independent: false }]
if (a.slice && !a.resume) {
  phase('Slice')
  const sliced = await station(`${itemText}\n\nCriteria:\n${bullets(criteria)}\n\nSplit into slices.`, {
    ...opt('slice'), agentType: 'factory-slicer', label: 'slice',
    schema: VERDICT(['SLICED', 'ESCALATE'], { brief: { type: 'string' }, owner: OWNER, slices: { type: 'array', items: { type: 'object', properties: {
      id: { type: 'string' }, criteria: STRINGS, independent: { type: 'boolean' } }, required: ['id', 'criteria'] } } }),
  })
  if (!sliced) return done('slice', 'agent-failed', { criteria })
  if (sliced.verdict !== 'SLICED' || !sliced.slices?.length) return done('slice', 'escalated', { criteria, brief: sliced.brief, owner: sliced.owner })
  slices = sliced.slices
}

const evidenceFor = (round, id) => a.evidenceDir ? `${a.evidenceDir}/${a.item.id}/r${round}-${id}` : null
// With several slices only the integrate step renders evidence, so slices never
// spend renders, or clash over ports, on screens the integrate step redraws.
const visualFor = (round, id) => a.visual
  ? `Visual evidence command: ${a.visual}\nWrite the evidence to: ${evidenceFor(round, id) ?? 'a new directory under the system temp directory'}\n`
  : ''
const buildSlice = (s, findings, round, branch, notes, base, render = true) => station(
  `${itemText}\n\nSlice ${s.id} criteria:\n${bullets(s.criteria)}\n\nCheck: ${a.check}\n` +
  (render ? visualFor(round, s.id) : '') +
  (base ? `Create ${branch} from ${base}, which holds the preceding slices, before changing anything.\n` : '') +
  freshBase(branch, base) +
  `Commit on branch ${branch}.\n` +
  (notes ? `\nHuman rework notes:\n${notes}\n` : '') +
  (findings.length ? `\nReview findings to address:\n${JSON.stringify(findings, null, 2)}` : ''), {
    ...opt('build'), agentType: 'factory-implementer', isolation: 'worktree', label: `build:${s.id} r${round}`, schema: BUILD,
  })

let findings = a.resume?.findings || []
let build = null
for (let round = 1; round <= a.rounds; round++) {
  phase('Build')
  if (round === 1 && slices.length > 1) {
    // Independent slices build in parallel worktrees. Dependent ones build in order, each
    // branched from the previous dependent slice so it sees that slice's changes.
    const parallelOut = await parallel(slices.filter((s) => s.independent).map((s) => () => buildSlice(s, [], round, sliceBranch(s.id), null, null, false)))
    const ordered = []
    for (const s of slices.filter((s) => !s.independent)) {
      const b = await buildSlice(s, [], round, sliceBranch(s.id), null, ordered.at(-1)?.branch, false)
      ordered.push(b)
      if (!built(b, false)) break
    }
    const results = [...parallelOut, ...ordered]
    history.push({ round, station: 'build', gates: results.map((r) => r?.gate ?? null) })
    // A slice agent that returned nothing failed; it did not report a block.
    if (results.some((r) => !r)) return done('build', 'agent-failed', { criteria, round, results })
    // The dependent loop stops at the first unbuilt slice, so a short list always holds one.
    const failed = results.find((r) => !built(r, false))
    if (failed) return unbuilt(failed, false, { criteria, round, results })
    build = await station(freshBase(itemBranch) +
      `Then merge the slice branches ${results.map((r) => r.branch).join(', ')} (each named factory-slice/${a.item.id}/<slice>) into ${itemBranch}, ` +
      `resolve conflicts against the criteria:\n${bullets(criteria)}\n\nThen run ${a.check}.` +
      (a.visual ? `\n${visualFor(round, 'integrate')}` : ''), {
      ...opt('build'), agentType: 'factory-implementer', isolation: 'worktree', label: 'integrate', schema: BUILD,
    })
  } else {
    build = await buildSlice({ id: 'all', criteria }, findings, round, itemBranch, round === 1 ? a.resume?.notes : null)
  }
  history.push({ round, station: 'build', gates: [build?.gate ?? null, ...(build?.visual ? [{ command: 'visual', ...build.visual }] : [])] })
  if (!build) return done('build', 'agent-failed', { criteria, round })
  if (!built(build)) return unbuilt(build, true, { criteria, round, build })

  phase('Review')
  const reviews = (await parallel(a.lenses.map((lens) => () => station(
    `Review branch ${build.branch} for ${a.item.id}.\nCriteria:\n${bullets(criteria)}` +
    (build.evidence ? `\nVisual evidence: ${build.evidence}` : ''), {
      ...opt('review'), agentType: `factory-reviewer-${lens}`, label: `review:${lens} r${round}`,
      schema: VERDICT(['PASS', 'CHANGES'], { findings: FINDINGS }, ['findings']),
    })))).filter(Boolean)
  if (reviews.length < a.lenses.length) return done('review', 'agent-failed', { criteria, round, branch: build.branch })
  findings = reviews.flatMap((r) => r.findings)
  // Per-lens verdicts and findings, so evolve can count rework from visual lenses.
  history.push({ round, station: 'review', verdicts: reviews.map((r) => r.verdict), findings: findings.length,
    lenses: reviews.map((r, i) => ({ lens: a.lenses[i], verdict: r.verdict, findings: r.findings.length })) })
  if (reviews.every((r) => r.verdict === 'PASS')) {
    // The judge needs the built change, so the orchestrator runs it and resumes at approve.
    if (a.holdout?.enabled) {
      return done('review', 'reviewed', { criteria, round, branch: build.branch, paths: build.paths, evidence: build.evidence })
    }
    return await approveStation(build.branch, criteria, round, build.paths, build.evidence)
  }
  log(`round ${round}: ${findings.length} findings`)
}
// The orchestrator requeues an exhausted item at build with these findings, and
// the next pick starts a fresh set of rounds.
return done('review', 'exhausted', { criteria, branch: build.branch, findings })
