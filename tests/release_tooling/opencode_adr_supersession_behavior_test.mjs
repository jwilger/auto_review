import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createRequire } from "node:module";

const root = path.resolve(import.meta.dirname, "../..");
const pluginPath = path.join(root, ".opencode/plugins/auto-review-discipline.ts");
const require = createRequire(import.meta.url);

function chain() {
  const schema = { metadata: {} };
  for (const method of ["min", "int", "nonnegative", "positive"]) {
    schema[method] = (...args) => {
      schema.metadata[method] = args;
      return schema;
    };
  }
  schema.describe = (description) => {
    schema.description = description;
    return schema;
  };
  schema.optional = () => {
    schema.optional = true;
    return schema;
  };
  return schema;
}

function stringSchema() {
  const schema = chain();
  schema.kind = "string";
  return schema;
}

function numberSchema() {
  const schema = chain();
  schema.kind = "number";
  return schema;
}

function objectSchema(fields = {}) {
  const schema = chain();
  schema.kind = "object";
  schema.fields = fields;
  return schema;
}

function arraySchema(item) {
  const schema = chain();
  schema.kind = "array";
  schema.item = item;
  return schema;
}

function fakeTool(definition) {
  return definition;
}
fakeTool.schema = {
  string: stringSchema,
  number: numberSchema,
  object: objectSchema,
  array: arraySchema,
};

function loadDisciplinePlugin() {
  const source = fs
    .readFileSync(pluginPath, "utf8")
    .replace(/^import \{ tool, type Plugin \} from "@opencode-ai\/plugin";\n/m, "")
    .replace(/^import \{[^\n]+\} from "\.\/lib\/shared\.ts";\n/m, "")
    .replace(/^import (\w+) from "(node:[^"]+)";$/gm, 'const $1 = require("$2");')
    .replace(/export const AutoReviewDisciplinePlugin: Plugin = async \(([^)]*)\) =>/, "const AutoReviewDisciplinePlugin = async ($1) =>")
    .replace("export default AutoReviewDisciplinePlugin;", "")
    .replace(/function filePathFromArgs\(args: unknown\): string \| undefined/, "function filePathFromArgs(args)")
    .replace(/function isEditTool\(toolID: string\): boolean/, "function isEditTool(toolID)")
    .replace(/function changedPathsFromArgs\(args: unknown\): string\[\]/, "function changedPathsFromArgs(args)")
    .replace(/function rejectsWaterfallTodo\(args: unknown\): boolean/, "function rejectsWaterfallTodo(args)")
    .replace(/function rejectsBroadDiagnosticTask\(args: unknown\): boolean/, "function rejectsBroadDiagnosticTask(args)")
    .replace(/ as Record<string, unknown>/g, "");

  return Function(
    "tool",
    "require",
    "getCycle",
    "isNonBehavioralPath",
    "isProductionRustPath",
    "isLikelyTestPath",
    "recordTouchedFile",
    "setCycle",
    "clearCycle",
    "recordVerification",
    "sessionContext",
    `${source}\nreturn AutoReviewDisciplinePlugin;`,
  )(
    fakeTool,
    require,
    () => undefined,
    () => true,
    () => false,
    () => false,
    () => {},
    () => {},
    () => {},
    () => {},
    () => [],
  );
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function assertSupersedesSchema(toolDefinition, toolName) {
  const supersedes = toolDefinition.args?.supersedes;
  assert(supersedes, `${toolName}.args.supersedes is not exposed`);
  assert(supersedes.optional === true, `${toolName}.args.supersedes is not optional`);
  assert(supersedes.kind === "array", `${toolName}.args.supersedes is not an array schema`);
  assert(supersedes.item?.kind === "object", `${toolName}.args.supersedes items are not object schemas`);
  assert(supersedes.item.fields?.path?.kind === "string", `${toolName}.args.supersedes item path is not a string field`);
  assert(supersedes.item.fields?.reason?.kind === "string", `${toolName}.args.supersedes item reason is not a string field`);
}

function acceptedAdr(title, decision) {
  return adrWithStatus(title, "Accepted", "Accepted context must stay stable.", decision, "Accepted consequences must stay stable.");
}

function adrWithStatus(title, status, context, decision, consequences) {
  return `# ${title}

## Status

${status}

## Date

2026-05-15

## Context

${context}

## Decision

${decision}

## Consequences

${consequences}
`;
}

