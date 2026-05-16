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

const pluginFactory = loadDisciplinePlugin();
const plugin = await pluginFactory();
const adrUpdate = plugin.tool?.adr_update;
const adrCreate = plugin.tool?.adr_create;
assert(adrUpdate?.execute, "adr_update tool is not registered");
assert(adrCreate?.description, "adr_create tool is not registered");

const workspace = fs.mkdtempSync(path.join(os.tmpdir(), "auto-review-adr-update-"));
fs.mkdirSync(path.join(workspace, "docs"));

const proposedAdrPath = path.join(workspace, "docs/ADR-0007-proposed-tooling.md");
const proposedWithoutPatchPath = path.join(workspace, "docs/ADR-0009-proposed-without-patch.md");
fs.writeFileSync(
  proposedAdrPath,
  `# ADR-0007: Proposed Tooling

## Status

Proposed

## Date

2026-05-15

## Context

Original context stays stable.

## Decision

Original decision text.

## Consequences

Original consequences stay stable.

## Proposed Architecture Patch

Path: docs/ARCHITECTURE.md
Find:
Before previous ADR update projection.
Replace:
After previous ADR update projection.
`,
);
fs.writeFileSync(
  proposedWithoutPatchPath,
  `# ADR-0009: Proposed Without Patch

## Status

Proposed

## Date

2026-05-15

## Context

Original context without stored architecture patch.

## Decision

Original decision without stored architecture patch.

## Consequences

Original consequences without stored architecture patch.
`,
);

const acceptedAdrPath = path.join(workspace, "docs/ADR-0008-accepted-tooling.md");
fs.writeFileSync(
  acceptedAdrPath,
  `# ADR-0008: Accepted Tooling

## Status

Accepted

## Date

2026-05-15

## Context

Accepted context.

## Decision

Accepted decision.

## Consequences

Accepted consequences.
`,
);

fs.writeFileSync(path.join(workspace, "docs/ARCHITECTURE.md"), "# Architecture\n\nBefore ADR update projection.\n");

