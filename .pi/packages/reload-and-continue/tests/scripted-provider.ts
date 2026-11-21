import { appendFileSync, existsSync, writeFileSync } from "node:fs";
import {
  createAssistantMessageEventStream,
  getCurrentTools,
  type AssistantMessage,
} from "@earendil-works/pi-ai";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const tool = "reload_and_continue";
const probePath = ".pi/extensions/new-resource.ts";
const continuation = "Resume item seventeen using new_regression_resource from .pi/extensions/new-resource.ts.";
const secondContinuation = "Use the second request instead.";
const record = (event: object) => appendFileSync("trace.jsonl", `${JSON.stringify(event)}\n`);

export default function (pi: ExtensionAPI) {
  const scenario = process.env.RELOAD_TEST_SCENARIO;
  pi.on("session_start", ({ reason }, ctx) => { record({ event: "start", reason, mode: ctx.mode }); });
  pi.on("session_shutdown", ({ reason }) => { record({ event: "shutdown", reason }); });
  pi.on("agent_settled", ({ aborted }, ctx) => {
    record({ event: "settled", aborted });
    if (scenario === "rpc") ctx.shutdown();
  });
  pi.on("tool_execution_end", ({ toolName, isError, result }) => {
    if (toolName === tool) record({ event: "result", isError, result });
  });
  pi.on("tool_result", ({ toolName, isError }, ctx) => {
    // Abort after the reload tool has accepted a request, but before the run settles.
    if (scenario === "abort" && toolName === tool && !isError) ctx.abort();
  });
  pi.registerProvider("regression", {
    baseUrl: "http://127.0.0.1:1",
    apiKey: "unused-offline-placeholder",
    api: "regression-script",
    models: [{ id: "script", name: "Script", reasoning: false, input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 8192, maxTokens: 1024 }],
    streamSimple(model, context) {
      const tools = getCurrentTools(context.messages).map(({ name }) => name);
      const input = context.messages.filter(({ role }) => role === "user").at(-1)?.content;
      record({ event: "request", tools, input });
      // File existence also tracks the initial request across provider reloads.
      const firstRequest = !existsSync(probePath);
      const callReload = firstRequest && tools.includes(tool);
      if (firstRequest) {
        // A resource that cannot have been loaded at startup.
        writeFileSync(probePath, `
          export default function (pi) {
            pi.registerTool({
              name: "new_regression_resource", label: "New resource", description: "Reload probe",
              parameters: { type: "object", properties: {} },
              async execute() { return { content: [], details: {}, terminate: true }; },
            });
          }
        `);
      }
      const continuations = scenario === "batch" ? [continuation, secondContinuation] : [continuation];
      const message: AssistantMessage = {
        role: "assistant", api: model.api, provider: model.provider, model: model.id,
        content: callReload
          ? continuations.map((text, index) => ({
              type: "toolCall", id: `reload_${index}`, name: tool, arguments: { continuation: text },
            }))
          : [{ type: "text", text: "Script finished." }],
        usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, totalTokens: 2,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
        stopReason: callReload ? "toolUse" : "stop", timestamp: Date.now(),
      };
      const stream = createAssistantMessageEventStream();
      stream.push({ type: "start", partial: message });
      stream.push({ type: "done", reason: callReload ? "toolUse" : "stop", message });
      stream.end();
      return stream;
    },
  });
}
