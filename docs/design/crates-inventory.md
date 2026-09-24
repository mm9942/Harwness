# Harwness Crate Inventory (Procurement List)

> Status: partially implemented · Last reviewed: 2026-09-24

Design-time inventory, researched 2026-07-14 against crates.io. This is the
canonical list of external dependencies for the crates planned in
[interaction-contract.md](interaction-contract.md),
[tui-command-contract.md](tui-command-contract.md),
[channel-ingress-telegram.md](channel-ingress-telegram.md),
[knowledge-surfaces.md](knowledge-surfaces.md) and
[secrets-and-audit.md](secrets-and-audit.md).

**Rules**

- Versions below are the latest verified at research time. They are *not* pins:
  every dependency is added via `cargo add <crate>` at implementation time so the
  actual latest compatible version is resolved then.
- Forbidden everywhere: `anyhow`, `thiserror` (hand-written error enums via
  `harw_macros::HarwError`), `log` (use `tracing`), and every crate listed under
  "Rejected" with a security note.
- RustSec-flagged crates that must never appear in the tree: `backoff`
  (unmaintained), `serde_yml` (RUSTSEC-2025-0068, unsound), `fs2` (unmaintained),
  `atomicwrites` (stale), `serde_yaml` (deprecated by its author).

## Already present in the workspace

| Crate | Where | Note |
|---|---|---|
| serde / serde_json | all crates | keep |
| toml 0.8 | harw-config | keep |
| uuid 1 (`v4`) | harw-types | **add `v7` feature** for time-ordered IDs |
| secrecy 0.8 | harw-provider | workspace convention for in-memory secrets; wraps zeroize |
| tokio 1 (`rt`, `sync`, `macros`, `net`, `time`, `rt-multi-thread`) | harw-core, harw-channel | features grew once `harw-channel` landed |
| once_cell, url, async-trait | various | keep |

## harw-tui

| Crate | Version (2026-07-14) | Purpose |
|---|---|---|
| ratatui | 0.30.2 | TUI framework |
| crossterm | 0.29.0 | terminal backend (ratatui default) |
| ratatui-textarea | 0.9.2 | multi-line input (official ratatui-org fork) |
| tui-input | 0.15.3 | single-line prompts |
| tui-markdown | 0.3.8 | markdown → `ratatui::Text` |
| syntect + two-face | 5.3.0 / 0.5.1 | syntax highlighting with bundled definitions |
| nucleo-matcher | 0.3.1 | fuzzy scoring for typed-ref completion |
| tui-scrollview | 0.6.7 | scroll primitive under the pager surface |
| arboard | 3.6.1 | clipboard for `/export` (feature-gate on headless Linux) |
| ratatui-image | 11.0.6 | optional inline images |
| clap | 4.6.1 | launch flags of the binary (incl. `--log`) |

Rejected: tui-textarea (rhysd original — stagnant, use the ratatui-org fork),
fuzzy-matcher (unmaintained since 2020), termimad (own painter model, not a
ratatui widget), copypasta (narrower platform coverage than arboard), edtui
(full modal editor, too heavy), nucleo full crate (async injector overkill;
revisit for filter-as-you-type lists).

Hand-rolled (no suitable crate): key-chord state machine (prefix tree + timeout
over crossterm `KeyEvent`s), pager semantics (in-buffer search, `--follow`,
dismiss) on top of tui-scrollview, sigil parser + typed-ref resolver with
"did you mean" ranking.

