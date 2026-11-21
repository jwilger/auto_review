import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

const command = "auto-review-reload-and-continue";

type Handoff = { send?: (text: string) => void };

// Pi reloads this module and invalidates its old API. Only the one-shot
// receiver lookup spans runtimes, keyed by Pi's unchanged session manager.
const bridgeKey = Symbol.for("auto_review.pi.reload_and_continue.v1");
const processGlobal = globalThis as typeof globalThis & {
  [bridgeKey]?: WeakMap<object, Handoff>;
};
const handoffs = processGlobal[bridgeKey] ??= new WeakMap<object, Handoff>();

export default function reloadAndContinue(pi: ExtensionAPI) {
  let pending: string | undefined;
  let ready: string | undefined;

  pi.registerTool({
    name: "reload_and_continue",
    label: "Reload and Continue",
    description:
      "Interactive Pi only. After saving a Pi resource, call this as the sole final tool call. " +
      "Pi will reload when this run settles and start a new turn with your continuation.",
    parameters: Type.Object({
      continuation: Type.String({
        minLength: 1,
        description: "Name the new resource and path, how to verify it loaded, and what to do next.",
      }),
    }),
    async execute(_id, { continuation }, _signal, _onUpdate, ctx) {
      if (ctx.mode !== "tui") throw new Error("reload_and_continue requires interactive Pi.");
      const text = continuation.trim();
      if (!text) throw new Error("Provide a continuation naming the resource and the next action.");

      const alreadyPending = pending !== undefined;
      pending ??= text;
      return {
        content: [{
          type: "text",
          text: alreadyPending ? "The first continuation is already queued." : "Reload queued for when this run settles.",
        }],
        details: {},
        terminate: true,
      };
    },
  });

  pi.on("agent_settled", ({ aborted }) => {
    const text = pending;
    pending = undefined;
    if (aborted || text === undefined) return;

    ready = text;
    // This is dispatched as a command after all settled handlers finish.
    pi.sendUserMessage(`/${command}`, { expandPromptTemplates: true });
  });

  pi.on("session_start", ({ reason }, ctx) => {
    if (reason !== "reload") return;
    const handoff = handoffs.get(ctx.sessionManager);
    if (handoff) {
      handoff.send = (text) => pi.sendUserMessage(text, { expandPromptTemplates: false });
    }
  });

  pi.registerCommand(command, {
    description: "Internal command for a settled reload_and_continue request",
    handler: async (_args, ctx) => {
      const text = ready;
      ready = undefined;
      if (text === undefined || ctx.mode !== "tui") return;

      const session = ctx.sessionManager;
      const handoff: Handoff = {};
      handoffs.set(session, handoff);
      try {
        await ctx.reload();
      } finally {
        handoffs.delete(session);
      }

      handoff.send?.(
        "Reload handoff: verify the resource named below actually loaded before relying on it. " +
        "If it did not load, report that instead of using it.\n\n" + text,
      );
    },
  });
}
