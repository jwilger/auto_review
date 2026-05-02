import type { Plugin } from "@kilocode/plugin";
import { blocksUnsafeToolchainCommand, commandText } from "./lib/shared.ts";

export const AutoReviewToolchainPlugin: Plugin = async ({ worktree }) => ({
  "shell.env": async (_input, output) => {
    output.env.CARGO_HOME = `${worktree}/.dependencies/cargo`;
    output.env.RUSTUP_HOME = `${worktree}/.dependencies/rustup`;
  },
  "tool.execute.before": async (input, output) => {
    if (/bash$/i.test(input.tool) && blocksUnsafeToolchainCommand(commandText(output.args))) {
      throw new Error("auto_review toolchain gate blocked a command that bypasses Nix, scope hygiene, hooks, signing, or git safety.");
    }
  },
});

export default AutoReviewToolchainPlugin;
