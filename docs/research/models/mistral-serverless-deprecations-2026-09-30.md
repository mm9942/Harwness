# Mistral Serverless API deprecations — 2026-09-30

Source: Mistral AI customer deprecation notice received 2026-09-30.

This file records provider-specific lifecycle facts used for Harwness planning. It is intentionally separate from direct-vendor research such as `zai.json`.

## Retiring 2026-09-30

### Mistral OCR 4.0

- retiring ID: `mistral-ocr-4-0`
- replacements: `mistral-ocr-4-1`, `mistral-ocr-4-latest`, `mistral-ocr-4`
- price: $4 / 1,000 pages
- Batch API: $2 / 1,000 pages (50% discount)
- Document AI with OCR 4.1: $5 / 1,000 pages

### Leanstral 1.5

- retiring ID: `labs-leanstral-1-5`
- replacement: none stated in the notice
- release class: Labs / experimental

## Retiring 2026-10-31

### Z.AI GLM 5.2 on Mistral Serverless

- retiring ID: `zai-glm-5-2`
- replacements: `zai-glm-5-3`, `zai-glm-latest`, `zai-glm-5`
- input: $1.40 / 1M tokens
- cached input: $0.14 / 1M tokens
- output: $4.40 / 1M tokens
- notice states pricing remains unchanged for the replacement

## General pricing note

Mistral states that models are available with a 50% discount through its Batch API.

## Data-model note

These are Mistral Serverless offering facts. They must not be applied automatically to the direct Mistral or direct Z.AI provider records when provider IDs, pricing, availability, aliases, or retirement schedules differ.