async function assertRejectsNonAcceptedSupersedes(toolUnderTest, toolName, status, args, unchangedPaths, forbiddenArchitectureText) {
  const before = new Map(unchangedPaths.map((filePath) => [filePath, fs.readFileSync(filePath, "utf8")]));
  let rejected = false;
  try {
    await toolUnderTest.execute(args, { sessionID: "adr-supersession-test" });
  } catch (error) {
    rejected = true;
    const message = error instanceof Error ? error.message : String(error);
    assert(/accepted/i.test(message), `${toolName} ${status} supersedes rejection did not mention Accepted-only prior ADRs: ${message}`);
  }
  assert(rejected, `${toolName} accepted a ${status} ADR supersedes entry`);
  for (const [filePath, contents] of before) {
    assert(fs.readFileSync(filePath, "utf8") === contents, `${toolName} changed ${filePath} before rejecting ${status} supersedes entry`);
  }
  const architecture = fs.readFileSync(path.join("docs", "ARCHITECTURE.md"), "utf8");
  assert(!architecture.includes(forbiddenArchitectureText), `${toolName} applied architecture patch before rejecting ${status} supersedes entry`);
}

async function assertAcceptRevalidatesRecordedSupersedes(toolUnderTest, status, targetPath, proposedPath, architecturePath) {
  const before = new Map(
    [targetPath, proposedPath, architecturePath].map((filePath) => [filePath, fs.readFileSync(filePath, "utf8")]),
  );
  fs.writeFileSync(
    targetPath,
    adrWithStatus(
      `ADR-${path.basename(targetPath).slice(4, 8)}: Volatile Prior`,
      status,
      `${status} prior context.`,
      `${status} prior decision.`,
      `${status} prior consequences.`,
    ),
  );
  before.set(targetPath, fs.readFileSync(targetPath, "utf8"));

  let rejected = false;
  try {
    await toolUnderTest.execute({ path: path.join("docs", path.basename(proposedPath)) }, { sessionID: "adr-supersession-test" });
  } catch (error) {
    rejected = true;
    const message = error instanceof Error ? error.message : String(error);
    assert(/accepted/i.test(message), `adr_accept ${status} recorded supersedes rejection did not mention Accepted-only prior ADRs: ${message}`);
  }
  assert(rejected, `adr_accept accepted a Proposed ADR with a recorded supersedes target that is now ${status}`);
  for (const [filePath, contents] of before) {
    assert(
      fs.readFileSync(filePath, "utf8") === contents,
      `adr_accept changed ${filePath} before rejecting recorded ${status} supersedes target`,
    );
  }
  const architecture = fs.readFileSync(architecturePath, "utf8");
  assert(
    architecture.includes("Before stale recorded supersedes projection."),
    `adr_accept applied architecture patch before rejecting recorded ${status} supersedes target`,
  );
  assert(
    !architecture.includes("After stale recorded supersedes projection."),
    `adr_accept left replacement text after rejecting recorded ${status} supersedes target`,
  );
}