Pitfalls: lock ratatui/crossterm to a single version across the workspace
(duplicate backend types don't interoperate); arboard pulls X11/Wayland
backends on Linux; nucleo-matcher pulls parking_lot with extra features —
watch workspace feature unification.

## harw-channel / harw-channel-telegram

| Crate | Version | Purpose |
|---|---|---|
| frankenstein | 0.50.x | typed Bot API client (1:1 structs, reqwest or ureq backend) — a client, not a framework |
| reqwest | align with frankenstein's pin (0.12/0.13 line) | HTTP client, rustls TLS |
| axum | 0.8.6 | webhook listener (plain HTTP on loopback; TLS at the reverse proxy) |
| governor | 0.10.4 | token-bucket rate limiting (inbound per peer, outbound per chat) |
| backon | 1.6.0 | retry/backoff (successor of the unmaintained `backoff`) |
| infer | 0.19.0 | content-based MIME sniffing (never trust Telegram's `mime_type`) |
| sha2 | 0.10.x stable | attachment-manifest digests (audit-conventional; blake3 documented alternative) |

Rejected: teloxide (framework with dispatcher/dialogue FSM, tokio-locked,
fights the harness's own `admit()`-before-dispatch pipeline), tgbotapi (less
maintained), mime_guess (extension-based; if an outbound fallback is ever
needed, use the maintained `mime-infer` fork), direct rustls for the webhook
listener (TLS terminates upstream).

Hand-rolled: pairing-code primitive, `ChannelAdapter`/`SessionKey` derivation/
`Admission`/`ChannelCapabilities`, MarkdownV2 downgrade + chunking,
`update_id` dedup + poll-offset persistence, approval-callback table with
replay protection, Telegram quota wiring (~1 msg/s/chat, ~30 msg/s global),
webhook secret-header middleware, config cross-validation.

Future channels: Slack → slack-morphism 2.19; Matrix → matrix-rust-sdk
(heavy — prefer matrix-sdk-base/-crypto; needs its own research pass).

## harw-session-store / harw-job-runtime / harw-knowledge

| Need | Choice | Version |
|---|---|---|
| journal persistence | hand-rolled JSONL append + **fs4** (advisory lock) + **tempfile** `persist()` (atomic rename) | 1.1.0 / 3.27.0 |
| embedded-DB fallback | redb — only if the index ever needs transactional multi-writer semantics | 4.1.0 |
| YAML frontmatter | hand-rolled fence split (~20 lines) + **serde_norway** | 0.9.42 |
| markdown parsing | pulldown-cmark (event stream, no AST) | 0.13.4 |
| `[[wikilink]]` extraction | hand-rolled scanner | — |
| bounded search | hand-rolled BM25-lite + **aho-corasick** | 1.1.4 |
| file watching | notify (pre-1.0 — pin exactly) | 9.0.0-rc.4 |
| time | **jiff** (`jiff::Timestamp` in all frontmatter/audit types) | 0.2.32 |
| IDs | uuid `v7` feature (existing dep); ulid optional for human-readable file names | 1.23.5 |
| cron (dream jobs) | **croner** (expression parser only) + hand-rolled `tokio::time::sleep_until` loop | 3.0.1 |
| paths/XDG | etcetera (default workspace-root resolution only) | 0.11.0 |
| content hashing | blake3 (artifact dedup/integrity) | 1.8.5 |

Rejected: sled (maintenance mode, unstable format), fjall (no LSM need yet —
second fallback after redb), rusqlite (contradicts markdown-as-source design;
fallback if multi-process writers materialize), serde_yml (**RUSTSEC-2025-0068**),
serde-yaml-ng (untrustworthy fork), gray_matter (wraps deprecated serde_yaml),
comrak (full mutable AST, too heavy), tantivy (wrong weight class for a
disposable rebuildable index), simsearch (fuzzy matching, not BM25),
atomicwrites (stale — tempfile covers it), fs2 (unmaintained — fs4 is the fork),
chrono (viable but jiff is the better 2026 default; nothing forces chrono),
directories (maintenance concerns — etcetera instead),
croner-scheduler (0.0.x, immature — only the croner parser is used).

## harw-secrets

See [secrets-and-audit.md](secrets-and-audit.md) for the full design.

| Crate | Version | Purpose |
|---|---|---|
| crypt_guard | 2.0.3 | standalone in-house PQC crate (crates.io, MIT): ML-KEM/ML-DSA/SLH-DSA, HKDF, CGv2 envelope, unconditional zeroize |
| keyring | latest at add time | optional OS-keyring KEK provenance |

Rules: crypt_guard is consumed as a **crates.io dependency** (not a path dep)
so Harwness builds standalone. **No direct deps** on ml-kem, ml-dsa, slh-dsa,
hkdf, chacha20poly1305, aes-gcm-siv — they come transitively through
crypt_guard. No dependency on any external reference-only study project;
Harwness's cryptography is self-contained.

Hand-rolled: hash-chained audit log (prev-hash linking + periodically
ML-DSA-signed chain-head checkpoints) — no existing crate or in-house project
provides this.
