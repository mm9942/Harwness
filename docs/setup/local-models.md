# Local models: vLLM, LM Studio and Ollama

This page describes how `harw` works with a model server running locally
(or on your own LAN). Any server with an OpenAI-compatible
`chat/completions` interface is supported. vLLM, LM Studio and Ollama have
been tested.

## What `harw` does differently for local providers

A provider counts as **local** if its base URL points at the local
machine (`localhost`, `127.0.0.0/8`, `::1`), if it uses the `ollama`
transport, or if it points at a private LAN IP with
`allow_insecure_lan = true`. Local providers get different defaults. Each
one can be overridden individually in `providers/<name>.toml`.

| Setting | Cloud default | Local default | Meaning |
|---|---|---|---|
| `auth_header` | `bearer` | `none` (only without `auth`) | no key, no `Authorization` header |
| `max_concurrency` | unlimited | `1` (on creation and in the catalog) | one model, one GPU: requests run sequentially |
| `request_timeout_secs` | 120 | 600 | total time without streaming, or wait time until response headers |
| `stream_idle_timeout_secs` | = request timeout | 120 | abort only once **no byte** arrives for this long |
| `retry_timeouts` | `true` | `false` | timeouts are not retried locally |
| `max_tokens_field` | `max_completion_tokens` | `max_tokens` | field name for the output limit (`both` sends both) |
| `send_reasoning_effort` | `true` | `false` | `reasoning_effort` in the request |
| `strict_tools` | `true` | `false` | `"strict"` and strict schemas per tool |
| `parallel_tool_calls` | (omitted) | `false` | value of `parallel_tool_calls` once tools are offered |

Further points:

- **Streaming:** while streaming, there is no overall time limit. A long
  but steadily flowing response is therefore not cut off. This also
  applies to cloud providers.
- **Context window:** local models never get the manufacturer's window
  from the built-in catalog. A locally started `Qwen3-32B` has whatever
  window the server was given via `--max-model-len` or the context length
  in LM Studio. `context_window` in `models/<id>.toml` is authoritative.
  `harw provider scan` fills in the window once the server reports it. If
  it is missing, a fallback window of 32k tokens applies, and the session
  logs a warning.
- **Proxy:** for loopback addresses, `harw` never uses a proxy from
  `HTTP(S)_PROXY`. This acts like an automatic `NO_PROXY` for `localhost`.
- **Reasoning text and tool syntax:** `<think>…</think>` appears in the
  live stream as reasoning text, not as the answer. Text starting with
  `<tool_call>`, `[TOOL_CALLS]` or `<|python_tag|>` is not shown live. At
  the end of the response, such text-encoded tool calls are recognized:
  Hermes/Qwen `<tool_call>`, Mistral `[TOOL_CALLS]`, Llama
  `<|python_tag|>`, and a response that consists solely of a JSON call to
  an offered tool. Unknown tool names or malformed JSON are never
  executed; the text is then left as plain text.
- **Models without tools:** if `models/<id>.toml` has `tool_calling =
  false` under `[capabilities]`, `harw` does not offer the model any
  tools. It gets a short notice instead. `harw provider scan` sets this
  value when the server reports `supported_parameters` without
  `"tools"`.

## vLLM

vLLM needs two startup options for tool calls: `--enable-auto-tool-choice`
and a `--tool-call-parser` matching the model. Without them the model only
responds with text.

```bash
# Qwen3 (Hermes format for tools), 32k context, one GPU
vllm serve Qwen/Qwen3-32B \
  --port 8000 \
  --max-model-len 32768 \
  --enable-auto-tool-choice \
  --tool-call-parser hermes \
  --reasoning-parser qwen3

# Qwen3-Coder
vllm serve Qwen/Qwen3-Coder-30B-A3B-Instruct \
  --port 8000 --max-model-len 65536 \
  --enable-auto-tool-choice --tool-call-parser qwen3_coder
```

