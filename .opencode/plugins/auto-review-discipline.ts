import { tool, type Plugin } from "@opencode-ai/plugin";
import { assertCleanWorktree, getCycle, isNonBehavioralPath, isProductionRustPath, isLikelyTestPath, recordTouchedFile, setCycle, clearCycle, recordVerification, sessionContext, validateRgrRedEvidence } from "./lib/shared.ts";

function filePathFromArgs(args: unknown): string | undefined {
  if (!args || typeof args !== "object") return undefined;
  const record = args as Record<string, unknown>;
  const path = record.filePath ?? record.file_path ?? record.path;
  return typeof path === "string" ? path : undefined;
}

function isEditTool(toolID: string): boolean {
  return /(^|\.)(edit|write|apply_patch)$/i.test(toolID) || /apply_patch/i.test(toolID);
}

function rejectsWaterfallTodo(args: unknown): boolean {
  const text = JSON.stringify(args ?? "").toLowerCase();
  const componentWords = ["model", "handler", "route", "repository", "service", "then add tests"];
  const hasComponents = componentWords.filter((word) => text.includes(word)).length >= 2;
  return hasComponents && !text.includes("red") && !text.includes("failing test") && !text.includes("rgr");
}

export const AutoReviewDisciplinePlugin: Plugin = async ({ worktree }) => ({
  tool: {
    rgr_start: tool({
      description: "Start an auto_review RED-GREEN-REFACTOR cycle for one behavior.",
      args: {
        behavior: tool.schema.string().describe("Observable behavior under test"),
        test: tool.schema.string().describe("Specific failing test name or path"),
      },
      async execute(args, context) {
        assertCleanWorktree(worktree);
        setCycle(context.sessionID, { behavior: args.behavior, test: args.test, stage: "red" });
        return `RGR cycle started for ${args.behavior}. Record observed RED output before production edits.`;
      },
    }),
    rgr_record_red: tool({
      description: "Record observed failing test output for the active RGR cycle.",
      args: {
        command: tool.schema.string().describe("Focused test command that failed"),
        output: tool.schema.string().min(1).describe("Copied failing output from the actual run"),
      },
      async execute(args, context) {
        const current = getCycle(context.sessionID);
        if (!current) throw new Error("Start an RGR cycle before recording RED.");
        validateRgrRedEvidence(args.output);
        setCycle(context.sessionID, { ...current, command: args.command, failingOutput: args.output, stage: "red" });
        return "RED recorded. RED review approval is required before production edits.";
      },
    }),
    rgr_approve_red: tool({
      description: "Approve the recorded RED evidence before production edits.",
      args: {},
      async execute(_args, context) {
        const current = getCycle(context.sessionID);
        if (!current?.failingOutput) throw new Error("Cannot approve RED before observed RED is recorded.");
        setCycle(context.sessionID, { ...current, reviewedRed: true });
        return "RED approved. Minimum production edits are now allowed for this cycle.";
      },
    }),
    rgr_mark_green: tool({
      description: "Mark the active RGR cycle green after the focused test passes.",
      args: { output: tool.schema.string().describe("Passing test output or concise verification summary") },
      async execute(args, context) {
        const current = getCycle(context.sessionID);
        if (!current?.failingOutput) throw new Error("Cannot mark GREEN before observed RED is recorded.");
        setCycle(context.sessionID, { ...current, stage: "green" });
        recordVerification(context.sessionID, args.output);
        return "GREEN recorded. Refactoring is allowed with tests green.";
      },
    }),
    rgr_mark_refactor: tool({
      description: "Mark refactor completion and clear the active RGR cycle.",
      args: { verification: tool.schema.string().describe("Verification run after refactor") },
      async execute(args, context) {
        recordVerification(context.sessionID, args.verification);
        clearCycle(context.sessionID);
        return "REFACTOR recorded. RGR cycle complete. Commit the approved GREEN/refactor state before starting the next RED.";
      },
    }),
    rgr_status: tool({
      description: "Inspect active RGR and verification context.",
      args: {},
      async execute(_args, context) {
        const items = sessionContext(context.sessionID);
        return items.length ? items.join("\n") : "No active RGR cycle recorded for this session.";
      },
    }),
  },
  "tool.execute.before": async (input, output) => {
    if (isEditTool(input.tool)) {
      const path = filePathFromArgs(output.args);
      if (path) recordTouchedFile(input.sessionID, path);
      if (path && isProductionRustPath(path) && !isLikelyTestPath(path) && !isNonBehavioralPath(path)) {
        const current = getCycle(input.sessionID);
        if (!current?.reviewedRed) {
          throw new Error("RGR gate: production Rust edits under crates/*/src require RED review approval recorded with rgr_approve_red.");
        }
      }
    }
    if (/todo(write|update)?$/i.test(input.tool) && rejectsWaterfallTodo(output.args)) {
      throw new Error("RGR plan gate: behavior work todo lists must name failing tests, not component-waterfall tasks.");
    }
  },
  "experimental.session.compacting": async (input, output) => {
    output.context.push(...sessionContext(input.sessionID));
  },
});

export default AutoReviewDisciplinePlugin;