const previousCwd = process.cwd();
process.chdir(workspace);
try {
  await adrUpdate.execute(
    {
      path: "docs/ADR-0007-proposed-tooling.md",
      title: "Proposed Tooling",
      date: "2026-05-16",
      context: "Updated context for a still-proposed decision.",
      decision: "Updated decision requested by the ADR author.",
      consequences: "Updated consequences supplied but not requested.",
      sectionsToUpdate: ["date", "decision"],
      architecturePatch: {
        path: "docs/ARCHITECTURE.md",
        find: "Before ADR update projection.",
        replace: "After ADR update projection.",
      },
    },
    { sessionID: "adr-update-test" },
  );

  const proposedAdr = fs.readFileSync(proposedAdrPath, "utf8");
  assert(proposedAdr.includes("## Date\n\n2026-05-16"), "adr_update did not rewrite requested Date section");
  assert(
    proposedAdr.includes("## Decision\n\nUpdated decision requested by the ADR author."),
    "adr_update did not rewrite requested Decision section",
  );
  assert(
    proposedAdr.includes("## Context\n\nOriginal context stays stable."),
    "adr_update rewrote a non-requested Context section",
  );
  assert(
    proposedAdr.includes("## Consequences\n\nOriginal consequences stay stable."),
    "adr_update rewrote a non-requested Consequences section",
  );
  assert(proposedAdr.includes("## Proposed Architecture Patch"), "adr_update did not retain the proposed architecture patch section");
  assert(proposedAdr.includes("docs/ARCHITECTURE.md"), "adr_update proposed architecture patch omitted target path");
  assert(proposedAdr.includes("Before ADR update projection."), "adr_update did not rewrite proposed architecture patch find text");
  assert(proposedAdr.includes("After ADR update projection."), "adr_update did not rewrite proposed architecture patch replacement text");
  assert(
    !proposedAdr.includes("After previous ADR update projection."),
    "adr_update retained the stale proposed architecture patch replacement text",
  );

  const architecture = fs.readFileSync(path.join(workspace, "docs/ARCHITECTURE.md"), "utf8");
  assert(
    architecture === "# Architecture\n\nBefore ADR update projection.\n",
    "adr_update mutated docs/ARCHITECTURE.md while ADR status is still Proposed",
  );

  const acceptedAdrBeforeRejectedUpdate = fs.readFileSync(acceptedAdrPath, "utf8");
  try {
    await adrUpdate.execute(
      {
        path: "docs/ADR-0008-accepted-tooling.md",
        title: "Accepted Tooling",
        date: "2026-05-16",
        context: "Attempted accepted context update.",
        decision: "Attempted accepted decision update.",
        consequences: "Attempted accepted consequences update.",
        sectionsToUpdate: ["decision"],
        architecturePatch: {
          path: "docs/ARCHITECTURE.md",
          find: "Before ADR update projection.",
          replace: "Accepted update should not apply.",
        },
      },
      { sessionID: "adr-update-test" },
    );
    throw new Error("adr_update accepted an Accepted ADR update");
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    assert(/proposed/i.test(message), `Accepted ADR rejection did not mention Proposed-only updates: ${message}`);
  }
  const acceptedAdrAfterRejectedUpdate = fs.readFileSync(acceptedAdrPath, "utf8");
  assert(
    acceptedAdrAfterRejectedUpdate === acceptedAdrBeforeRejectedUpdate,
    "adr_update changed an Accepted ADR before rejecting the update",
  );
  const architectureAfterRejectedUpdate = fs.readFileSync(path.join(workspace, "docs/ARCHITECTURE.md"), "utf8");
  assert(
    !architectureAfterRejectedUpdate.includes("Accepted update should not apply."),
    "adr_update applied the paired architecture patch for a rejected Accepted ADR update",
  );

  await adrUpdate.execute(
    {
      path: "docs/ADR-0009-proposed-without-patch.md",
      title: "Proposed Without Patch",
      date: "2026-05-16",
      context: "Updated context without existing architecture patch.",
      decision: "Updated decision without existing architecture patch.",
      consequences: "Updated consequences without existing architecture patch.",
      sectionsToUpdate: ["decision"],
      architecturePatch: {
        path: "docs/ARCHITECTURE.md",
        find: "Before ADR update projection.",
        replace: "After appended ADR update projection.",
      },
    },
    { sessionID: "adr-update-test" },
  );
  const proposedWithoutPatch = fs.readFileSync(proposedWithoutPatchPath, "utf8");
  assert(
    proposedWithoutPatch.includes("## Proposed Architecture Patch"),
    "adr_update did not append a proposed architecture patch section when the Proposed ADR lacked one",
  );
  assert(
    proposedWithoutPatch.includes("After appended ADR update projection."),
    "adr_update did not store the appended proposed architecture patch replacement text",
  );
  assert(
    fs.readFileSync(path.join(workspace, "docs/ARCHITECTURE.md"), "utf8") === "# Architecture\n\nBefore ADR update projection.\n",
    "adr_update applied an appended architecture patch before the ADR was accepted",
  );

  assert(/store|defer/i.test(adrCreate.description), `adr_create description does not say it stores or defers architecture patches: ${adrCreate.description}`);
  assert(/store|defer/i.test(adrUpdate.description), `adr_update description does not say it stores or defers architecture patches: ${adrUpdate.description}`);
  assert(!/apply/i.test(adrCreate.description), `adr_create description still says it applies Proposed architecture patches: ${adrCreate.description}`);
  assert(!/apply/i.test(adrUpdate.description), `adr_update description still says it applies Proposed architecture patches: ${adrUpdate.description}`);
} finally {
  process.chdir(previousCwd);
  fs.rmSync(workspace, { recursive: true, force: true });
}

console.log("opencode adr_update behavior test passed");