Common parsers: `hermes` (Qwen2.5/Qwen3, Hermes), `qwen3_coder`,
`mistral`, `llama3_json` (Llama 3.x). With `--reasoning-parser`, vLLM
delivers reasoning text separately in `reasoning_content`. `harw` reads
it but does not show it as the answer.

Setting up `harw`:

```bash
harw provider add vllm --api openai-chat --base-url http://localhost:8000/v1 --no-auth
harw provider scan vllm          # reads the models and `max_model_len`
harw config set default_provider vllm   # optional: make vLLM the default
harw model default Qwen/Qwen3-32B
```

If vLLM runs with `--api-key`, pass a reference instead of `--no-auth`:
`--auth env:VLLM_API_KEY`. The header is then `Authorization: Bearer …`.
`--auth-header x-api-key` selects a different transport.

## LM Studio

1. In LM Studio, start the local server under **Developer** (default
   port 1234).
2. Load a model and deliberately set the **Context Length** (e.g.
   32768). This is the window `harw` uses.
3. Set it up in `harw`:

```bash
harw provider add lmstudio --api openai-chat --base-url http://localhost:1234/v1 --no-auth
harw provider scan lmstudio
```

LM Studio does not report the context window via `/v1/models`. For local
providers, `harw provider scan` therefore also queries
`GET /api/v0/models` and takes `loaded_context_length` or
`max_context_length` from there. A `context_window` entered by hand in
`models/<id>.toml` is kept on a repeated scan if the server reports none.

The catalog seed already creates `providers/lmstudio.toml` and
`providers/vllm.toml` disabled, with `auth_header = "none"` and
`max_concurrency = 1`. `harw provider enable lmstudio` is then enough.

## Ollama

```bash
harw provider add local --api ollama --base-url http://localhost:11434 --no-auth
harw provider scan local
```

Ollama does not report the context window. Set `context_window` in
`models/<id>.toml` to match `num_ctx`.

## Servers on the LAN

`http` to a private IP (`10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16`,
IPv6 ULA) is only allowed with an explicit opt-in. Traffic is then
unencrypted:

```bash
harw provider add gpu-box --api openai-chat \
  --base-url http://192.168.1.20:8000/v1 --no-auth --allow-insecure-lan
```

Hostnames (`gpu-box.lan`) don't count, because DNS can point anywhere. If
you need a name, put TLS in front of it and use `https`.

## Context window and concurrency

```toml
# models/Qwen%2FQwen3-32B.toml (written by `harw provider scan`)
id = "Qwen/Qwen3-32B"
provider = "vllm"
context_window = 32768

[capabilities]
tool_calling = true
```

```toml
# providers/vllm.toml
name = "vllm"
api = "openai-chat"
base_url = "http://localhost:8000/v1"
auth_header = "none"
max_concurrency = 1          # 2-4 if the server has enough KV cache
request_timeout_secs = 900   # very long prompts on slower hardware
```

`max_concurrency` is the recommended place for parallelism.
`[rate_limit] max_concurrent` also applies per budget bucket (including
per model). If both are set, the smaller limit wins. Further requests,
e.g. from parallel children, wait in the provider's queue. The wait time
does not count toward the request timeout, and the log reports
`provider.concurrency.waiting_for_slot`.

With windows under 64k, the full tool schemas often don't fit next to the
task and history. Such children are best started with a role that has
only a few tools, e.g. explorer.

## Mixed roles: local and cloud

The role models can be chosen per slot. A common pattern: exploration and
simple workers run locally, the orchestrator runs in the cloud.

```bash
harw model internal set explorer Qwen/Qwen3-32B --provider vllm
harw model internal set worker-simple Qwen/Qwen3-32B --provider vllm
harw model internal set root-orchestrator claude-opus-5-5 --provider anthropic
```

Equivalent in the harness configuration:

```toml
[internal_models.explorer]
provider = "vllm"
model = "Qwen/Qwen3-32B"

[internal_models.worker_simple]
provider = "vllm"
model = "Qwen/Qwen3-32B"
```

With `max_concurrency = 1`, an explorer wave then processes its children
one after another instead of overloading the local server.