function assertPriorAdrSupersededWithoutBodyRewrite(body, supersedingAdrId, preservedDecision, reason) {
  assert(/## Status\n\nSuperseded/.test(body), "prior Accepted ADR status was not changed to Superseded");
  assert(body.includes(supersedingAdrId), "prior Accepted ADR does not identify the superseding ADR");
  assert(body.includes("## Superseded By"), "prior Accepted ADR did not gain a Superseded By section");
  assert(body.includes(reason), "prior Accepted ADR did not record the typed supersession reason");
  assert(body.includes("## Context\n\nAccepted context must stay stable."), "prior Accepted ADR Context section changed");
  assert(body.includes(`## Decision\n\n${preservedDecision}`), "prior Accepted ADR Decision section changed");
  assert(body.includes("## Consequences\n\nAccepted consequences must stay stable."), "prior Accepted ADR Consequences section changed");
}

const pluginFactory = loadDisciplinePlugin();
const plugin = await pluginFactory();
const adrCreate = plugin.tool?.adr_create;
const adrUpdate = plugin.tool?.adr_update;
const adrAccept = plugin.tool?.adr_accept;
assert(adrCreate?.execute, "adr_create tool is not registered");
assert(adrUpdate?.execute, "adr_update tool is not registered");
assert(adrAccept?.execute, "adr_accept tool is not registered");
assertSupersedesSchema(adrCreate, "adr_create");
assertSupersedesSchema(adrUpdate, "adr_update");

const workspace = fs.mkdtempSync(path.join(os.tmpdir(), "auto-review-adr-supersession-"));
fs.mkdirSync(path.join(workspace, "docs"));

const createPriorPath = path.join(workspace, "docs/ADR-0003-accepted-create-baseline.md");
const proposedPriorPath = path.join(workspace, "docs/ADR-0004-proposed-prior.md");
const updatePriorPath = path.join(workspace, "docs/ADR-0005-accepted-update-baseline.md");
const updateTargetPath = path.join(workspace, "docs/ADR-0006-proposed-replacement.md");
const rejectedPriorPath = path.join(workspace, "docs/ADR-0002-rejected-prior.md");
const volatilePriorPath = path.join(workspace, "docs/ADR-0008-volatile-prior.md");
const volatileReplacementPath = path.join(workspace, "docs/ADR-0009-volatile-replacement.md");
fs.writeFileSync(createPriorPath, acceptedAdr("ADR-0003: Accepted Create Baseline", "Create baseline decision must stay stable."));
fs.writeFileSync(
  proposedPriorPath,
  adrWithStatus("ADR-0004: Proposed Prior", "Proposed", "Proposed prior context.", "Proposed prior decision.", "Proposed prior consequences."),
);
fs.writeFileSync(updatePriorPath, acceptedAdr("ADR-0005: Accepted Update Baseline", "Update baseline decision must stay stable."));
fs.writeFileSync(
  updateTargetPath,
  `# ADR-0006: Proposed Replacement

## Status

Proposed

## Date

2026-05-15

## Context

Initial replacement context.

## Decision

Initial replacement decision.

## Consequences

Initial replacement consequences.
`,
);
fs.writeFileSync(
  rejectedPriorPath,
  adrWithStatus("ADR-0002: Rejected Prior", "Rejected", "Rejected prior context.", "Rejected prior decision.", "Rejected prior consequences."),
);
fs.writeFileSync(
  path.join(workspace, "docs/ARCHITECTURE.md"),
  "# Architecture\n\nBefore invalid create projection.\nBefore invalid update projection.\nBefore stale recorded supersedes projection.\nBefore create projection.\nBefore update projection.\n",
);

const previousCwd = process.cwd();
process.chdir(workspace);
try {
  await assertRejectsNonAcceptedSupersedes(
    adrCreate,
    "adr_create",
    "Proposed",
    {
      title: "Invalid Proposed Supersession",
      date: "2026-05-16",
      context: "This ADR should not be created when it supersedes a Proposed prior ADR.",
      decision: "Reject non-Accepted prior ADR supersession input.",
      consequences: "No files should change for invalid supersession metadata.",
      supersedes: [
        {
          path: "docs/ADR-0004-proposed-prior.md",
          reason: "Proposed ADRs are still under review and cannot be superseded.",
        },
      ],
      architecturePatch: {
        path: "docs/ARCHITECTURE.md",
        find: "Before invalid create projection.",
        replace: "Invalid create projection should not apply.",
      },
    },
    [proposedPriorPath],
    "Invalid create projection should not apply.",
  );

  await assertRejectsNonAcceptedSupersedes(
    adrUpdate,
    "adr_update",
    "Rejected",
    {
      path: "docs/ADR-0006-proposed-replacement.md",
      title: "Proposed Replacement",
      date: "2026-05-16",
      context: "Invalid update context should not be written.",
      decision: "Invalid update decision should not be written.",
      consequences: "Invalid update consequences should not be written.",
      sectionsToUpdate: ["decision"],
      supersedes: [
        {
          path: "docs/ADR-0002-rejected-prior.md",
          reason: "Rejected ADRs are not active accepted decisions and cannot be superseded.",
        },
      ],
      architecturePatch: {
        path: "docs/ARCHITECTURE.md",
        find: "Before invalid update projection.",
        replace: "Invalid update projection should not apply.",
      },
    },
    [rejectedPriorPath, updateTargetPath],
    "Invalid update projection should not apply.",
  );

  await adrCreate.execute(
    {
      title: "Superseding Create Decision",
      date: "2026-05-16",
      context: "A new accepted direction replaces the old create baseline.",
      decision: "Use the superseding create decision.",
      consequences: "Operators can follow the replacement chain.",
      supersedes: [
        {
          path: "docs/ADR-0003-accepted-create-baseline.md",
          reason: "The new create decision replaces the baseline ADR.",
        },
      ],
      architecturePatch: {
        path: "docs/ARCHITECTURE.md",
        find: "Before create projection.",
        replace: "After create projection.",
      },
    },
    { sessionID: "adr-supersession-test" },
  );

  const createdAdr = fs.readFileSync(path.join(workspace, "docs/ADR-0007-superseding-create-decision.md"), "utf8");
  assert(createdAdr.includes("## Supersedes"), "adr_create did not record typed supersedes metadata on the new ADR");
  assert(createdAdr.includes("ADR-0003"), "adr_create Supersedes section does not reference the prior ADR");
  assert(
    createdAdr.includes("The new create decision replaces the baseline ADR."),
    "adr_create Supersedes section did not preserve the typed supersession reason",
  );
  assert(
    fs.readFileSync(createPriorPath, "utf8").includes("## Status\n\nAccepted"),
    "adr_create superseded the prior Accepted ADR before the replacement ADR was accepted",
  );

  fs.writeFileSync(volatilePriorPath, acceptedAdr("ADR-0008: Volatile Prior", "Volatile prior accepted decision."));
  fs.writeFileSync(
    volatileReplacementPath,
    adrWithStatus(
      "ADR-0009: Volatile Replacement",
      "Proposed",
      "Volatile replacement context.",
      "Volatile replacement decision.",
      "Volatile replacement consequences.",
    ),
  );

  await adrUpdate.execute(
    {
      path: "docs/ADR-0006-proposed-replacement.md",
      title: "Proposed Replacement",
      date: "2026-05-16",
      context: "Updated replacement context.",
      decision: "Updated replacement decision.",
      consequences: "Updated replacement consequences.",
      sectionsToUpdate: ["decision"],
      supersedes: [
        {
          path: "docs/ADR-0005-accepted-update-baseline.md",
          reason: "The updated proposed replacement supersedes the accepted update baseline.",
        },
      ],
      architecturePatch: {
        path: "docs/ARCHITECTURE.md",
        find: "Before update projection.",
        replace: "After update projection.",
      },
    },
    { sessionID: "adr-supersession-test" },
  );

  const updatedAdr = fs.readFileSync(updateTargetPath, "utf8");
  assert(updatedAdr.includes("## Supersedes"), "adr_update did not record typed supersedes metadata on the updated ADR");
  assert(updatedAdr.includes("ADR-0005"), "adr_update Supersedes section does not reference the prior ADR");
  assert(
    updatedAdr.includes("The updated proposed replacement supersedes the accepted update baseline."),
    "adr_update Supersedes section did not preserve the typed supersession reason",
  );
  assert(
    fs.readFileSync(updatePriorPath, "utf8").includes("## Status\n\nAccepted"),
    "adr_update superseded the prior Accepted ADR before the replacement ADR was accepted",
  );

  await adrUpdate.execute(
    {
      path: "docs/ADR-0009-volatile-replacement.md",
      title: "Volatile Replacement",
      date: "2026-05-16",
      context: "Volatile replacement context remains proposed while the prior status changes.",
      decision: "Recorded supersedes intent must be revalidated when this ADR is accepted.",
      consequences: "Accepting must reject without applying the patch if the prior ADR is no longer Accepted.",
      sectionsToUpdate: ["decision"],
      supersedes: [
        {
          path: "docs/ADR-0008-volatile-prior.md",
          reason: "The volatile replacement supersedes the accepted volatile baseline only if it remains Accepted.",
        },
      ],
      architecturePatch: {
        path: "docs/ARCHITECTURE.md",
        find: "Before stale recorded supersedes projection.",
        replace: "After stale recorded supersedes projection.",
      },
    },
    { sessionID: "adr-supersession-test" },
  );

  await assertAcceptRevalidatesRecordedSupersedes(
    adrAccept,
    "Rejected",
    volatilePriorPath,
    volatileReplacementPath,
    path.join(workspace, "docs/ARCHITECTURE.md"),
  );

  await adrAccept.execute(
    { path: "docs/ADR-0007-superseding-create-decision.md" },
    { sessionID: "adr-supersession-test" },
  );
  assertPriorAdrSupersededWithoutBodyRewrite(
    fs.readFileSync(createPriorPath, "utf8"),
    "ADR-0007",
    "Create baseline decision must stay stable.",
    "The new create decision replaces the baseline ADR.",
  );

  await adrAccept.execute(
    { path: "docs/ADR-0006-proposed-replacement.md" },
    { sessionID: "adr-supersession-test" },
  );
  assertPriorAdrSupersededWithoutBodyRewrite(
    fs.readFileSync(updatePriorPath, "utf8"),
    "ADR-0006",
    "Update baseline decision must stay stable.",
    "The updated proposed replacement supersedes the accepted update baseline.",
  );
} finally {
  process.chdir(previousCwd);
  fs.rmSync(workspace, { recursive: true, force: true });
}

console.log("opencode adr supersession behavior test passed");
