# Model-initiated image inspection

A model can call `fs.view_image` with `{ "path": "relative/image.png", "detail": "high" }` during the normal tool loop. `detail` is optional (`low`, `high`, `auto`). This requires the same `ReadWorkspace` grant and path review as `fs.read`. PNG, JPEG, GIF and WebP are identified from their bytes; the existing media policy caps input at 7 MiB, 8000 pixels per edge and 40 million pixels. The existing sanitizer removes metadata and trailing payloads. No image decoder or OCR summarizer replaces the model's visual input.

The tool returns a typed `ImageToolResult` containing a sanitized content reference and the detail preference. It writes no image bytes, source paths or base64 into the transcript. The source is bounded process memory (64 MiB); restart or eviction can make a reference unavailable. In that case the model receives an explicit image-not-sent placeholder, never an invented visual description.

History projection accepts this descriptor only for an earlier `fs.view_image` call with the matching call ID. All results of a parallel tool batch precede the generated user media message, whose text identifies each image's originating call. At most ten images are forwarded per batch; further results report `image_limit_exceeded`. Existing request-budget estimation includes the projected visual-token cost.

The actual sanitized pixels travel through the normal provider image schemas: OpenAI Chat Completions `image_url` data parts, OpenAI Responses `input_image`, and Anthropic Messages base64 `image` blocks. A subsequent model response can continue reasoning and call additional tools. Image content remains untrusted data.

For a model configured with `image_input = false`, the existing provider gate replaces the image tool result with a structured `image_inspection_unavailable` outcome (`status: error`, `code: vision_unavailable`) before request serialization. No image pixels are sent to that text-only model, and this route does not change provider or model. A separate explicitly configured image-understanding operation, where available, is a distinct tool route.

Regression tests cover permission and path denial, invalid images, successful sanitized media reads, image association after parallel results, rejection of arbitrary tool JSON, text-only refusal, and native image serialization for all three provider formats. Tests are authored but have not been run in this change because Cargo execution was prohibited for this worktree.

## Selecting an auxiliary image model

Use `/models image <provider/model>` to save a profile-scoped image-understanding
selection, `/models image show` to inspect it, and `/models image reset` to remove
it. Selection requires confirmed image-input capability from model configuration
or the exact provider/model catalog entry. A text-only model such as GLM-5.3 is
rejected. The generic `/models set image-understanding <provider/model>` and
`/models reset image-understanding` commands address the same selection.

The model can call `image.inspect` with structured arguments such as
`{"path":"relative/image.png","question":"What does this chart show?"}`. The
operation asks the selected image model once and returns its answer with the
provider and model named in both text and structured provenance. This lets a
text-only main model use the explicit image model. The operation requires model
tool approval and its own `ReadWorkspace` grant. It delegates the actual read to
the `fs.view_image` executor to reuse workspace path checks, sanitization and
byte/pixel limits. This is an independently authorized operation: it does not
inherit or re-evaluate `fs.view_image` allowlist/denylist rules. Its own
`approval = "always"` surface and the caller's sandbox authorize the read.

When no image model is selected, `image.inspect` can use the active main model
only when that exact model has confirmed image-input capability. Otherwise it
returns `OpError::NotAvailable` with `no_image_model` and the actionable
`/models image <provider/model>` hint. A malformed, retired or text-only explicit
selection fails; it never substitutes another model or provider. No OpenRouter
model is chosen implicitly.

The auxiliary request includes actual native image media and a question of at
most 4096 bytes. It offers no tools and executes no returned tool calls. It
inherits turn cancellation, uses the existing production provider router and its
rate limits, and has a 60-second timeout. Each request has a unique auxiliary
agent identity within the caller's session, isolating native-thinking replay from
the main conversation. Output is limited to 2048 requested tokens and 16 KiB of
returned answer text, with a `truncated` flag when needed. Failed requests retain
their output reservation because providers may bill them.

Current integration exposes `image.inspect` on the root model-tool surface.
Child-agent registries do not yet receive this operation. Repeated auxiliary
calls share a finite output allowance derived from the configured root token
budget; this allowance is separate from the main turn's token meter. It does not
reserve or debit the main turn's remaining tokens, count auxiliary input tokens,
or enforce a combined main-plus-auxiliary token ceiling. Native
`fs.view_image` inspection on a vision-capable main model continues through the
normal turn loop and its existing accounting. Auxiliary answers remain untrusted
model-generated observations.
