# ar-llm

LLM provider abstraction. The bot's review-pipeline activities call
into a `Router` keyed by `ModelTier`; the router dispatches to the
configured provider. Today only `OpenAiProvider` ships, but it
covers every OpenAI-compatible endpoint (hosted OpenAI, Ollama,
vLLM, Together, OpenRouter, …) — a different vendor lives behind
adding one more `LlmProvider` impl.

## Public surface

| Item | Purpose |
|------|---------|
| `types::LlmProvider` (trait) | Object-safe; one method, `complete`. |
| `types::ModelTier` | `Reasoning`, `Cheap`, `Embedding`. The triage / verifier / RAG-context paths all pick a tier so swapping a model is a config change, not a code change. |
| `types::CompleteRequest`, `CompleteResponse` | Provider-agnostic shapes. `CompleteRequest::response_format` carries optional `JsonSchema { name, schema }` for structured output. |
| `types::Message`, `Role` | Chat turn. `system` field on `CompleteRequest` is preferred over a system-role message; the OpenAI provider emits both for compatibility. |
| `router::Router` | Tier → provider map. `with(tier, provider)` builder, `complete(tier, req)` dispatch. |
| `openai::OpenAiProvider` | Generic OpenAI-compatible client. `with_embedding_model` opts the provider into double-duty for the Embedding tier. |

## Configuration

Production wiring lives in the gateway's `main.rs`. The relevant
env vars:

```
LLM_BASE_URL                 # required: provider root
LLM_API_KEY                  # cloud only; Ollama doesn't need it
LLM_REASONING_MODEL          # default qwen2.5-coder:32b
LLM_CHEAP_MODEL              # opt-in; enables triage + verifier
LLM_EMBEDDING_MODEL          # opt-in; enables RAG context

LLM_EMBEDDING_BASE_URL       # if the embedder lives on a separate endpoint
LLM_EMBEDDING_API_KEY
LLM_CHEAP_BASE_URL
LLM_CHEAP_API_KEY
```

`auto_review doctor --llm-base-url ... --llm-reasoning-model ...`
verifies each configured model is loaded on the inference server.

## Tests

`cargo test -p ar-llm` covers the router's tier dispatch and
error-on-missing-tier path, plus the OpenAI provider's request
shape against a wiremock'd endpoint. The full review pipeline's
LLM-driven tests (in `ar-review`) use a `CannedProvider` that
implements `LlmProvider` over a vec of pre-recorded responses.

## Dependencies

`reqwest` for HTTP, `serde_json` for the chat-completions wire
shape, `async-trait` so `LlmProvider` is object-safe.
