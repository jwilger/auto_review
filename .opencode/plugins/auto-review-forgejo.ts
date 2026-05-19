import { tool, type Plugin } from "@opencode-ai/plugin";
import { blocksForgejoInlineReply, commandText, forgejoInlineReplyPayload, recordForgejoFeedback } from "./lib/shared.ts";

const issuePrBranchPattern = /(?:^|[\\/]|\s)issue[-_](\d+)(?:[-_][^\s]+)?(?:\s|$)/i;

function extractCommandArg(command: string, flag: string): string | undefined {
  const escaped = flag.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const regex = new RegExp(`(?:^|\\s)--${escaped}(?:=|\\s+)(?:\"([^\"]*)\"|'([^']*)'|([^\\s]+))`);
  const match = regex.exec(command);
  if (!match) return undefined;
  return match[1] ?? match[2] ?? match[3];
}

function parseForgejoPrCreate(command: string) {
  if (!/\b(?:tea|forgejo)\s+pr\s+create\b/.test(command)) return null;
  return {
    head: extractCommandArg(command, "head"),
    title: extractCommandArg(command, "title"),
    body: extractCommandArg(command, "description") || extractCommandArg(command, "body"),
  };
}

function issueBranchAndNumber(head: string | undefined): number | null {
  if (!head) return null;
  const match = issuePrBranchPattern.exec(head);
  return match ? Number(match[1]) : null;
}

function hasIssueClosureTrailer(text: string, issue: number): boolean {
  if (!text) return false;
  const escaped = String(issue);
  const trailer = new RegExp(`(^|\\n)\\s*(Closes|Fixes|Resolves)\\s*#\\s*${escaped}(?:\\s|:|$)`, "i");
  return trailer.test(text);
}

export const AutoReviewForgejoPlugin: Plugin = async () => ({
  tool: {
    forgejo_inline_reply_payload: tool({
      description: "Build the Forgejo inline review reply payload using comment.position as new_position.",
      args: {
        body: tool.schema.string().describe("Reply body"),
        path: tool.schema.string().describe("Original inline comment path"),
        position: tool.schema.number().int().nonnegative().describe("Original inline comment position field"),
      },
      async execute(args) {
        return JSON.stringify(forgejoInlineReplyPayload(args), null, 2);
      },
    }),
    forgejo_feedback_status: tool({
      description: "Record or summarize unresolved Forgejo feedback status for compaction context.",
      args: { summary: tool.schema.string().describe("Feedback status summary") },
      async execute(args, context) {
        recordForgejoFeedback(context.sessionID, args.summary);
        return `Forgejo feedback status recorded: ${args.summary}`;
      },
    }),
    forgejo_review_api_recipe: tool({
      description: "Return the Forgejo API recipe for listing reviews/comments and replying inline.",
      args: { owner: tool.schema.string(), repo: tool.schema.string(), pr: tool.schema.number().int().positive() },
      async execute(args) {
        return [
          `GET /api/v1/repos/${args.owner}/${args.repo}/pulls/${args.pr}/reviews`,
          `GET /api/v1/repos/${args.owner}/${args.repo}/pulls/${args.pr}/reviews/{review_id}/comments`,
          `POST /api/v1/repos/${args.owner}/${args.repo}/pulls/${args.pr}/reviews/{review_id}/comments`,
          "Payload: { body, path: comment.path, new_position: comment.position, old_position: 0 }",
        ].join("\n");
      },
    }),
  },
  "tool.execute.before": async (input, output) => {
    if (/bash$/i.test(input.tool) && blocksForgejoInlineReply(commandText(output.args))) {
      throw new Error("Forgejo review gate: inline feedback replies must use the existing review comment thread before any top-level PR comment.");
    }

    const command = commandText(output.args);
    const prCreate = /\b(?:tea|forgejo)\s+pr\s+create\b/.test(command) ? parseForgejoPrCreate(command) : null;
    if (!prCreate) return;

    const issueNumber = issueBranchAndNumber(prCreate.head);
    if (!issueNumber) return;
    const description = prCreate.body ?? "";

    if (!hasIssueClosureTrailer(description, issueNumber)) {
      throw new Error(
        `Forgejo PR creation gate: branch '${prCreate.head}' appears issue-linked. Include one closure trailer in the PR description, for example ` +
          `"Closes #${issueNumber}", "Fixes #${issueNumber}", or "Resolves #${issueNumber}".`
      );
    }
  },
});

export default AutoReviewForgejoPlugin;
export const server = AutoReviewForgejoPlugin;
