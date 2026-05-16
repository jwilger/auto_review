import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createRequire } from "node:module";
import { spawnSync } from "node:child_process";

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

function loadDisciplinePlugin(recordTouchedFile = () => {}) {
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
    recordTouchedFile,
    () => {},
    () => {},
    () => {},
    () => [],
  );
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function runGit(workspace, args) {
  const result = spawnSync("git", args, { cwd: workspace, encoding: "utf8" });
  if (result.status !== 0) {
    throw new Error(`git ${args.join(" ")} failed\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`);
  }
}

function adr(title, status, extra = "") {
  return `# ${title}

## Status

${status}

## Date

2026-05-16

## Context

Context for ${title}.

## Decision

Decision for ${title}.

## Consequences

Consequences for ${title}.
${extra}`;
}

async function assertRefusesMainAdr(toolUnderTest) {
  const beforeAdr = fs.readFileSync("docs/ADR-0001-accepted-on-main.md", "utf8");
  const beforeArchitecture = fs.readFileSync("docs/ARCHITECTURE.md", "utf8");
  let rejected = false;
  try {
    await toolUnderTest.execute({ path: "docs/ADR-0001-accepted-on-main.md" }, { sessionID: "adr-delete-unmerged-test" });
  } catch (error) {
    rejected = true;
    const message = error instanceof Error ? error.message : String(error);
    assert(/main/i.test(message), `adr_delete_unmerged rejection did not mention main: ${message}`);
  }
  assert(rejected, "adr_delete_unmerged deleted an ADR that exists on main");
  assert(fs.readFileSync("docs/ADR-0001-accepted-on-main.md", "utf8") === beforeAdr, "main ADR changed before rejection");
  assert(fs.readFileSync("docs/ARCHITECTURE.md", "utf8") === beforeArchitecture, "architecture changed before main-ADR rejection");
}

async function assertRejectsWithoutDeleting(toolUnderTest, workspace, targetPath, messagePattern, description) {
  const absoluteTarget = path.join(workspace, targetPath);
  const beforeTarget = fs.readFileSync(absoluteTarget, "utf8");
  let rejected = false;
  try {
    await toolUnderTest.execute({ path: targetPath }, { sessionID: "adr-delete-unmerged-test" });
  } catch (error) {
    rejected = true;
    const message = error instanceof Error ? error.message : String(error);
    assert(messagePattern.test(message), `${description} rejection had unexpected message: ${message}`);
  }
  assert(rejected, `${description} did not reject`);
  assert(fs.existsSync(absoluteTarget), `${description} deleted the target before rejecting`);
  assert(fs.readFileSync(absoluteTarget, "utf8") === beforeTarget, `${description} changed the target before rejecting`);
}

const touchedFiles = [];
const pluginFactory = loadDisciplinePlugin((_sessionID, filePath) => touchedFiles.push(filePath));
const plugin = await pluginFactory();
const adrDeleteUnmerged = plugin.tool?.adr_delete_unmerged;
assert(adrDeleteUnmerged?.execute, "adr_delete_unmerged tool is not registered");
assert(adrDeleteUnmerged.args?.path?.kind === "string", "adr_delete_unmerged.args.path is not a string field");

const nonGitWorkspace = fs.mkdtempSync(path.join(os.tmpdir(), "auto-review-adr-delete-unmerged-non-git-"));
fs.mkdirSync(path.join(nonGitWorkspace, "docs"));
fs.writeFileSync(
  path.join(nonGitWorkspace, "docs/ADR-0004-non-git.md"),
  adr("ADR-0004: Non Git", "Proposed"),
);

let previousCwd = process.cwd();
process.chdir(nonGitWorkspace);
try {
  await assertRejectsWithoutDeleting(
    adrDeleteUnmerged,
    nonGitWorkspace,
    "docs/ADR-0004-non-git.md",
    /main|git/i,
    "non-git workspace",
  );
} finally {
  process.chdir(previousCwd);
  fs.rmSync(nonGitWorkspace, { recursive: true, force: true });
}

const workspace = fs.mkdtempSync(path.join(os.tmpdir(), "auto-review-adr-delete-unmerged-"));
fs.mkdirSync(path.join(workspace, "docs"));
fs.writeFileSync(
  path.join(workspace, "docs/ADR-0001-accepted-on-main.md"),
  adr("ADR-0001: Accepted On Main", "Accepted"),
);
fs.writeFileSync(
  path.join(workspace, "docs/ARCHITECTURE.md"),
  "# Architecture\n\nStable architecture from ADR-0001 must remain.\n",
);
runGit(workspace, ["init", "-b", "main"]);
runGit(workspace, ["config", "user.email", "auto-review@example.invalid"]);
runGit(workspace, ["config", "user.name", "Auto Review Test"]);
runGit(workspace, ["add", "docs/ADR-0001-accepted-on-main.md", "docs/ARCHITECTURE.md"]);
runGit(workspace, ["commit", "-m", "docs: seed main ADR"]);

fs.writeFileSync(
  path.join(workspace, "docs/ADR-0002-delete-me.md"),
  adr("ADR-0002: Delete Me", "Proposed"),
);
fs.writeFileSync(
  path.join(workspace, "docs/ADR-0003-successor.md"),
  adr(
    "ADR-0003: Successor",
    "Proposed",
    "\n## Supersedes\n\n- docs/ADR-0002-delete-me.md: Temporary draft replaced by successor.\n- docs/ADR-0001-accepted-on-main.md: Accepted baseline remains.\n",
  ),
);
fs.appendFileSync(
  path.join(workspace, "docs/ARCHITECTURE.md"),
  "Draft projection from ADR-0002-delete-me.md must be removed.\n",
);
fs.writeFileSync(
  path.join(workspace, "docs/not-an-adr.md"),
  "# Not an ADR\n\nProtected delete should reject this path.\n",
);

previousCwd = process.cwd();
process.chdir(workspace);
try {
  await assertRejectsWithoutDeleting(
    adrDeleteUnmerged,
    workspace,
    "docs/not-an-adr.md",
    /docs\/ADR-|ADR/i,
    "non-ADR target path",
  );

  await adrDeleteUnmerged.execute({ path: "docs/ADR-0002-delete-me.md" }, { sessionID: "adr-delete-unmerged-test" });

  assert(!fs.existsSync("docs/ADR-0002-delete-me.md"), "adr_delete_unmerged did not delete the unmerged ADR file");
  const architecture = fs.readFileSync("docs/ARCHITECTURE.md", "utf8");
  assert(!architecture.includes("ADR-0002-delete-me.md"), "adr_delete_unmerged left the deleted ADR projection in docs/ARCHITECTURE.md");
  assert(architecture.includes("Stable architecture from ADR-0001 must remain."), "adr_delete_unmerged removed unrelated architecture content");

  const successor = fs.readFileSync("docs/ADR-0003-successor.md", "utf8");
  assert(!successor.includes("ADR-0002-delete-me.md"), "adr_delete_unmerged left deleted ADR supersession metadata in another ADR");
  assert(successor.includes("ADR-0001-accepted-on-main.md"), "adr_delete_unmerged removed unrelated supersession metadata");
  assert(
    !touchedFiles.includes("docs/not-an-adr.md"),
    "adr_delete_unmerged recorded an unrelated file whose content did not change",
  );

  await assertRefusesMainAdr(adrDeleteUnmerged);
} finally {
  process.chdir(previousCwd);
  fs.rmSync(workspace, { recursive: true, force: true });
}

console.log("opencode adr_delete_unmerged behavior test passed");
