import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createRequire } from "node:module";

const root = path.resolve(import.meta.dirname, "../..");
const pluginPath = path.join(root, ".opencode/plugins/auto-review-discipline.ts");
const require = createRequire(import.meta.url);

function chain() {
  const schema = {};
  for (const method of ["describe", "min", "int", "nonnegative", "positive", "optional"] ) {
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
const adrCreate = plugin.tool?.adr_create;
assert(adrCreate?.execute, "adr_create tool is not registered");

const workspace = fs.mkdtempSync(path.join(os.tmpdir(), "auto-review-adr-create-"));
fs.mkdirSync(path.join(workspace, "docs"));
fs.writeFileSync(path.join(workspace, "docs/ADR-0001-existing.md"), "# ADR-0001: Existing\n");
fs.writeFileSync(path.join(workspace, "docs/ADR-0016-latest.md"), "# ADR-0016: Latest\n");
fs.writeFileSync(
  path.join(workspace, "docs/ARCHITECTURE.md"),
  "# Architecture\n\nCurrent projection before ADR.\n",
);

const previousCwd = process.cwd();
process.chdir(workspace);
try {
  await adrCreate.execute(
    {
      title: "Plugin Managed ADR Creation",
      date: "2026-05-16",
      context: "Direct edits to ADR files and the architecture projection need one coordinated tool path.",
      decision: "Create ADRs through adr_create so the ADR event and projection patch stay paired.",
      consequences: "Reviewers can see the proposed decision and architecture projection update together.",
      architecturePatch: {
        path: "docs/ARCHITECTURE.md",
        find: "Current projection before ADR.",
        replace: "Current projection after ADR-created architecture patch.",
      },
    },
    { sessionID: "adr-create-test" },
  );

  const adrPath = path.join(workspace, "docs/ADR-0017-plugin-managed-adr-creation.md");
  assert(fs.existsSync(adrPath), "adr_create did not allocate docs/ADR-0017-plugin-managed-adr-creation.md");
  const adr = fs.readFileSync(adrPath, "utf8");
  assert(adr.includes("# ADR-0017: Plugin Managed ADR Creation"), "ADR title did not use next allocated ID");
  assert(adr.includes("## Status\n\nProposed"), "ADR status was not derived as Proposed");
  assert(adr.includes("## Date\n\n2026-05-16"), "ADR did not include the supplied date");
  assert(adr.includes("## Context"), "ADR did not include a Context section");
  assert(
    adr.includes("Direct edits to ADR files and the architecture projection need one coordinated tool path."),
    "ADR did not include the supplied context",
  );
  assert(adr.includes("## Decision"), "ADR did not include a Decision section");
  assert(
    adr.includes("Create ADRs through adr_create so the ADR event and projection patch stay paired."),
    "ADR did not include the supplied decision",
  );
  assert(adr.includes("## Consequences"), "ADR did not include a Consequences section");
  assert(
    adr.includes("Reviewers can see the proposed decision and architecture projection update together."),
    "ADR did not include the supplied consequences",
  );
  assert(!adr.includes("undefined"), "ADR included an untyped or missing field value");
  assert(adr.includes("## Proposed Architecture Patch"), "ADR did not include the deferred proposed architecture patch section");
  assert(adr.includes("docs/ARCHITECTURE.md"), "proposed architecture patch section did not include the target path");
  assert(adr.includes("Current projection before ADR."), "proposed architecture patch section did not include the find text");
  assert(
    adr.includes("Current projection after ADR-created architecture patch."),
    "proposed architecture patch section did not include the replacement text",
  );

  const architecture = fs.readFileSync(path.join(workspace, "docs/ARCHITECTURE.md"), "utf8");
  assert(
    architecture === "# Architecture\n\nCurrent projection before ADR.\n",
    "adr_create mutated docs/ARCHITECTURE.md while ADR status is still Proposed",
  );

  try {
    await adrCreate.execute({ title: "Incomplete ADR" }, { sessionID: "adr-create-test" });
    throw new Error("adr_create accepted missing required fields");
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    assert(/context|decision|consequences|architecture/i.test(message), `missing-field rejection was not specific: ${message}`);
  }

  try {
    await adrCreate.execute(
      {
        title: "Supersedes Missing ADR",
        date: "2026-05-16",
        context: "Supersedes metadata should only point at ADR files that are present in the repository.",
        decision: "Reject supersedes entries when the referenced ADR file does not exist.",
        consequences: "Operators get a specific correction instead of a filesystem error.",
        architecturePatch: {
          path: "docs/ARCHITECTURE.md",
          find: "Current projection before ADR.",
          replace: "Current projection after rejected supersedes entry.",
        },
        supersedes: [{ path: "docs/ADR-0099-missing.md", reason: "It is listed but absent." }],
      },
      { sessionID: "adr-create-test" },
    );
    throw new Error("adr_create accepted a supersedes path for a missing ADR file");
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    assert(/existing ADR file/i.test(message), `missing supersedes ADR rejection was not specific: ${message}`);
  }
} finally {
  process.chdir(previousCwd);
  fs.rmSync(workspace, { recursive: true, force: true });
}

console.log("opencode adr_create behavior test passed");
