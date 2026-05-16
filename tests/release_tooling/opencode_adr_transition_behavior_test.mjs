import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createRequire } from "node:module";

const root = path.resolve(import.meta.dirname, "../..");
const pluginPath = path.join(root, ".opencode/plugins/auto-review-discipline.ts");
const require = createRequire(import.meta.url);

function chain() {
  const schema = {};
  for (const method of ["describe", "min", "int", "nonnegative", "positive", "optional"]) {
    schema[method] = () => schema;
  }
  return schema;
}

function fakeTool(definition) {
  return definition;
}
fakeTool.schema = {
  string: chain,
  number: chain,
  object: () => ({ ...chain(), fields: () => chain() }),
  array: () => chain(),
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

function proposedAdr(title, status = "Proposed", proposedPatch = "") {
  return `# ${title}

## Status

${status}

## Date

2026-05-16

## Context

Original context must stay stable.

## Decision

Original decision must stay stable.

## Consequences

Original consequences must stay stable.
${proposedPatch}
`;
}

function replaceStatusOnly(body, status) {
  return body.replace("## Status\n\nProposed", `## Status\n\n${status}`);
}

async function assertRejectsNonProposedWithoutSideEffects(toolUnderTest, args, filePath, expectedMessage) {
  const before = fs.readFileSync(filePath, "utf8");
  try {
    await toolUnderTest.execute(args, { sessionID: "adr-transition-test" });
    throw new Error("tool accepted a non-Proposed ADR transition");
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    assert(/proposed/i.test(message), `${expectedMessage}: ${message}`);
  }
  const after = fs.readFileSync(filePath, "utf8");
  assert(after === before, "non-Proposed ADR transition changed the ADR file before rejecting");
}

const pluginFactory = loadDisciplinePlugin();
const plugin = await pluginFactory();
const adrUpdate = plugin.tool?.adr_update;
const adrAccept = plugin.tool?.adr_accept;
const adrReject = plugin.tool?.adr_reject;
assert(adrUpdate?.execute, "adr_update tool is not registered");
assert(adrAccept?.execute, "adr_accept tool is not registered");
assert(adrReject?.execute, "adr_reject tool is not registered");
assert(adrAccept !== adrReject, "adr_accept and adr_reject must be registered as separate tools");

const workspace = fs.mkdtempSync(path.join(os.tmpdir(), "auto-review-adr-transition-"));
fs.mkdirSync(path.join(workspace, "docs"));

const acceptPath = path.join(workspace, "docs/ADR-0009-accept-me.md");
const rejectPath = path.join(workspace, "docs/ADR-0010-reject-me.md");
const acceptedPath = path.join(workspace, "docs/ADR-0011-already-accepted.md");
const rejectedPath = path.join(workspace, "docs/ADR-0012-already-rejected.md");
const emptyRationalePath = path.join(workspace, "docs/ADR-0013-empty-rationale.md");
const updateThenAcceptPath = path.join(workspace, "docs/ADR-0015-update-then-accept.md");
const headingPatchPath = path.join(workspace, "docs/ADR-0016-heading-patch.md");

const proposedAccept = proposedAdr("ADR-0009: Accept Me");
const proposedPatchAccept = proposedAdr(
  "ADR-0014: Accept Architecture Patch",
  "Proposed",
  `
## Proposed Architecture Patch

Path: docs/ARCHITECTURE.md
Find:
Architecture before accept.
Replace:
Architecture after accept.
`,
);
const proposedReject = proposedAdr("ADR-0010: Reject Me");
fs.writeFileSync(acceptPath, proposedAccept);
const acceptPatchPath = path.join(workspace, "docs/ADR-0014-accept-architecture-patch.md");
fs.writeFileSync(acceptPatchPath, proposedPatchAccept);
fs.writeFileSync(rejectPath, proposedReject);
fs.writeFileSync(acceptedPath, proposedAdr("ADR-0011: Already Accepted", "Accepted"));
fs.writeFileSync(rejectedPath, proposedAdr("ADR-0012: Already Rejected", "Rejected"));
fs.writeFileSync(emptyRationalePath, proposedAdr("ADR-0013: Empty Rationale"));
fs.writeFileSync(updateThenAcceptPath, proposedAdr("ADR-0015: Update Then Accept"));
fs.writeFileSync(headingPatchPath, proposedAdr("ADR-0016: Heading Patch"));
fs.writeFileSync(path.join(workspace, "docs/ARCHITECTURE.md"), "# Architecture\n\nArchitecture before accept.\n");

const previousCwd = process.cwd();
process.chdir(workspace);
try {
  await adrAccept.execute({ path: "docs/ADR-0009-accept-me.md" }, { sessionID: "adr-transition-test" });
  const acceptedAdr = fs.readFileSync(acceptPath, "utf8");
  assert(acceptedAdr === replaceStatusOnly(proposedAccept, "Accepted"), "adr_accept changed more than the Status section");

  await adrAccept.execute({ path: "docs/ADR-0014-accept-architecture-patch.md" }, { sessionID: "adr-transition-test" });
  const acceptedPatchAdr = fs.readFileSync(acceptPatchPath, "utf8");
  assert(acceptedPatchAdr.includes("## Status\n\nAccepted"), "adr_accept did not mark the architecture-patch ADR as Accepted");
  assert(!acceptedPatchAdr.includes("## Proposed Architecture Patch"), "adr_accept did not remove the proposed architecture patch section");
  const acceptedArchitecture = fs.readFileSync(path.join(workspace, "docs/ARCHITECTURE.md"), "utf8");
  assert(acceptedArchitecture.includes("Architecture after accept."), "adr_accept did not apply the stored proposed architecture patch");
  assert(!acceptedArchitecture.includes("Architecture before accept."), "adr_accept left the replaced architecture text in place");

  fs.writeFileSync(path.join(workspace, "docs/ARCHITECTURE.md"), "# Architecture\n\nArchitecture before update then accept.\n");
  await adrUpdate.execute(
    {
      path: "docs/ADR-0015-update-then-accept.md",
      title: "Update Then Accept",
      date: "2026-05-16",
      context: "Updated context for update-then-accept.",
      decision: "Updated decision for update-then-accept.",
      consequences: "Updated consequences for update-then-accept.",
      sectionsToUpdate: ["decision"],
      architecturePatch: {
        path: "docs/ARCHITECTURE.md",
        find: "Architecture before update then accept.",
        replace: "Architecture after update then accept.",
      },
    },
    { sessionID: "adr-transition-test" },
  );
  await adrAccept.execute({ path: "docs/ADR-0015-update-then-accept.md" }, { sessionID: "adr-transition-test" });
  const updateThenAcceptArchitecture = fs.readFileSync(path.join(workspace, "docs/ARCHITECTURE.md"), "utf8");
  assert(
    updateThenAcceptArchitecture.includes("Architecture after update then accept."),
    "adr_accept did not apply an architecture patch appended by adr_update to a Proposed ADR without an existing patch section",
  );
  const updateThenAcceptedAdr = fs.readFileSync(updateThenAcceptPath, "utf8");
  assert(!updateThenAcceptedAdr.includes("## Proposed Architecture Patch"), "adr_accept did not remove the patch section appended by adr_update");

  const headingFind = `# Architecture

Architecture introduction before heading patch.

## ADR event stream

The event stream projection still uses a draft-only shape.

## Runtime review pipeline

Runtime pipeline details must remain after accepting the ADR.
`;
  const headingReplace = `# Architecture

Architecture introduction after heading patch.

## ADR event stream

The ADR event stream projection uses accepted ADR records.

## Runtime review pipeline

Runtime pipeline details must remain after accepting the ADR.
`;
  fs.writeFileSync(path.join(workspace, "docs/ARCHITECTURE.md"), headingFind);
  await adrUpdate.execute(
    {
      path: "docs/ADR-0016-heading-patch.md",
      title: "Heading Patch",
      date: "2026-05-16",
      context: "Updated context for heading patch.",
      decision: "Updated decision for heading patch.",
      consequences: "Updated consequences for heading patch.",
      sectionsToUpdate: ["decision"],
      architecturePatch: {
        path: "docs/ARCHITECTURE.md",
        find: headingFind,
        replace: headingReplace,
      },
    },
    { sessionID: "adr-transition-test" },
  );
  await adrAccept.execute({ path: "docs/ADR-0016-heading-patch.md" }, { sessionID: "adr-transition-test" });
  const headingArchitecture = fs.readFileSync(path.join(workspace, "docs/ARCHITECTURE.md"), "utf8");
  assert(
    headingArchitecture === headingReplace,
    "adr_accept truncated a proposed architecture patch when Find/Replace contained markdown headings",
  );
  const headingAcceptedAdr = fs.readFileSync(headingPatchPath, "utf8");
  assert(!headingAcceptedAdr.includes("## Proposed Architecture Patch"), "adr_accept left the heading patch section in the ADR");
  assert(!headingAcceptedAdr.includes("Find:"), "adr_accept left Find fragments from the heading patch in the ADR");
  assert(!headingAcceptedAdr.includes("Replace:"), "adr_accept left Replace fragments from the heading patch in the ADR");

  try {
    await adrReject.execute({ path: "docs/ADR-0013-empty-rationale.md", rationale: "" }, { sessionID: "adr-transition-test" });
    throw new Error("adr_reject accepted an empty rejection rationale");
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    assert(/rationale/i.test(message), `empty-rationale rejection was not specific: ${message}`);
  }
  assert(
    fs.readFileSync(emptyRationalePath, "utf8") === proposedAdr("ADR-0013: Empty Rationale"),
    "empty-rationale rejection changed the Proposed ADR file before rejecting",
  );

  await adrReject.execute(
    {
      path: "docs/ADR-0010-reject-me.md",
      rationale: "The proposed projection conflicts with the accepted event stream architecture.",
    },
    { sessionID: "adr-transition-test" },
  );
  const rejectedAdr = fs.readFileSync(rejectPath, "utf8");
  assert(rejectedAdr.includes("## Status\n\nRejected"), "adr_reject did not mark the ADR as Rejected");
  assert(
    rejectedAdr.includes("The proposed projection conflicts with the accepted event stream architecture."),
    "adr_reject did not record the required rejection rationale",
  );
  assert(
    rejectedAdr.includes("## Context\n\nOriginal context must stay stable."),
    "adr_reject changed the Context section",
  );
  assert(
    rejectedAdr.includes("## Decision\n\nOriginal decision must stay stable."),
    "adr_reject changed the Decision section",
  );
  assert(
    rejectedAdr.includes("## Consequences\n\nOriginal consequences must stay stable."),
    "adr_reject changed the Consequences section",
  );

  await assertRejectsNonProposedWithoutSideEffects(
    adrAccept,
    { path: "docs/ADR-0011-already-accepted.md" },
    acceptedPath,
    "adr_accept non-Proposed rejection did not mention Proposed-only transitions",
  );
  await assertRejectsNonProposedWithoutSideEffects(
    adrReject,
    { path: "docs/ADR-0012-already-rejected.md", rationale: "Still not acceptable." },
    rejectedPath,
    "adr_reject non-Proposed rejection did not mention Proposed-only transitions",
  );
} finally {
  process.chdir(previousCwd);
  fs.rmSync(workspace, { recursive: true, force: true });
}

console.log("opencode adr transition behavior test passed");
