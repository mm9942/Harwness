# Native reasoning and tool continuity

Tool execution uses structured provider tool calls after the complete response has
been parsed. Reasoning text is never parsed as a command. The existing tool loop
can retrieve context when an assistant response contains tool calls and no visible
answer; tool results are returned before the next model request.

Chat-compatible reasoning routes can preserve the exact native assistant message,
including `reasoning_content`, content and tool calls. Known official GLM and
DeepSeek routes have explicit profiles; arbitrary compatible endpoints must opt
in using `capabilities.chat_reasoning_replay`. GLM-5.3 is text-only and its mandatory
thinking mode is retained. Generic effort mapping to vendor values is adapter
policy, not a claim that every provider implements the same effort scale.

Anthropic continuations preserve the complete original assistant content array,
including signed or redacted thinking blocks and tool-use ordering. OpenAI
Responses uses native reasoning items. Opaque provider state is replayed unchanged;
it is not rendered as a replacement explanation or synthesized from visible text.

Replay caches are process-local and bounded. Entries must match the caller and
provider route and the original conversation round, rather than only a tool-call
identifier. A route requiring preserved context refuses when that context is
missing. Restart, cache eviction, history compaction and normalized tool arguments
can therefore require a fresh context; transparent durable continuation is not
implemented by this change. This limitation matters for long-running agents.

Provider contracts consulted:

- https://docs.z.ai/guides/llm/glm-5.3
- https://docs.z.ai/guides/capabilities/thinking-mode
- https://api-docs.deepseek.com/guides/thinking_mode
- https://developers.openai.com/api/docs/guides/reasoning
- https://platform.claude.com/docs/en/build-with-claude/thinking

Regression tests are authored. The user paused Cargo compilation, so the combined
implementation has not been compiled, run against real providers or tested yet.
