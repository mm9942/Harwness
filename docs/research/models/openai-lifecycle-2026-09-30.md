# OpenAI API lifecycle snapshot — 2026-09-30

Primary source: https://developers.openai.com/api/docs/deprecations  
Cross-check: https://developers.openai.com/api/docs/changelog

This file captures time-sensitive OpenAI API lifecycle information separately from the general `openai.json` capability catalog.

## Imminent / upcoming shutdowns

### 2026-10-01

- `gpt-5.4-cyber` -> `gpt-5.6-cyber`

### 2026-10-23

OpenAI lists the following older snapshots/families for API shutdown on 2026-10-23:

- `gpt-3.5-turbo-0125` -> `gpt-5.6-terra`
- `gpt-4-0613` -> `gpt-5.6-sol`
- `gpt-4-1106-preview` -> `gpt-5.6-sol`
- `gpt-4-turbo` / `gpt-4-turbo-2024-04-09` -> `gpt-5.6-sol`
- `gpt-4.1-nano` / `gpt-4.1-nano-2025-04-14` -> `gpt-5.6-luna`
- `gpt-4o-2024-05-13` -> `gpt-5.6-sol`
- `gpt-image-1` -> `gpt-image-2.5-sunburst` or `gpt-image-2.5-flare`
- `o1-2024-12-17` -> `gpt-5.6-sol`
- `o1-pro-2025-03-19` -> `gpt-5.6-sol` with `reasoning.mode = "pro"`
- `o3-mini-2025-01-31` -> `gpt-5.6-sol`
- `o4-mini-2025-04-16` -> `gpt-5.6-terra`
- `ft-o4-mini-2025-04-16` -> `gpt-5.6-terra`

Fine-tuned legacy families scheduled for the same date include `ft-gpt-3.5-turbo`, `ft-gpt-4`, `ft-gpt-4.1-nano-2025-04-14`, `ft-babbage-002`, and `ft-davinci-002`.

### 2026-11-30

- Reusable prompt objects and the `v1/prompts` API shut down.

This is not a model lifecycle event, but it affects provider/runtime compatibility and should be tracked by the same scheduled provider audit.

### 2026-12-01

- `gpt-image-1-mini` -> `gpt-image-2.5-sunburst` or `gpt-image-2.5-flare`
- `gpt-image-1.5` -> `gpt-image-2.5-sunburst` or `gpt-image-2.5-flare`
- `chatgpt-image-latest` -> `gpt-image-2.5-sunburst` or `gpt-image-2.5-flare`

### 2026-12-11

- `gpt-5-2025-08-07` -> `gpt-5.6-sol`
- `gpt-5-mini-2025-08-07` -> `gpt-5.6-terra`
- `gpt-5-nano-2025-08-07` -> `gpt-5.6-luna`
- `gpt-5-pro-2025-10-06` -> `gpt-5.6-sol` with `reasoning.mode = "pro"`
- `o3-2025-04-16` -> `gpt-5.6-sol`
- `o3-pro-2025-06-10` -> `gpt-5.6-sol` with `reasoning.mode = "pro"`

### 2027-01-20

Legacy audio/realtime families are scheduled for shutdown, including:

- `gpt-realtime` -> `gpt-realtime-2.1`
- `gpt-audio` -> `gpt-audio-1.5`
- `gpt-4o-audio` -> `gpt-audio-1.5`
- `gpt-4o-realtime` -> `gpt-realtime-2.1`
- `gpt-realtime-mini` -> `gpt-realtime-2.1-mini`
- `gpt-audio-mini` -> `gpt-audio-1.5`
- `gpt-4o-mini-realtime` -> `gpt-realtime-2.1-mini`
- `gpt-4o-mini-audio` -> `gpt-audio-1.5`

### 2027-02-26

Transcription families scheduled for shutdown:

- `whisper-1`
- `gpt-4o-transcribe`
- `gpt-4o-mini-transcribe`
- `gpt-4o-transcribe-diarize`

Recommended replacement: `gpt-live-transcribe` or `gpt-transcribe`, depending on the workload.

## Already retired immediately before this snapshot

On 2026-09-28, OpenAI shut down:

- `gpt-3.5-turbo-instruct`
- `babbage-002`
- `davinci-002`
- `gpt-3.5-turbo-1106`

These must not remain implicit Harwness candidates.

## Lifecycle policy

OpenAI documents minimum notice periods of:

- GA models: at least 6 months
- specialized GA variants: at least 3 months
- Preview models: potentially much shorter, e.g. about 2 weeks

Harwness must therefore treat preview/specialized model IDs as higher lifecycle-risk than stable GA IDs.

## Harwness implications

- Store explicit shutdown timestamps and replacements rather than a boolean `deprecated`.
- Separate model aliases from pinned snapshots.
- Do not silently migrate active sessions.
- Exclude shutdown models from implicit routing.
- Fail explicit selection after shutdown with a structured replacement list.
- Track non-model provider API lifecycle items such as `v1/prompts` in a sibling provider-capability lifecycle structure rather than forcing them into model metadata.
