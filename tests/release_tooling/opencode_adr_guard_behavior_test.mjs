import fs from "node:fs";
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
  object: () => chain(),
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

const pluginFactory = loadDisciplinePlugin();
const plugin = await pluginFactory();
const beforeHook = plugin["tool.execute.before"];

const protectedAttempts = [
  {
    name: "edit rejects ADR document path",
    input: { tool: "edit", sessionID: "adr-guard-test" },
    output: { args: { filePath: "docs/ADR-0001-hybrid-review-pipeline.md", oldString: "old", newString: "new" } },
  },
  {
    name: "write rejects architecture document path",
    input: { tool: "write", sessionID: "adr-guard-test" },
    output: { args: { path: "docs/ARCHITECTURE.md", content: "updated architecture" } },
  },
  {
    name: "edit rejects absolute ADR document path",
    input: { tool: "edit", sessionID: "adr-guard-test" },
    output: {
      args: {
        filePath: path.join(root, "docs/ADR-0001-hybrid-review-pipeline.md"),
        oldString: "old",
        newString: "new",
      },
    },
  },
  {
    name: "write rejects absolute architecture document path",
    input: { tool: "write", sessionID: "adr-guard-test" },
    output: { args: { path: path.join(root, "docs/ARCHITECTURE.md"), content: "updated architecture" } },
  },
  {
    name: "apply_patch rejects ADR document update",
    input: { tool: "apply_patch", sessionID: "adr-guard-test" },
    output: {
      args: {
        patchText: "*** Begin Patch\n*** Update File: docs/ADR-0001-hybrid-review-pipeline.md\n@@\n-old\n+new\n*** End Patch",
      },
    },
  },
  {
    name: "apply_patch rejects architecture document add",
    input: { tool: "apply_patch", sessionID: "adr-guard-test" },
    output: {
      args: {
        patchText: "*** Begin Patch\n*** Add File: docs/ARCHITECTURE.md\n+new architecture\n*** End Patch",
      },
    },
  },
  {
    name: "apply_patch rejects absolute ADR document update",
    input: { tool: "apply_patch", sessionID: "adr-guard-test" },
    output: {
      args: {
        patchText: `*** Begin Patch\n*** Update File: ${path.join(root, "docs/ADR-0001-hybrid-review-pipeline.md")}\n@@\n-old\n+new\n*** End Patch`,
      },
    },
  },
  {
    name: "apply_patch rejects absolute architecture document add",
    input: { tool: "apply_patch", sessionID: "adr-guard-test" },
    output: {
      args: {
        patchText: `*** Begin Patch\n*** Add File: ${path.join(root, "docs/ARCHITECTURE.md")}\n+new architecture\n*** End Patch`,
      },
    },
  },
];

let failures = 0;

for (const attempt of protectedAttempts) {
  try {
    await beforeHook(attempt.input, attempt.output);
    console.log(`not ok - ${attempt.name} (tool call was allowed)`);
    failures += 1;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (/adr/i.test(message) && /workflow/i.test(message) && /tools?/i.test(message)) {
      console.log(`ok - ${attempt.name}`);
    } else {
      console.log(`not ok - ${attempt.name} (directive did not mention ADR workflow tools: ${message})`);
      failures += 1;
    }
  }
}

if (failures > 0) {
  console.log(`opencode ADR guard behavior tests failed: ${failures}`);
  process.exit(1);
}

console.log("opencode ADR guard behavior tests passed");
