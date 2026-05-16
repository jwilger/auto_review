import type { Plugin } from "@opencode-ai/plugin";
import { sessionContext } from "./lib/shared.ts";

export const AutoReviewContextPlugin: Plugin = async () => ({
  "experimental.session.compacting": async (input, output) => {
    const context = sessionContext(input.sessionID);
    if (context.length) {
      output.context.push("auto_review project context:", ...context);
    }
  },
  "tool.execute.after": async (input, output) => {
    if (/rgr_|forgejo_/i.test(input.tool)) {
      output.metadata = { ...(output.metadata ?? {}), autoReviewContextPreserved: true };
    }
  },
});

export default AutoReviewContextPlugin;
