export const meta = {
  name: 'factory-upkeep',
  description: 'Scan for upkeep work and draft tracker items for the factory',
  phases: [{ title: 'Scan' }],
}

// args: {scans: factory.yaml loops.upkeep.scans, existing: [open item titles]}
// Scans run as the built-in read-only Explore agent. The orchestrator files the returned items.
const ITEMS = { type: 'object', properties: { items: { type: 'array', items: { type: 'object', properties: {
  title: { type: 'string' }, body: { type: 'string' }, class: { type: 'string' } }, required: ['title', 'body'] } } },
  required: ['items'] }

phase('Scan')
const a = typeof args === 'string' ? JSON.parse(args) : args
const found = await parallel(a.scans.map((scan) => () => agent(
  `Scan this repository for ${scan} upkeep. Read-only. Return small, independent items, each with ` +
  `acceptance criteria in the body. Skip anything already covered by: ${JSON.stringify(a.existing)}`,
  { phase: 'Scan', label: `scan:${scan}`, agentType: 'Explore', schema: ITEMS, effort: 'low' })))

const items = found.filter(Boolean).flatMap((r) => r.items)
const failed = a.scans.filter((_, i) => !found[i])
log(`${items.length} candidate items${failed.length ? `; failed scans: ${failed.join(', ')}` : ''}`)
return { items, failed }
